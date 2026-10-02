import { expect, test } from "bun:test";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine, type AuthoritySnapshot, type InstallationContext } from "@anamnesis/core";
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

const AUTHORITY_ENV = ["ANAMNESIS_NEO4J_CONTAINER", "ANAMNESIS_QA_OWNER", "ANAMNESIS_NEO4J_URI"] as const;
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

const OWNED = {
  uri: process.env["ANAMNESIS_TEST_NEO4J_URI"] ?? "", password: process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"] ?? "",
  container: process.env["ANAMNESIS_NEO4J_CONTAINER"] ?? "", owner: process.env["ANAMNESIS_QA_OWNER"] ?? "",
};
const ownedTest = test.skipIf(Object.values(OWNED).some(value => value === ""));
const dockerOutput = async (args: string[]) => (await promisify(execFile)("docker", args)).stdout.trim();
const installationContext: InstallationContext = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() };

ownedTest("the runtime authority fences the owned container, dumps and reloads it offline, then restarts it on its new port", async () => {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "runtime-authority-"));
  const engine = new Engine({ uri: OWNED.uri, password: OWNED.password, objectsRoot: join(root, "objects") });
  const installation = { root, token: "token", incarnation: uuidv7(), epoch: uuidv7(), assertOwned: async () => {}, release: async () => {} };
  const archive = join(root, "archive"), staging = join(root, "staging"), corrupt = join(root, "corrupt");
  await mkdir(join(archive, "database"), { recursive: true });
  await mkdir(join(corrupt, "database"), { recursive: true });
  await writeFile(join(corrupt, "database", "neo4j.dump"), "not a neo4j dump");
  try {
    await engine.init();
    await engine.claimWriterEpoch();
    const episode = await engine.remember({ content: "fenced before the dump", time: { value: "2026-09-01T00:00:00Z", precision: "day" }, origin: { source: root, session: root, actor: "user", record: "one" }, source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context: installationContext });
    await withAuthorityEnv({ ANAMNESIS_NEO4J_CONTAINER: OWNED.container, ANAMNESIS_QA_OWNER: OWNED.owner, ANAMNESIS_NEO4J_URI: OWNED.uri }, async () => {
      const adapter = await createRuntimeAuthority(engine, installation, installationContext);
      await expect(adapter.authoritySnapshot("1")).rejects.toThrow("writer_fence_required");
      const fenced = await adapter.revokeWriters();
      expect(fenced.epoch).toMatch(/^\d+$/);
      expect(fenced.cutoff).toMatchObject({ ingest_seq: 1, policy_revision: 0 });
      expect(await dockerOutput(["inspect", "--format", "{{.State.Running}}", OWNED.container])).toBe("false");
      const authority = await adapter.authoritySnapshot(fenced.epoch);
      expect(authority.members).toEqual([episode.id]);
      expect(authority.coverage).toEqual(fenced.cutoff);
      const dumpPath = join(archive, "database", "neo4j.dump");
      const dump = await adapter.dumpOffline(dumpPath, fenced.epoch);
      const bytes = (await stat(dumpPath)).size;
      expect(bytes).toBeGreaterThan(0);
      expect(dump).toEqual({ metadata: Buffer.from(JSON.stringify({ format: "anamnesis.adapter-dump/1", epoch: fenced.epoch, bytes }) + "\n"), neo4jVersion: NEO4J_VERSION, imageDigest: NEO4J_IMAGE.slice("neo4j@".length) });
      const manifest = manifestTemplate("op-live", fenced.cutoff, authority, [], "c".repeat(64));
      await adapter.materializeMembers(archive, manifest);
      const config = await readFile(join(archive, "config.jsonc"));
      expect(JSON.parse(config.toString())).toEqual({ uri: OWNED.uri, user: "neo4j", database: "neo4j" });
      expect(JSON.parse(await readFile(join(archive, "neo4j.auth"), "utf8"))).toEqual({ database: "neo4j" });
      expect(manifest.members.find(member => member.role === "config")).toMatchObject({ bytes: config.byteLength, sha256: sha256(config) });
      expect(manifest.members.find(member => member.role === "auth")?.bytes).toBe((await stat(join(archive, "neo4j.auth"))).size);
      await mkdir(join(root, "memberless"));
      await expect(adapter.materializeMembers(join(root, "memberless"), { ...manifest, members: [] })).rejects.toThrow("invalid_manifest_members");
      await adapter.restoreOffline(archive, staging, manifest);
      expect(await readdir(join(staging, "database", "databases"))).toContain("neo4j");
      await expect(adapter.restoreOffline(corrupt, join(root, "corrupt-staging"), manifest)).rejects.toThrow("neo4j_load_failed");
      expect(await adapter.startAndReady(root, fenced.epoch)).toEqual({ sourceId: installation.incarnation, epoch: fenced.epoch, ready: true });
      const restarted = process.env["ANAMNESIS_NEO4J_URI"] ?? "";
      expect(restarted).toMatch(/^bolt:\/\/127\.0\.0\.1:\d+$/);
      expect(await dockerOutput(["inspect", "--format", "{{.State.Running}}", OWNED.container])).toBe("true");
      const driver = neo4j.driver(restarted, neo4j.auth.basic("neo4j", OWNED.password), { disableLosslessIntegers: true });
      try { expect((await driver.executeQuery("MATCH (e:Episode {id:$id}) RETURN count(e) AS n", { id: episode.id })).records[0]!.get("n")).toBe(1); }
      finally { await driver.close(); }
    });
  } finally {
    await engine.close();
    if (await dockerOutput(["inspect", "--format", "{{.State.Running}}", OWNED.container]) !== "true") await dockerOutput(["start", OWNED.container]);
    await rm(root, { recursive: true, force: true });
  }
}, 240_000);
