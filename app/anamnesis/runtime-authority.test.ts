import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AuthoritySnapshot } from "@anamnesis/core";
import { createRuntimeAuthority, manifestTemplate, objectInventory } from "./runtime-authority.ts";
import { NEO4J_IMAGE, NEO4J_VERSION, OwnedNeo4jAdapter } from "./owned-neo4j-adapter.ts";

const sha256 = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");
const cutoff = { ingest_seq: 7, structure_revision: 3, policy_revision: 2 };
const authority: AuthoritySnapshot = {
  members: [], retained_generations: [], coverage: cutoff, physical_links: [], invalidation_evidence: [], source_hashes: [],
};

async function writeObject(root: string, bytes: Uint8Array, mediaType: string): Promise<string> {
  const hash = sha256(bytes);
  await mkdir(join(root, hash.slice(0, 2)), { recursive: true });
  await writeFile(join(root, hash.slice(0, 2), hash), bytes);
  await writeFile(join(root, hash.slice(0, 2), `${hash}.json`), JSON.stringify({ mediaType }));
  return hash;
}

test("objectInventory lists hash-addressed objects sorted by hash and skips foreign entries", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
  try {
    const a = await writeObject(root, new TextEncoder().encode("alpha"), "text/plain");
    const b = await writeObject(root, new TextEncoder().encode("bravo"), "application/json");
    await mkdir(join(root, "zz-not-a-prefix"));
    await writeFile(join(root, a.slice(0, 2), "notes.txt"), "ignored");
    const inventory = await objectInventory(root);
    // sha256("alpha") starts 8ed3f6ad, sha256("bravo") starts f144a690: alpha sorts first although bravo was written second.
    expect(a.slice(0, 8)).toBe("8ed3f6ad");
    expect(b.slice(0, 8)).toBe("f144a690");
    expect(inventory).toEqual([{ hash: a, size: 5, media_type: "text/plain" }, { hash: b, size: 5, media_type: "application/json" }]);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("objectInventory refuses an object whose bytes do not match its name", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
  try {
    const hash = await writeObject(root, new TextEncoder().encode("original"), "text/plain");
    await writeFile(join(root, hash.slice(0, 2), hash), "tampered");
    await expect(objectInventory(root)).rejects.toThrow("object_corrupt");
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("manifestTemplate pins format, image digest and one data+sidecar member per object, sorted by path", () => {
  const objects = [
    { hash: "f".repeat(64), size: 9, media_type: "text/plain" },
    { hash: "0".repeat(64), size: 4, media_type: "application/json" },
  ];
  const manifest = manifestTemplate("op-1", cutoff, authority, objects, "c".repeat(64));
  expect(manifest.format).toBe("anamnesis.archive/1");
  expect(manifest.operation_id).toBe("op-1");
  expect(manifest.cutoff).toEqual(cutoff);
  expect(manifest.authority).toBe(authority);
  expect(manifest.compatibility.neo4j_version).toBe(NEO4J_VERSION);
  expect(manifest.compatibility.neo4j_image_digest).toBe(NEO4J_IMAGE.slice("neo4j@".length));
  expect(manifest.configuration.config_sha256).toBe("c".repeat(64));
  const zeros = "0".repeat(64), effs = "f".repeat(64);
  expect(manifest.members.map(member => member.path)).toEqual([
    "config.jsonc", "database/neo4j.dump", "database/neo4j.dump.metadata.json", "neo4j.auth",
    `objects/00/${zeros}`, `objects/00/${zeros}.json`, `objects/ff/${effs}`, `objects/ff/${effs}.json`,
  ]);
  expect(manifest.members.map(member => member.role)).toEqual([
    "config", "database_dump", "dump_metadata", "auth", "object_data", "object_sidecar", "object_data", "object_sidecar",
  ]);
  expect(manifest.members.find(member => member.role === "object_data" && member.sha256 === "f".repeat(64))?.bytes).toBe(9);
  expect(manifest.objects).toBe(objects);
});

const AUTHORITY_ENV = ["ANAMNESIS_NEO4J_CONTAINER", "ANAMNESIS_QA_OWNER"] as const;
type AuthorityEnv = typeof AUTHORITY_ENV[number];

async function withAuthorityEnv(values: Partial<Record<AuthorityEnv, string>>, run: () => Promise<void>): Promise<void> {
  const saved = AUTHORITY_ENV.map(name => [name, process.env[name]] as const);
  for (const name of AUTHORITY_ENV) { const value = values[name]; if (value === undefined) delete process.env[name]; else process.env[name] = value; }
  try { await run(); }
  finally { for (const [name, value] of saved) { if (value === undefined) delete process.env[name]; else process.env[name] = value; } }
}

// Neither test reaches a path that reads engine, installation or context: the guard throws first, and the
// construction path only captures them in closures that are never invoked here.
type Args = Parameters<typeof createRuntimeAuthority>;
const unreadArgs: Args = [{} as Args[0], {} as Args[1], {} as Args[2]];

test.each([
  { missing: "ANAMNESIS_NEO4J_CONTAINER", present: "ANAMNESIS_QA_OWNER" },
  { missing: "ANAMNESIS_QA_OWNER", present: "ANAMNESIS_NEO4J_CONTAINER" },
] as const)("createRuntimeAuthority refuses to build when $missing is unset", ({ present }) => withAuthorityEnv({ [present]: "set" }, async () => {
  await expect(createRuntimeAuthority(...unreadArgs)).rejects.toMatchObject({ code: "backup_adapter_unavailable" });
}));

// The four methods invoked below are literal no-ops in createRuntimeAuthority; none of them may ever reach the adapter's docker exec,
// or this pure suite would spawn a real docker process.
test("createRuntimeAuthority returns an owned adapter once both names are set; rebind, verify, quarantine and stop resolve to nothing", () =>
  withAuthorityEnv({ ANAMNESIS_NEO4J_CONTAINER: "anamnesis-qa-neo4j", ANAMNESIS_QA_OWNER: "qa-owner" }, async () => {
    const adapter = await createRuntimeAuthority(...unreadArgs);
    expect(adapter).toBeInstanceOf(OwnedNeo4jAdapter);
    await expect(Promise.all([adapter.rebindSource("src"), adapter.verifyPhysicalLinks("/root"), adapter.quarantine("/root"), adapter.stop()])).resolves.toEqual([undefined, undefined, undefined, undefined]);
  }));
