import { afterEach, expect, test } from "bun:test";
import { lstat, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { backupOwned, restoreOwned } from "./backup-restore-orchestrator.ts";
import { preflightArchive, type ArchiveCompatibility, type ArchiveManifest } from "./archive-manifest.ts";
import { NEO4J_VERSION } from "./owned-neo4j-adapter.ts";
import { manifestTemplate } from "./runtime-authority.ts";
import { FIXTURE_CONFIG, FIXTURE_IMAGE_DIGEST, fakeAuthorityAdapter, fixtureAuthority, fixtureCutoff, noOverrides, sha256, type AdapterOverrides } from "./authority-adapter.fixture.ts";

const OPERATION = "01993000-0000-7000-8000-000000000042";
const SOURCE = "source-incarnation";
const compatibility: ArchiveCompatibility = {
  schema_versions: ["anamnesis.storage/1"], neo4j_versions: [NEO4J_VERSION],
  neo4j_image_digests: [FIXTURE_IMAGE_DIGEST], episode_digest_version_ceiling: 2,
};

function manifest(): ArchiveManifest {
  return manifestTemplate(OPERATION, { ...fixtureCutoff }, fixtureAuthority(), [], sha256(FIXTURE_CONFIG));
}
const fakeAdapter = (overrides: AdapterOverrides = noOverrides) => fakeAuthorityAdapter(SOURCE, { overrides });

const parents: string[] = [];
afterEach(async () => { for (const parent of parents.splice(0)) await rm(parent, { recursive: true, force: true }); });

async function workspace() {
  const parent = await mkdtemp(join(tmpdir(), "anamnesis-orchestrator-"));
  parents.push(parent);
  const root = join(parent, "source");
  await mkdir(root, { mode: 0o700 });
  return { parent, root, destination: join(parent, "archive") };
}

type BackupInput = Parameters<typeof backupOwned>[0];
type Patch = (input: BackupInput) => void | Promise<void>;
async function backup(overrides: AdapterOverrides = noOverrides, patch: Patch = () => {}) {
  const { parent, root, destination } = await workspace();
  const { adapter, calls } = fakeAdapter(overrides);
  const input: BackupInput = { root, destination, operationId: OPERATION, compatibility, manifest: manifest() };
  await patch(input);
  return { parent, root, destination, calls, result: backupOwned(input, adapter) };
}

test("backupOwned publishes a complete archive and resumes the writer afterwards", async () => {
  const { destination, calls, result } = await backup();
  const produced = await result;
  expect(calls).toEqual(["revokeWriters", "authoritySnapshot", "dumpOffline", "materializeMembers", "startAndReady"]);
  expect(produced.members.find(m => m.role === "database_dump")?.sha256).toBe(sha256("neo4j dump"));
  const admitted = await preflightArchive(destination, compatibility);
  expect(admitted.manifest.operation_id).toBe(OPERATION);
  expect(admitted.manifest.members.map(m => [m.path, m.sha256])).toEqual(produced.members.map(m => [m.path, m.sha256]));
  for (const member of produced.members) expect(sha256(await readFile(join(destination, member.path)))).toBe(member.sha256);
  await expect(lstat(`${destination}.${OPERATION}.partial`)).rejects.toMatchObject({ code: "ENOENT" });
});

test("backupOwned refuses mismatched identity, stale cutoff, and changed or absent authority before dumping", async () => {
  const cases: [string, AdapterOverrides, Patch][] = [
    ["identity_conflict", noOverrides, input => { input.operationId = "01993000-0000-7000-8000-000000000099"; }],
    ["stale_epoch", () => ({ revokeWriters: async () => ({ epoch: "1", cutoff: { ...fixtureCutoff, ingest_seq: 8 } }) }), () => {}],
    ["authority_snapshot_unavailable", noOverrides, input => { delete input.manifest.authority; }],
    ["authority_snapshot_changed", () => ({ authoritySnapshot: async () => ({ ...fixtureAuthority(), members: ["episode-2"] }) }), () => {}],
  ];
  for (const [code, overrides, patch] of cases) {
    const { calls, result } = await backup(overrides, patch);
    await expect(result).rejects.toMatchObject({ code });
    expect(calls).not.toContain("dumpOffline");
  }
});

test("backupOwned refuses overlapping roots and an existing destination", async () => {
  const { result } = await backup(noOverrides, input => { input.destination = join(input.root, "nested"); });
  await expect(result).rejects.toMatchObject({ code: "unsafe_path" });
  const { result: second } = await backup(noOverrides, async input => { await writeFile(input.destination, "occupied"); });
  await expect(second).rejects.toMatchObject({ code: "destination_exists" });
});

async function objectStore(parent: string, payloads: string[]) {
  const objectRoot = join(parent, "objects");
  const objects: ArchiveManifest["objects"] = [];
  for (const text of payloads) {
    const bytes = Buffer.from(text), hash = sha256(bytes);
    await mkdir(join(objectRoot, hash.slice(0, 2)), { recursive: true, mode: 0o700 });
    await writeFile(join(objectRoot, hash.slice(0, 2), hash), bytes, { mode: 0o600 });
    await writeFile(join(objectRoot, hash.slice(0, 2), `${hash}.json`), JSON.stringify({ hash, size: bytes.length, mediaType: "text/plain" }), { mode: 0o600 });
    objects.push({ hash, size: bytes.length, media_type: "text/plain" });
  }
  return { objectRoot, objects: objects.sort((a, b) => a.hash.localeCompare(b.hash)) };
}

test("backupOwned copies the object store and hashes every sidecar into the manifest", async () => {
  const { destination, result } = await backup(noOverrides, async input => {
    const { objectRoot, objects } = await objectStore(join(input.root, ".."), ["one", "two"]);
    input.objectRoot = objectRoot;
    input.manifest = manifestTemplate(OPERATION, { ...fixtureCutoff }, fixtureAuthority(), objects, sha256(FIXTURE_CONFIG));
  });
  const produced = await result;
  const sidecars = produced.members.filter(m => m.role === "object_sidecar");
  expect(sidecars).toHaveLength(2);
  for (const member of sidecars) expect(sha256(await readFile(join(destination, member.path)))).toBe(member.sha256);
  expect((await preflightArchive(destination, compatibility)).manifest.objects.map(o => o.hash)).toEqual(produced.objects.map(o => o.hash));
});

test("backupOwned refuses an object store whose inventory differs from the manifest", async () => {
  const { destination, result } = await backup(noOverrides, async input => {
    const { objectRoot } = await objectStore(join(input.root, ".."), ["one"]);
    input.objectRoot = objectRoot;
  });
  await expect(result).rejects.toMatchObject({ code: "object_inventory_changed" });
  await expect(lstat(`${destination}.${OPERATION}.partial`)).rejects.toMatchObject({ code: "ENOENT" });
});

test("backupOwned removes the partial directory when the dump fails", async () => {
  const { destination, calls, result } = await backup(record => ({ dumpOffline: async () => { record("dumpOffline"); throw new Error("dump_exploded"); } }));
  await expect(result).rejects.toThrow("dump_exploded");
  expect(calls).not.toContain("startAndReady");
  await expect(lstat(`${destination}.${OPERATION}.partial`)).rejects.toMatchObject({ code: "ENOENT" });
  await expect(lstat(destination)).rejects.toMatchObject({ code: "ENOENT" });
});

async function archiveFromBackup() {
  const { parent, destination, result } = await backup();
  await result;
  const live = join(parent, "live");
  await mkdir(live, { mode: 0o700 });
  await writeFile(join(live, "sentinel"), "old live data");
  return { archive: destination, live, staging: join(parent, "staging"), rollback: join(parent, "rollback") };
}

test("restoreOwned swaps the admitted archive into place behind the adapter and drops the rollback copy", async () => {
  const { archive, live, staging, rollback } = await archiveFromBackup();
  const { adapter, calls } = fakeAdapter();
  const admitted = await restoreOwned({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback, operationId: OPERATION, compatibility, expectedSourceId: SOURCE }, adapter);
  expect(admitted.manifest.operation_id).toBe(OPERATION);
  expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startAndReady", "rebindSource"]);
  await expect(lstat(join(live, "sentinel"))).rejects.toMatchObject({ code: "ENOENT" });
  await expect(lstat(rollback)).rejects.toMatchObject({ code: "ENOENT" });
  await expect(lstat(staging)).rejects.toMatchObject({ code: "ENOENT" });
});

test("restoreOwned quarantines staging when the restarted source does not match", async () => {
  const { archive, live, staging, rollback } = await archiveFromBackup();
  const { adapter, calls } = fakeAdapter(record => ({ startAndReady: async (_root, epoch) => { record("startAndReady"); return { sourceId: "impostor", epoch, ready: true }; } }));
  await expect(restoreOwned({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback, operationId: OPERATION, compatibility, expectedSourceId: SOURCE }, adapter))
    .rejects.toMatchObject({ code: "source_rebind_mismatch" });
  expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startAndReady", `quarantine:${staging}`]);
  expect((await readFile(join(rollback, "sentinel"), "utf8"))).toBe("old live data");
  await expect(lstat(join(live, "sentinel"))).rejects.toMatchObject({ code: "ENOENT" });
});

test("restoreOwned refuses an archive whose operation id differs before touching the adapter", async () => {
  const { archive, live, staging, rollback } = await archiveFromBackup();
  const { adapter, calls } = fakeAdapter();
  await expect(restoreOwned({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback, operationId: "01993000-0000-7000-8000-000000000099", compatibility, expectedSourceId: SOURCE }, adapter))
    .rejects.toMatchObject({ code: "identity_conflict" });
  expect(calls).toEqual([]);
  await expect(lstat(staging)).rejects.toMatchObject({ code: "ENOENT" });
});
