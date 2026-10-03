import { afterEach, expect, spyOn, test } from "bun:test";
import type { BigIntStats, PathLike, StatOptions, Stats } from "node:fs";
import * as fsp from "node:fs/promises";
import { chmod, link, lstat, mkdir, mkdtemp, readFile, rm, symlink, truncate, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { backupOwned, restoreOwned } from "./backup-restore-orchestrator.ts";
import { ARCHIVE_LIMITS, preflightArchive, type ArchiveCompatibility, type ArchiveManifest } from "./archive-manifest.ts";
import { NEO4J_VERSION } from "./owned-neo4j-adapter.ts";
import { manifestTemplate } from "./runtime-authority.ts";
import { FIXTURE_CONFIG, FIXTURE_IMAGE_DIGEST, fakeAuthorityAdapter, fixtureAuthority, fixtureCutoff, noOverrides, sha256, type AdapterOverrides } from "./authority-adapter.fixture.ts";

const OPERATION = "01993000-0000-7000-8000-000000000042";
const SOURCE = "source-incarnation";
const compatibility: ArchiveCompatibility = {
  schema_versions: ["anamnesis.storage/1"], neo4j_versions: [NEO4J_VERSION],
  neo4j_image_digests: [FIXTURE_IMAGE_DIGEST], episode_digest_version_ceiling: 2,
};
const failure = (code: string, detail: string) => ({ code, message: `${code}: ${detail}` });

function manifest(objects: ArchiveManifest["objects"] = []): ArchiveManifest {
  return manifestTemplate(OPERATION, { ...fixtureCutoff }, fixtureAuthority(), objects, sha256(FIXTURE_CONFIG));
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
type Patch = (input: BackupInput, parent: string) => void | Promise<void>;
async function backup(overrides: AdapterOverrides = noOverrides, patch: Patch = () => {}) {
  const { parent, root, destination } = await workspace();
  const { adapter, calls } = fakeAdapter(overrides);
  const input: BackupInput = { root, destination, operationId: OPERATION, compatibility, manifest: manifest() };
  await patch(input, parent);
  return { parent, root, destination, calls, result: backupOwned(input, adapter) };
}
const mode = async (path: string) => Number((await lstat(path)).mode & 0o777);
const partialGone = (destination: string) => expect(lstat(`${destination}.${OPERATION}.partial`)).rejects.toMatchObject({ code: "ENOENT" });

test("backupOwned publishes a complete, private archive and resumes the writer afterwards", async () => {
  const { destination, calls, result } = await backup();
  const produced = await result;
  expect(calls).toEqual(["revokeWriters", "authoritySnapshot", "dumpOffline", "materializeMembers", "startAndReady"]);
  expect(produced.members.find(m => m.role === "database_dump")?.sha256).toBe(sha256("neo4j dump"));
  const admitted = await preflightArchive(destination, compatibility);
  expect(admitted.manifest.operation_id).toBe(OPERATION);
  expect(admitted.manifest.members.map(m => [m.path, m.sha256])).toEqual(produced.members.map(m => [m.path, m.sha256]));
  for (const member of produced.members) expect(sha256(await readFile(join(destination, member.path)))).toBe(member.sha256);
  expect(await Promise.all([destination, join(destination, "database")].map(mode))).toEqual([0o700, 0o700]);
  expect(await Promise.all(["manifest.json", "backup.complete", "database/neo4j.dump.metadata.json"].map(p => mode(join(destination, p))))).toEqual([0o600, 0o600, 0o600]);
  await partialGone(destination);
});

test("backupOwned refuses mismatched identity, stale cutoff, and changed or absent authority before dumping", async () => {
  const cases: [string, string, AdapterOverrides, Patch][] = [
    ["identity_conflict", "manifest operation mismatch", noOverrides, input => { input.operationId = "01993000-0000-7000-8000-000000000099"; }],
    ["stale_epoch", "cutoff changed before dump", () => ({ revokeWriters: async () => ({ epoch: "1", cutoff: { ...fixtureCutoff, ingest_seq: 8 } }) }), () => {}],
    ["authority_snapshot_unavailable", "manifest authority evidence is absent", noOverrides, input => { delete input.manifest.authority; }],
    ["authority_snapshot_changed", "authority snapshot does not match cutoff manifest", () => ({ authoritySnapshot: async () => ({ ...fixtureAuthority(), members: { count: 1, sha256: sha256('["episode-2"]') } }) }), () => {}],
    ["object_limit", "manifest exceeds the archive object limits", noOverrides, input => { input.manifest.objects = Array.from({ length: ARCHIVE_LIMITS.objects + 1 }, (_, i) => ({ hash: i.toString(16).padStart(64, "0"), size: 1, media_type: "text/plain" })); }],
    ["object_limit", "manifest exceeds the archive object limits", noOverrides, input => { input.manifest.objects = [{ hash: ONE, size: ARCHIVE_LIMITS.total_member_bytes + 1, media_type: "text/plain" }]; }],
  ];
  for (const [code, detail, overrides, patch] of cases) {
    const { calls, result } = await backup(overrides, patch);
    await expect(result).rejects.toMatchObject(failure(code, detail));
    expect(calls).not.toContain("dumpOffline");
  }
});

test("backupOwned refuses a manifest that lacks the dump or dump metadata member", async () => {
  for (const [role, detail] of [["database_dump", "manifest has no database dump member"], ["dump_metadata", "manifest has no dump metadata member"]] as const) {
    const { destination, result } = await backup(noOverrides, input => { input.manifest.members = input.manifest.members.filter(m => m.role !== role); });
    await expect(result).rejects.toMatchObject(failure("invalid_dump", detail));
    await partialGone(destination);
  }
});

test("backupOwned refuses roots that are not owned private directories", async () => {
  const cases: [string, Patch][] = [
    ["a regular file", async input => { await rm(input.root, { recursive: true }); await writeFile(input.root, "file"); }],
    ["a symlink to a directory", async (input, parent) => { await rm(input.root, { recursive: true }); await symlink(parent, input.root); }],
    ["a group-writable directory", async input => { await chmod(input.root, 0o720); }],
  ];
  for (const [, patch] of cases) {
    const { calls, result } = await backup(noOverrides, patch);
    await expect(result).rejects.toMatchObject(failure("unsafe_path", "owned private directory required"));
    expect(calls).toEqual([]);
  }
});

test.skipIf(process.getuid?.() === 0)("backupOwned refuses a private directory owned by another user", async () => {
  const { result } = await backup(noOverrides, input => { input.root = "/usr"; });
  await expect(result).rejects.toMatchObject(failure("unsafe_path", "owned private directory required"));
});

test("backupOwned refuses a nested destination and an existing one, but accepts a sibling sharing the root's prefix", async () => {
  const { result } = await backup(noOverrides, input => { input.destination = join(input.root, "nested"); });
  await expect(result).rejects.toMatchObject(failure("unsafe_path", "overlapping roots"));
  const { result: second } = await backup(noOverrides, async input => { await writeFile(input.destination, "occupied"); });
  await expect(second).rejects.toMatchObject({ code: "destination_exists" });
  const { result: sibling } = await backup(noOverrides, input => { input.destination = `${input.root}-archive`; });
  expect((await sibling).operation_id).toBe(OPERATION);
});

test("backupOwned surfaces a boot failure after the archive is published and leaves the archive complete", async () => {
  const { destination, result } = await backup(record => ({ startAndReady: async () => { record("startAndReady"); throw new Error("boot_failed"); } }));
  await expect(result).rejects.toThrow("boot_failed");
  expect((await preflightArchive(destination, compatibility)).manifest.operation_id).toBe(OPERATION);
  await partialGone(destination);
});

const SIDECAR = (hash: string, size: number, mediaType: unknown = "text/plain") => JSON.stringify({ hash, size, mediaType });
async function putObject(objectRoot: string, prefix: string, name: string, bytes: Uint8Array, sidecar: string | null = SIDECAR(name, bytes.byteLength)) {
  await mkdir(join(objectRoot, prefix), { recursive: true, mode: 0o700 });
  await writeFile(join(objectRoot, prefix, name), bytes, { mode: 0o600 });
  if (sidecar !== null) await writeFile(join(objectRoot, prefix, `${name}.json`), sidecar, { mode: 0o600 });
  return join(objectRoot, prefix, name);
}
async function objectStore(parent: string, payloads: string[]) {
  const objectRoot = join(parent, "objects");
  await mkdir(objectRoot, { mode: 0o700 });
  const objects: ArchiveManifest["objects"] = [];
  for (const text of payloads) {
    const bytes = Buffer.from(text), hash = sha256(bytes);
    await putObject(objectRoot, hash.slice(0, 2), hash, bytes);
    objects.push({ hash, size: bytes.length, media_type: "text/plain" });
  }
  return { objectRoot, objects: objects.sort((a, b) => a.hash.localeCompare(b.hash)) };
}
const withObjects = (payloads: string[]): Patch => async (input, parent) => {
  const { objectRoot, objects } = await objectStore(parent, payloads);
  input.objectRoot = objectRoot;
  input.manifest = manifest(objects);
};

test("backupOwned copies the object store, including two objects sharing a prefix, and hashes every sidecar into the manifest", async () => {
  const payloads = ["one", "two", "object-7", "object-10"];
  expect(new Set(payloads.map(p => sha256(p).slice(0, 2))).size).toBe(3);
  const { destination, result } = await backup(noOverrides, withObjects(payloads));
  const produced = await result;
  const sidecars = produced.members.filter(m => m.role === "object_sidecar");
  expect(sidecars).toHaveLength(4);
  for (const member of sidecars) expect(sha256(await readFile(join(destination, member.path)))).toBe(member.sha256);
  const admitted = await preflightArchive(destination, compatibility);
  expect(admitted.manifest.objects.map(o => o.hash)).toEqual(payloads.map(p => sha256(p)).sort());
  for (const { hash, size } of admitted.manifest.objects) {
    const data = join(destination, "objects", hash.slice(0, 2), hash);
    expect(await Promise.all([join(destination, "objects"), join(destination, "objects", hash.slice(0, 2)), data, `${data}.json`].map(mode))).toEqual([0o700, 0o700, 0o600, 0o600]);
    expect((await readFile(data)).byteLength).toBe(size);
    await expect(lstat(`${data}.tmp`)).rejects.toMatchObject({ code: "ENOENT" });
  }
});

test("backupOwned omits the objects directory when the object store is empty", async () => {
  const { destination, result } = await backup(noOverrides, withObjects([]));
  expect((await result).objects).toEqual([]);
  await expect(lstat(join(destination, "objects"))).rejects.toMatchObject({ code: "ENOENT" });
  expect((await preflightArchive(destination, compatibility)).manifest.objects).toEqual([]);
});

test("backupOwned admits a manifest exactly at the object count and total byte limits", async () => {
  const staleCutoff: AdapterOverrides = () => ({ revokeWriters: async () => ({ epoch: "1", cutoff: { ...fixtureCutoff, ingest_seq: 8 } }) });
  const boundaries: ArchiveManifest["objects"][] = [
    Array.from({ length: ARCHIVE_LIMITS.objects }, (_, i) => ({ hash: i.toString(16).padStart(64, "0"), size: 1, media_type: "text/plain" })),
    [{ hash: ONE, size: ARCHIVE_LIMITS.total_member_bytes, media_type: "text/plain" }],
  ];
  for (const objects of boundaries) {
    const { result } = await backup(staleCutoff, input => { input.manifest = manifest(objects); });
    await expect(result).rejects.toMatchObject(failure("stale_epoch", "cutoff changed before dump"));
  }
});

test("backupOwned refuses an object store whose inventory differs from the manifest", async () => {
  const { destination, result } = await backup(noOverrides, async (input, parent) => {
    input.objectRoot = (await objectStore(parent, ["one"])).objectRoot;
    input.manifest = manifest([{ hash: sha256("two"), size: 3, media_type: "text/plain" }]);
  });
  await expect(result).rejects.toMatchObject(failure("object_inventory_changed", "ObjectStore inventory does not match the authority manifest"));
  await partialGone(destination);
});

test("backupOwned stops copying as soon as the object store holds more objects or bytes than the manifest", async () => {
  const cases: [string, string[], ArchiveManifest["objects"]][] = [
    ["a second object", ["one", "two"], [{ hash: sha256("one"), size: 100, media_type: "text/plain" }]],
    ["one byte more than declared", ["four"], [{ hash: sha256("four"), size: 3, media_type: "text/plain" }]],
  ];
  for (const [, payloads, objects] of cases) {
    const { destination, result } = await backup(noOverrides, async (input, parent) => {
      input.objectRoot = (await objectStore(parent, payloads)).objectRoot;
      input.manifest = manifest(objects);
    });
    await expect(result).rejects.toMatchObject(failure("object_inventory_changed", "object store exceeds the authority manifest"));
    await partialGone(destination);
  }
});

const HEX = "0123456789abcdef".repeat(4);
const ONE = sha256("one"), ONE_PREFIX = ONE.slice(0, 2), OTHER_PREFIX = ONE_PREFIX === "00" ? "01" : "00";
type StoreSetup = (objectRoot: string, parent: string) => Promise<void>;
const malformedStores: [string, StoreSetup, string, string][] = [
  ["an entry that is not a two-hex prefix", async root => { await mkdir(join(root, "abc")); }, "invalid_object_store", "unexpected object-store entry"],
  ["an entry whose tail is a two-hex prefix", async root => { await mkdir(join(root, "xab")); }, "invalid_object_store", "unexpected object-store entry"],
  ["a prefix entry that is a regular file", async root => { await writeFile(join(root, ONE_PREFIX), "file", { mode: 0o600 }); }, "unsafe_path", "object prefix is not owned/private"],
  ["an orphan sidecar", async root => { await putObject(root, ONE_PREFIX, ONE, Buffer.from("one")); await writeFile(join(root, ONE_PREFIX, `${HEX}.json`), SIDECAR(HEX, 1)); }, "invalid_object_store", "orphan object sidecar"],
  ["a sidecar whose base has a trailing non-hex character", async root => { await putObject(root, HEX.slice(0, 2), `${HEX}x`, Buffer.from("x")); }, "invalid_object_store", "orphan object sidecar"],
  ["a sidecar whose base has a leading non-hex character", async root => { await putObject(root, "ab", `x${HEX}`, Buffer.from("x")); }, "invalid_object_store", "orphan object sidecar"],
  ["an object filed under the wrong prefix", async root => { await putObject(root, OTHER_PREFIX, ONE, Buffer.from("one")); }, "invalid_object_store", "object inventory contains an invalid path"],
  ["an object without a sidecar", async root => { await putObject(root, ONE_PREFIX, ONE, Buffer.from("one"), null); }, "invalid_object_store", "object inventory contains an invalid path"],
  ["a group-writable prefix directory", async root => { await putObject(root, ONE_PREFIX, ONE, Buffer.from("one")); await chmod(join(root, ONE_PREFIX), 0o720); }, "unsafe_path", "object prefix is not owned/private"],
  ["a hard-linked object", async (root, parent) => { await link(await putObject(root, ONE_PREFIX, ONE, Buffer.from("one")), join(parent, "alias")); }, "unsafe_path", "object is not a bounded owned regular file"],
  ["an object that is a symlink to identical bytes", async (root, parent) => { await writeFile(join(parent, "elsewhere"), "one", { mode: 0o600 }); await rm(await putObject(root, ONE_PREFIX, ONE, Buffer.from("one"))); await symlink(join(parent, "elsewhere"), join(root, ONE_PREFIX, ONE)); }, "unsafe_path", "object is not a bounded owned regular file"],
  ["a group-writable object", async root => { await chmod(await putObject(root, ONE_PREFIX, ONE, Buffer.from("one")), 0o620); }, "unsafe_path", "object is not a bounded owned regular file"],
  ["an object one byte over the size limit", async root => { await truncate(await putObject(root, ONE_PREFIX, ONE, Buffer.from("one"), SIDECAR(ONE, 3)), ARCHIVE_LIMITS.object_bytes + 1); }, "unsafe_path", "object is not a bounded owned regular file"],
  ["an object exactly at the size limit whose sidecar disagrees", async root => { await truncate(await putObject(root, ONE_PREFIX, ONE, Buffer.from("one"), SIDECAR(ONE, 3)), ARCHIVE_LIMITS.object_bytes); }, "object_corrupt", "object sidecar disagrees with data"],
  ["a sidecar naming another hash", async root => { await putObject(root, ONE_PREFIX, ONE, Buffer.from("one"), SIDECAR(HEX, 3)); }, "object_corrupt", "object sidecar disagrees with data"],
  ["a sidecar with the wrong size", async root => { await putObject(root, ONE_PREFIX, ONE, Buffer.from("one"), SIDECAR(ONE, 4)); }, "object_corrupt", "object sidecar disagrees with data"],
  ["a sidecar whose media type is not text", async root => { await putObject(root, ONE_PREFIX, ONE, Buffer.from("one"), SIDECAR(ONE, 3, 1)); }, "object_corrupt", "object sidecar disagrees with data"],
  ["data that does not hash to its name", async root => { await putObject(root, ONE_PREFIX, ONE, Buffer.from("two"), SIDECAR(ONE, 3)); }, "object_corrupt", "object hash mismatch"],
  ["a group-writable object store root", async root => { await chmod(root, 0o720); }, "unsafe_path", "owned private directory required"],
  ["an object store root that is a file", async root => { await rm(root, { recursive: true }); await writeFile(root, "file"); }, "unsafe_path", "owned private directory required"],
  ["an object store root that is a symlink", async (root, parent) => { await rm(root, { recursive: true }); await symlink(parent, root); }, "unsafe_path", "owned private directory required"],
];
for (const [name, setup, code, detail] of malformedStores) test(`backupOwned refuses an object store with ${name}`, async () => {
  const { destination, result } = await backup(noOverrides, async (input, parent) => {
    const { objectRoot } = await objectStore(parent, []);
    await setup(objectRoot, parent);
    input.objectRoot = objectRoot;
    input.manifest = manifest([{ hash: ONE, size: 3, media_type: "text/plain" }]);
  });
  await expect(result).rejects.toMatchObject(failure(code, detail));
  await partialGone(destination);
});

type StatPatch = (stat: BigIntStats) => Partial<BigIntStats>;
/** The nth lstat of `path` answers with its real stat shadowed by `patch`; every other lstat passes through. */
function perturbLstat(path: string, nth: number, patch: StatPatch) {
  const real = fsp.lstat; let seen = 0;
  function perturbed(target: PathLike, options?: StatOptions & { bigint?: false | undefined }): Promise<Stats>;
  function perturbed(target: PathLike, options: StatOptions & { bigint: true }): Promise<BigIntStats>;
  function perturbed(target: PathLike, options?: StatOptions): Promise<Stats | BigIntStats>;
  async function perturbed(target: PathLike, options?: StatOptions): Promise<Stats | BigIntStats> {
    const stat = await real(target, options);
    if (target !== path || ++seen !== nth || !("mtimeNs" in stat)) return stat;
    return Object.assign(Object.create(stat) as BigIntStats, patch(stat));
  }
  return spyOn(fsp, "lstat").mockImplementation(perturbed);
}

test("backupOwned refuses an object store whose root, prefix or object changed while it was being copied", async () => {
  const object = (root: string) => join(root, ONE_PREFIX, ONE), prefix = (root: string) => join(root, ONE_PREFIX), self = (root: string) => root;
  const cases: [(objectRoot: string) => string, StatPatch, string][] = [
    [object, stat => ({ ino: stat.ino + 1n }), "object changed during snapshot"],
    [object, stat => ({ dev: stat.dev + 1n }), "object changed during snapshot"],
    [object, stat => ({ size: stat.size + 1n }), "object changed during snapshot"],
    [object, stat => ({ mtimeNs: stat.mtimeNs + 1n }), "object changed during snapshot"],
    [prefix, stat => ({ ino: stat.ino + 1n }), "object prefix changed"],
    [prefix, stat => ({ dev: stat.dev + 1n }), "object prefix changed"],
    [prefix, stat => ({ mtimeNs: stat.mtimeNs + 1n }), "object prefix changed"],
    [self, stat => ({ ino: stat.ino + 1n }), "object root changed"],
  ];
  for (const [target, patch, detail] of cases) {
    let spy: ReturnType<typeof perturbLstat> | undefined;
    const { destination, result } = await backup(noOverrides, async (input, parent) => {
      await withObjects(["one"])(input, parent);
      spy = perturbLstat(target(input.objectRoot!), 2, patch);
    });
    try { await expect(result).rejects.toMatchObject(failure("source_changed", detail)); } finally { spy?.mockRestore(); }
    await partialGone(destination);
  }
});

/** Assumes Bun's writeFile/readFile/mkdtemp/createReadStream/createWriteStream bypass the exported fs/promises.open and the fixture adapter never open()s a file; an unexpected extra path here means one of those changed. */
test("backupOwned fsyncs every file and directory it publishes and closes each descriptor", async () => {
  const synced: string[] = [], closed: string[] = [], real = fsp.open;
  const spy = spyOn(fsp, "open").mockImplementation(async (path, flags, mode) => {
    const handle = await real(path, flags, mode), { sync, close } = handle;
    handle.sync = async () => { synced.push(String(path)); await sync.call(handle); };
    handle.close = async () => { closed.push(String(path)); await close.call(handle); };
    return handle;
  });
  try {
    const { parent, destination, result } = await backup(noOverrides, withObjects(["one"]));
    await result;
    const partial = `${destination}.${OPERATION}.partial`, objectDir = join(partial, "objects", ONE_PREFIX), object = join(objectDir, ONE);
    const published = [`${object}.tmp`, objectDir, `${object}.json`, objectDir, join(partial, "database", "neo4j.dump"), join(partial, "database", "neo4j.dump.metadata.json"), join(partial, "manifest.json"), join(partial, "backup.complete"), partial, parent];
    expect(synced.toSorted()).toEqual(published.toSorted());
    expect(closed.toSorted()).toEqual(published.toSorted());
  } finally { spy.mockRestore(); }
});

test("backupOwned refuses to overwrite a file the adapter left in the partial archive", async () => {
  const base = fakeAdapter().adapter;
  const planting = (name: string): AdapterOverrides => () => ({ materializeMembers: async (root, target) => { await base.materializeMembers(root, target); await writeFile(join(root, name), "planted"); } });
  const plants: AdapterOverrides[] = [
    () => ({ dumpOffline: async (dump, epoch) => { const result = await base.dumpOffline(dump, epoch); await writeFile(join(dirname(dump), "neo4j.dump.metadata.json"), "planted"); return result; } }),
    planting("manifest.json"),
    planting("backup.complete"),
  ];
  for (const overrides of plants) {
    const { destination, result } = await backup(overrides);
    await expect(result).rejects.toMatchObject({ code: "EEXIST" });
    await partialGone(destination);
  }
});

test("backupOwned removes the partial directory when the dump fails", async () => {
  const { destination, calls, result } = await backup(record => ({ dumpOffline: async () => { record("dumpOffline"); throw new Error("dump_exploded"); } }));
  await expect(result).rejects.toThrow("dump_exploded");
  expect(calls).not.toContain("startAndReady");
  await partialGone(destination);
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
type RestoreInput = Parameters<typeof restoreOwned>[0];
const restoreInput = (paths: Omit<RestoreInput, "operationId" | "compatibility" | "expectedSourceId">, operationId = OPERATION): RestoreInput => ({ ...paths, operationId, compatibility, expectedSourceId: SOURCE });

test("restoreOwned swaps the admitted archive into a private live root behind the adapter and drops the rollback copy", async () => {
  const { archive, live, staging, rollback } = await archiveFromBackup();
  const { adapter, calls } = fakeAdapter();
  const admitted = await restoreOwned(restoreInput({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback }), adapter);
  expect(admitted.manifest.operation_id).toBe(OPERATION);
  expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startRestored", "restoredAuthoritySnapshot", "rebindSource"]);
  expect(await mode(live)).toBe(0o700);
  await expect(lstat(join(live, "sentinel"))).rejects.toMatchObject({ code: "ENOENT" });
  await expect(lstat(rollback)).rejects.toMatchObject({ code: "ENOENT" });
  await expect(lstat(staging)).rejects.toMatchObject({ code: "ENOENT" });
});

test("restoreOwned quarantines staging when the restarted source does not match or is not ready", async () => {
  const readiness: [string, Partial<Awaited<ReturnType<ReturnType<typeof fakeAdapter>["adapter"]["startRestored"]>>>][] = [["impostor", { sourceId: "impostor" }], ["not ready", { ready: false }], ["wrong epoch", { epoch: "99" }]];
  for (const [, patch] of readiness) {
    const { archive, live, staging, rollback } = await archiveFromBackup();
    const { adapter, calls } = fakeAdapter(record => ({ startRestored: async (_root, epoch) => { record("startRestored"); return { sourceId: SOURCE, epoch, ready: true, ...patch }; } }));
    await expect(restoreOwned(restoreInput({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback }), adapter))
      .rejects.toMatchObject(failure("source_rebind_mismatch", "restored source is not the expected authority"));
    expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startRestored", `quarantine:${staging}`]);
    await expectRolledBack(live, staging, rollback);
  }
});

/** After a post-promotion failure the old root is live again, the restored tree waits under the staging name, and no rollback copy remains. */
async function expectRolledBack(live: string, staging: string, rollback: string) {
  expect(await readFile(join(live, "sentinel"), "utf8")).toBe("old live data");
  expect((await lstat(staging)).isDirectory()).toBe(true);
  await expect(lstat(join(staging, "sentinel"))).rejects.toMatchObject({ code: "ENOENT" });
  await expect(lstat(rollback)).rejects.toMatchObject({ code: "ENOENT" });
}

test("restoreOwned still quarantines the rejected tree when the post-promotion undo fails", async () => {
  const { archive, live, staging, rollback } = await archiveFromBackup();
  // The rollback copy vanishes while the promoted tree is being verified, so `rename(rollback, live)` cannot succeed.
  const { adapter, calls } = fakeAdapter(record => ({
    restoredAuthoritySnapshot: async () => { record("restoredAuthoritySnapshot"); await rm(rollback, { recursive: true }); throw new Error("snapshot_exploded"); },
  }));
  const result = restoreOwned(restoreInput({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback }), adapter);
  await expect(result).rejects.toMatchObject({ code: "rollback_failed" });
  await expect(result).rejects.toThrow(/snapshot_exploded; undo: .*ENOENT/);
  expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startRestored", "restoredAuthoritySnapshot", `quarantine:${staging}`]);
  // The undo got as far as moving the rejected tree back under the staging name; no live root remains.
  await expect(lstat(join(staging, "sentinel"))).rejects.toMatchObject({ code: "ENOENT" });
  await expect(lstat(live)).rejects.toMatchObject({ code: "ENOENT" });
});

test("restoreOwned reports a quarantine failure beside the refusal that caused it", async () => {
  const { archive, live, staging, rollback } = await archiveFromBackup();
  const { adapter, calls } = fakeAdapter(record => ({
    startRestored: async (_root, epoch) => { record("startRestored"); return { sourceId: "impostor", epoch, ready: true }; },
    quarantine: async root => { record(`quarantine:${root}`); throw new Error("container_stuck"); },
  }));
  const result = restoreOwned(restoreInput({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback }), adapter);
  await expect(result).rejects.toMatchObject({ code: "quarantine_failed" });
  await expect(result).rejects.toThrow(/source_rebind_mismatch: .*; quarantine: Error: container_stuck/);
  expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startRestored", `quarantine:${staging}`]);
  await expectRolledBack(live, staging, rollback);
});

test("restoreOwned refuses an archive whose operation id differs before touching the adapter", async () => {
  const { archive, live, staging, rollback } = await archiveFromBackup();
  const { adapter, calls } = fakeAdapter();
  await expect(restoreOwned(restoreInput({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback }, "01993000-0000-7000-8000-000000000099"), adapter))
    .rejects.toMatchObject(failure("identity_conflict", "archive operation mismatch"));
  expect(calls).toEqual([]);
  await expect(lstat(staging)).rejects.toMatchObject({ code: "ENOENT" });
});

test.each(["members", "physical_links", "invalidation_evidence", "source_hashes"] as const)(
  "restoreOwned refuses a changed %s digest before rebinding or discarding rollback", async field => {
    const { archive, live, staging, rollback } = await archiveFromBackup();
    const { adapter, calls } = fakeAdapter(record => ({
      restoredAuthoritySnapshot: async () => {
        record("restoredAuthoritySnapshot");
        const restored = fixtureAuthority();
        restored[field] = { ...restored[field], sha256: "b".repeat(64) };
        return restored;
      },
    }));
    await expect(restoreOwned(restoreInput({ archive, liveRoot: live, stagingRoot: staging, rollbackRoot: rollback }), adapter))
      .rejects.toMatchObject(failure("authority_digest_mismatch", "restored authority does not match manifest"));
    expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startRestored", "restoredAuthoritySnapshot", `quarantine:${staging}`]);
    await expectRolledBack(live, staging, rollback);
  });
