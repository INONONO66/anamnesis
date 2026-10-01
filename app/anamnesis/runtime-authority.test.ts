import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AuthoritySnapshot } from "@anamnesis/core";
import { createRuntimeAuthority, manifestTemplate, objectInventory } from "./runtime-authority.ts";
import { NEO4J_IMAGE, NEO4J_VERSION } from "./owned-neo4j-adapter.ts";

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

test.each([
  { missing: "ANAMNESIS_NEO4J_CONTAINER", present: "ANAMNESIS_QA_OWNER" },
  { missing: "ANAMNESIS_QA_OWNER", present: "ANAMNESIS_NEO4J_CONTAINER" },
])("createRuntimeAuthority refuses to build when $missing is unset", async ({ missing, present }) => {
  const saved = { container: process.env["ANAMNESIS_NEO4J_CONTAINER"], owner: process.env["ANAMNESIS_QA_OWNER"] };
  delete process.env[missing];
  process.env[present] = "set";
  // The guard runs before any argument is read; the typed empty objects are never dereferenced.
  type Args = Parameters<typeof createRuntimeAuthority>;
  const [engine, installation, context]: Args = [{} as Args[0], {} as Args[1], {} as Args[2]];
  try {
    await expect(createRuntimeAuthority(engine, installation, context)).rejects.toMatchObject({ code: "backup_adapter_unavailable" });
  } finally {
    if (saved.container === undefined) delete process.env["ANAMNESIS_NEO4J_CONTAINER"]; else process.env["ANAMNESIS_NEO4J_CONTAINER"] = saved.container;
    if (saved.owner === undefined) delete process.env["ANAMNESIS_QA_OWNER"]; else process.env["ANAMNESIS_QA_OWNER"] = saved.owner;
  }
});
