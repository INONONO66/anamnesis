import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { backupOwned } from "./backup-restore-orchestrator.ts";
import { fixture } from "./archive-manifest.fixture.mjs";

test("backup publishes an object store at the archive object paths", async t => {
  const f = await fixture(t);
  const parent = await mkdtemp("/tmp/ana-backup-object-");
  t.after(async () => {
    await import("node:fs/promises").then(({ rm }) => rm(parent, { recursive: true, force: true }));
  });
  const root = join(parent, "source"), destination = join(parent, "archive");
  await mkdir(root, { mode: 0o700 });
  const authority = {
    members: ["episode-1"], retained_generations: [1],
    coverage: { ingest_seq: 1, structure_revision: 1, policy_revision: 1 },
    physical_links: [], invalidation_evidence: [], source_hashes: ["a".repeat(64)],
  };
  const manifest = { ...f.manifest, authority };
  const adapter = {
    revokeWriters: async () => ({ epoch: "1", cutoff: manifest.cutoff }),
    authoritySnapshot: async () => authority,
    dumpOffline: async (path) => {
      await writeFile(path, Buffer.from("neo4j dump"), { mode: 0o600 });
      return { metadata: new Uint8Array(), neo4jVersion: "5.26.12", imageDigest: "sha256:" + "a".repeat(64) };
    },
    materializeMembers: async () => {},
    startAndReady: async () => ({ sourceId: "source", epoch: "1", ready: true }),
    stop: async () => {}, restoreOffline: async () => {}, rebindSource: async () => {},
    verifyPhysicalLinks: async () => {}, quarantine: async () => {},
  };
  await backupOwned({ root, destination, operationId: manifest.operation_id,
    compatibility: { schema_versions: ["anamnesis.storage/1"], neo4j_versions: ["5.26.12"], neo4j_image_digests: ["sha256:" + "a".repeat(64)], episode_digest_version_ceiling: 2 },
    manifest, objectRoot: join(f.root, "objects") }, adapter);
  const objectPath = join(destination, "objects", f.dataPath.slice("objects/".length));
  assert.equal((await readFile(objectPath)).toString(), "owned payload bytes\n");
  assert.equal(JSON.parse(await readFile(`${objectPath}.json`, "utf8")).hash, f.manifest.objects[0].hash);
  await stat(join(destination, "manifest.json"));
});
