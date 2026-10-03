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
import { createRuntimeAuthority, fencedAdapter, manifestTemplate, objectInventory, readRestoredAuthority } from "./runtime-authority.ts";
import { backupOwned, restoreOwned } from "./backup-restore-orchestrator.ts";
import { NEO4J_IMAGE, NEO4J_VERSION, OwnedNeo4jAdapter } from "./owned-neo4j-adapter.ts";
import { fixtureAuthority } from "./authority-adapter.fixture.ts";

const sha256 = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");
const cutoff = { ingest_seq: 7, structure_revision: 3, policy_revision: 2 };
const authority: AuthoritySnapshot = {
  ...fixtureAuthority(), retained_generations: [], coverage: cutoff,
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

const AUTHORITY_ENV = ["ANAMNESIS_NEO4J_CONTAINER", "ANAMNESIS_QA_OWNER", "ANAMNESIS_NEO4J_URI", "ANAMNESIS_NEO4J_PASSWORD"] as const;
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
/** A test that fences the owned container restarts it on a fresh ephemeral port; the env URI from file start is then stale. */
const ownedUri = async () => `bolt://127.0.0.1:${(await dockerOutput(["port", OWNED.container, "7687/tcp"])).split(":").at(-1)}`;
const restoredBinding = async (root: string) => JSON.parse(await readFile(join(root, "authority.json"), "utf8")) as { container: string; uri: string; owner: string };
const count = async (driver: neo4j.Driver, cypher: string, params: Record<string, unknown> = {}) => (await driver.executeQuery(cypher, params)).records[0]!.get("n") as number;
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
  let restoredContainer: string | undefined;
  try {
    await engine.init();
    await engine.claimWriterEpoch();
    const episode = await engine.remember({ content: "fenced before the dump", time: { value: "2026-09-01T00:00:00Z", precision: "day" }, origin: { source: root, session: root, actor: "user", record: "one" }, source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context: installationContext });
    await withAuthorityEnv({ ANAMNESIS_NEO4J_CONTAINER: OWNED.container, ANAMNESIS_QA_OWNER: OWNED.owner, ANAMNESIS_NEO4J_URI: OWNED.uri, ANAMNESIS_NEO4J_PASSWORD: OWNED.password }, async () => {
      const adapter = await createRuntimeAuthority(engine, installation, installationContext);
      await expect(adapter.authoritySnapshot("1")).rejects.toThrow("writer_fence_required");
      await expect(adapter.restoredAuthoritySnapshot()).rejects.toThrow("restore_not_started");
      const fenced = await adapter.revokeWriters();
      expect(fenced.epoch).toMatch(/^\d+$/);
      expect(fenced.cutoff).toMatchObject({ ingest_seq: 1, policy_revision: 0 });
      expect(await dockerOutput(["inspect", "--format", "{{.State.Running}}", OWNED.container])).toBe("false");
      const authority = await adapter.authoritySnapshot(fenced.epoch);
      expect(authority.members).toEqual({ count: 1, sha256: sha256(JSON.stringify([episode.id])) });
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
      // Restarting the fenced source is not a restore: nothing restored exists to read yet.
      await expect(adapter.restoredAuthoritySnapshot()).rejects.toThrow("restore_not_started");
      const source = neo4j.driver(restarted, neo4j.auth.basic("neo4j", OWNED.password), { disableLosslessIntegers: true });
      try {
        expect(await count(source, "MATCH (e:Episode {id:$id}) RETURN count(e) AS n", { id: episode.id })).toBe(1);
        // Diverge the source after the dump so the restored database is distinguishable from it.
        await source.executeQuery("MATCH (e:Episode {id:$id}) SET e.id=$changed", { id: episode.id, changed: uuidv7() });
        expect(await adapter.startRestored(staging, fenced.epoch)).toEqual({ sourceId: installation.incarnation, epoch: fenced.epoch, ready: true });
        const bound = await restoredBinding(staging);
        restoredContainer = bound.container;
        expect(bound.container).not.toBe(OWNED.container);
        expect(bound.uri).not.toBe(restarted);
        expect(bound.owner).toBe(OWNED.owner);
        expect(await dockerOutput(["inspect", "--format", '{{index .Config.Labels "anamnesis.qa.owner"}}', bound.container])).toBe(OWNED.owner);
        // The restored snapshot is the dump-time authority, not the diverged source.
        expect(await adapter.restoredAuthoritySnapshot()).toEqual(authority);
        const restored = neo4j.driver(bound.uri, neo4j.auth.basic("neo4j", OWNED.password), { disableLosslessIntegers: true });
        try { expect(await count(restored, "MATCH (e:Episode {id:$id}) RETURN count(e) AS n", { id: episode.id })).toBe(1); }
        finally { await restored.close(); }
        expect(await count(source, "MATCH (e:Episode {id:$id}) RETURN count(e) AS n", { id: episode.id })).toBe(0);
        // Quarantine removes the container that served the rejected tree and forgets the binding.
        await adapter.quarantine(staging);
        await expect(dockerOutput(["inspect", "--format", "{{.State.Running}}", bound.container])).rejects.toThrow(/no such (object|container)/i);
        await expect(adapter.restoredAuthoritySnapshot()).rejects.toThrow("restore_not_started");
        restoredContainer = undefined;
      }
      finally { await source.close(); }
    });
  } finally {
    try {
      await engine.close();
      await removeOwnedContainers(restoredContainer);
      if (await dockerOutput(["inspect", "--format", "{{.State.Running}}", OWNED.container]) !== "true") await dockerOutput(["start", OWNED.container]);
    } finally { await rm(root, { recursive: true, force: true }); }
  }
}, 240_000);

/** Removes every container this test started, including one a failed restore left running before its binding was read. */
async function removeOwnedContainers(known: string | undefined) {
  const shared = await dockerOutput(["inspect", "--format", "{{.Id}}", OWNED.container]);
  const started = (await dockerOutput(["ps", "-aq", "--no-trunc", "--filter", `label=anamnesis.qa.owner=${OWNED.owner}`])).split("\n").filter(id => id && id !== shared);
  for (const container of new Set([...(known ? [known] : []), ...started])) await dockerOutput(["rm", "-f", container]).catch(() => undefined);
}

// #229 acceptance: 100k members through the real archive and restore boundary, no sleeps. The source is diverged after
// the archive completes, so the digest comparison inside restoreOwned can only pass against the database the restore loaded.
ownedTest("100k Elements round-trip through the archive; the restored database, not the diverged source, reproduces the manifest digests", async () => {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const base = await mkdtemp(join(parent, "runtime-round-trip-"));
  const live = join(base, "live"), destination = join(base, "archive");
  await mkdir(join(live, "objects"), { recursive: true, mode: 0o700 });
  const operationId = uuidv7(), size = 100_000, page = 5_000;
  const staging = `${live}.restore-staging.${operationId}`, rollback = `${live}.restore-rollback.${operationId}`;
  const installation = { root: live, token: "token", incarnation: uuidv7(), epoch: uuidv7(), assertOwned: async () => {}, release: async () => {} };
  // UUIDv7-shaped member IDs that share one prefix, so the fixture is both admissible and removable in one pass.
  const prefix = installation.incarnation.slice(0, 31);
  const memberId = (n: number) => `${prefix}${n.toString(16).padStart(5, "0")}`;
  const compatibility = { schema_versions: ["anamnesis.storage/1"], neo4j_versions: [NEO4J_VERSION], neo4j_image_digests: [NEO4J_IMAGE.slice("neo4j@".length)], episode_digest_version_ceiling: 2 as const };
  const sourceUri = await ownedUri();
  const engine = new Engine({ uri: sourceUri, password: OWNED.password, objectsRoot: join(live, "objects") });
  const seed = neo4j.driver(sourceUri, neo4j.auth.basic("neo4j", OWNED.password), { disableLosslessIntegers: true });
  let restoredContainer: string | undefined;
  try {
    await engine.init();
    await engine.claimWriterEpoch();
    // The owned container is shared within this file: the previous test leaves its Episode behind.
    const baseElements = await count(seed, "MATCH (e:Element) RETURN count(e) AS n");
    const baseEpisodes = await count(seed, "MATCH (e:Episode) RETURN count(e) AS n");
    const episode = await engine.remember({ content: "archived with one hundred thousand members", time: { value: "2026-09-01T00:00:00Z", precision: "day" }, origin: { source: live, session: live, actor: "user", record: "one" }, source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context: installationContext });
    for (let first = 1; first <= size; first += page) {
      const ids = Array.from({ length: Math.min(page, size - first + 1) }, (_, offset) => memberId(first + offset));
      await seed.executeQuery("UNWIND $ids AS id CREATE (:Element {id:id})", { ids });
    }
    await seed.close();
    await withAuthorityEnv({ ANAMNESIS_NEO4J_CONTAINER: OWNED.container, ANAMNESIS_QA_OWNER: OWNED.owner, ANAMNESIS_NEO4J_URI: sourceUri, ANAMNESIS_NEO4J_PASSWORD: OWNED.password }, async () => {
      const adapter = await createRuntimeAuthority(engine, installation, installationContext);
      const fenced = await adapter.revokeWriters();
      const authority = await adapter.authoritySnapshot(fenced.epoch);
      expect(authority.members.count).toBe(baseElements + size + 1);
      expect(authority.source_hashes.count).toBe(baseEpisodes + 1);
      // The archive preflight pins the config member to this fingerprint; materializeMembers writes exactly these bytes.
      const configSha256 = sha256(JSON.stringify({ uri: sourceUri, user: "neo4j", database: "neo4j" }));
      const manifest = manifestTemplate(operationId, fenced.cutoff, authority, await objectInventory(join(live, "objects")), configSha256);
      const written = await backupOwned({ root: live, destination, operationId, compatibility, manifest, objectRoot: join(live, "objects") }, fencedAdapter(adapter, fenced, authority));
      expect(written.authority).toEqual(authority);
      expect(JSON.parse(await readFile(join(destination, "manifest.json"), "utf8")).authority).toEqual(authority);
      // backupOwned restarted the fenced source on a new port; diverge it so the restore can only verify against the loaded database.
      const source = neo4j.driver(process.env["ANAMNESIS_NEO4J_URI"] ?? "", neo4j.auth.basic("neo4j", OWNED.password), { disableLosslessIntegers: true });
      try {
        await source.executeQuery("CREATE (:Element {id:$id})", { id: memberId(0) });
        expect(await count(source, "MATCH (e:Element) WHERE e.id STARTS WITH $prefix RETURN count(e) AS n", { prefix })).toBe(size + 1);
        const result = await restoreOwned({ archive: destination, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback, operationId, compatibility, expectedSourceId: installation.incarnation }, adapter);
        expect(result.manifest.authority).toEqual(authority);
        const bound = await restoredBinding(live);
        restoredContainer = bound.container;
        expect(bound.container).not.toBe(OWNED.container);
        const restored = neo4j.driver(bound.uri, neo4j.auth.basic("neo4j", OWNED.password), { disableLosslessIntegers: true });
        try {
          expect(await count(restored, "MATCH (e:Element) RETURN count(e) AS n")).toBe(baseElements + size + 1);
          expect(await count(restored, "MATCH (e:Element {id:$id}) RETURN count(e) AS n", { id: memberId(0) })).toBe(0);
          expect(await count(restored, "MATCH (e:Episode {id:$id}) RETURN count(e) AS n", { id: episode.id })).toBe(1);
        } finally { await restored.close(); }
        expect(await readRestoredAuthority({ uri: bound.uri, user: "neo4j", password: OWNED.password }, installationContext)).toEqual(authority);
        await expect(stat(staging)).rejects.toMatchObject({ code: "ENOENT" });
        await expect(stat(rollback)).rejects.toMatchObject({ code: "ENOENT" });
      } finally {
        // Remove the fixture from the shared owned source on every path; the Episode is left as the other owned test leaves its own.
        try {
          const session = source.session();
          try { await session.run("MATCH (e:Element) WHERE e.id STARTS WITH $prefix CALL { WITH e DETACH DELETE e } IN TRANSACTIONS OF 5000 ROWS", { prefix }); }
          finally { await session.close(); }
          expect(await count(source, "MATCH (e:Element) WHERE e.id STARTS WITH $prefix RETURN count(e) AS n", { prefix })).toBe(0);
        } finally { await source.close(); }
      }
    });
  } finally {
    try {
      await engine.close();
      await removeOwnedContainers(restoredContainer);
      if (await dockerOutput(["inspect", "--format", "{{.State.Running}}", OWNED.container]) !== "true") await dockerOutput(["start", OWNED.container]);
    } finally { await rm(base, { recursive: true, force: true }); }
  }
}, 600_000);
