import { createHash } from "node:crypto";
import { type BigIntStats, constants } from "node:fs";
import { createReadStream, createWriteStream } from "node:fs";
import { mkdir, open, rename, rm, lstat, readdir, readFile, writeFile } from "node:fs/promises";
import { Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { dirname, join, resolve } from "node:path";
import { ARCHIVE_LIMITS, preflightArchive, verifyAuthoritySnapshot, type ArchiveCompatibility, type ArchiveManifest, type AuthoritySnapshot } from "./archive-manifest.ts";

/** The only boundary at which process, Neo4j, spool, or source identity effects may occur. */
export interface TrustedAuthorityAdapter {
  revokeWriters(): Promise<{ epoch: string; cutoff: ArchiveManifest["cutoff"] }>;
  authoritySnapshot(epoch: string): Promise<AuthoritySnapshot>;
  restoredAuthoritySnapshot(): Promise<AuthoritySnapshot>;
  dumpOffline(destination: string, epoch: string): Promise<{ metadata: Uint8Array; neo4jVersion: string; imageDigest: string }>;
  materializeMembers(root: string, manifest: ArchiveManifest): Promise<void>;
  /** Backup: restart the fenced source under `root`. */
  startAndReady(root: string, epoch: string): Promise<{ sourceId: string; epoch: string; ready: boolean }>;
  /** Restore: start the database `restoreOffline` loaded under `root`; `restoredAuthoritySnapshot` then reads it. */
  startRestored(root: string, epoch: string): Promise<{ sourceId: string; epoch: string; ready: boolean }>;
  stop(): Promise<void>;
  restoreOffline(archive: string, staging: string, manifest: ArchiveManifest): Promise<void>;
  rebindSource(sourceId: string): Promise<void>;
  verifyPhysicalLinks(root: string): Promise<void>;
  quarantine(root: string): Promise<void>;
}

export interface BackupInput { root: string; destination: string; operationId: string; compatibility: ArchiveCompatibility; manifest: ArchiveManifest; objectRoot?: string; }
export interface RestoreInput { archive: string; liveRoot: string; stagingRoot: string; rollbackRoot: string; operationId: string; compatibility: ArchiveCompatibility; expectedSourceId: string; }
class AuthorityOrchestrationError extends Error { constructor(public readonly code: string, message: string) { super(`${code}: ${message}`); } }
const hash = (b: Uint8Array | string) => createHash("sha256").update(b).digest("hex");
const canonical = (v: unknown): string => Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : v !== null && typeof v === "object" ? `{${Object.keys(v as object).sort().map(k => `${JSON.stringify(k)}:${canonical((v as Record<string, unknown>)[k])}`).join(",")}}` : JSON.stringify(v);
const ownedPrivate = (s: BigIntStats) => s.uid === BigInt(process.getuid!()) && (s.mode & 0o022n) === 0n;
const ownedDir = async (p: string) => { const s = await lstat(p, { bigint: true }); if (!s.isDirectory() || !ownedPrivate(s)) throw new AuthorityOrchestrationError("unsafe_path", "owned private directory required"); return s; };
const fsync = async (p: string) => { const f = await open(p, constants.O_RDONLY); try { await f.sync(); } finally { await f.close(); } };
const publish = async (p: string, bytes: Uint8Array | string) => { await writeFile(p, bytes, { flag: "wx", mode: 0o600 }); await fsync(p); };
const fresh = async (p: string) => { try { await lstat(p); throw new AuthorityOrchestrationError("destination_exists", p); } catch (e) { if (!(e instanceof Error && (e as NodeJS.ErrnoException).code === "ENOENT")) throw e; } };
const objectBytes = (objects: ArchiveManifest["objects"]) => objects.reduce((bytes, object) => bytes + object.size, 0);
const HASH = /^[0-9a-f]{64}$/;

/** One object prefix directory: owned, unshared, and every sidecar paired with its object; returns its stat and sorted names. */
async function admitPrefix(dir: string): Promise<{ ds: BigIntStats; names: string[] }> {
  const ds = await lstat(dir, { bigint: true });
  if (!ds.isDirectory() || !ownedPrivate(ds)) throw new AuthorityOrchestrationError("unsafe_path", "object prefix is not owned/private");
  const names = (await readdir(dir)).sort();
  if (names.some(name => name.endsWith(".json") && (!HASH.test(name.slice(0, -5)) || !names.includes(name.slice(0, -5))))) throw new AuthorityOrchestrationError("invalid_object_store", "orphan object sidecar");
  return { ds, names };
}
/** One named object: a regular file with a single link, owned and unshared, whose sidecar agrees with it. */
async function admitObject(dir: string, name: string): Promise<{ data: string; before: BigIntStats; mediaType: string }> {
  const data = join(dir, name), sidecar = `${data}.json`, before = await lstat(data, { bigint: true });
  if (!before.isFile() || before.nlink !== 1n || !ownedPrivate(before) || before.size > BigInt(ARCHIVE_LIMITS.object_bytes)) throw new AuthorityOrchestrationError("unsafe_path", "object is not a bounded owned regular file");
  const raw = JSON.parse(await readFile(sidecar, "utf8")) as Record<string, unknown>;
  if (raw.hash !== name || raw.size !== Number(before.size) || typeof raw.mediaType !== "string") throw new AuthorityOrchestrationError("object_corrupt", "object sidecar disagrees with data");
  return { data, before, mediaType: raw.mediaType };
}
/** Copy one admitted object to `<target>.tmp` while hashing it, verify the source did not move, then publish data and sidecar. */
async function copyObject(object: Awaited<ReturnType<typeof admitObject>>, prefix: string, name: string, destination: string): Promise<ArchiveManifest["objects"][number]> {
  const { data, before, mediaType } = object;
  const targetDir = join(destination, prefix); await mkdir(targetDir, { recursive: true, mode: 0o700 });
  const target = join(targetDir, name), temp = `${target}.tmp`;
  const digest = createHash("sha256");
  await pipeline(createReadStream(data), new Transform({ transform(chunk, _encoding, callback) { digest.update(chunk); callback(null, chunk); } }), createWriteStream(temp, { flags: "wx", mode: 0o600 }));
  const after = await lstat(data, { bigint: true }); if (after.ino !== before.ino || after.dev !== before.dev || after.size !== before.size || after.mtimeNs !== before.mtimeNs) throw new AuthorityOrchestrationError("source_changed", "object changed during snapshot");
  if (digest.digest("hex") !== name) throw new AuthorityOrchestrationError("object_corrupt", "object hash mismatch");
  await fsync(temp); await rename(temp, target); await fsync(targetDir);
  await publish(`${target}.json`, JSON.stringify({ hash: name, size: Number(before.size), mediaType })); await fsync(targetDir);
  return { hash: name, size: Number(before.size), media_type: mediaType };
}
/** Enumerates and copies committed ObjectStore pairs without buffering payloads.
 * The source is fenced by owner/device/inode/stat checks; every destination is
 * exclusive and published only after a streamed hash and fsync. Prefixes and
 * names are visited in sorted order, so the inventory comes out sorted by hash;
 * copying stops as soon as the store holds more objects or bytes than the
 * authority manifest declares; the caller owns the destination's parent and
 * removes it on failure. */
async function snapshotObjectStore(sourceRoot: string, destinationRoot: string, expected: ArchiveManifest["objects"]): Promise<ArchiveManifest["objects"]> {
  const source = resolve(sourceRoot), destination = resolve(destinationRoot);
  const root = await ownedDir(source); await ownedDir(dirname(destination));
  await fresh(destination); await mkdir(destination, { mode: 0o700 }); await ownedDir(destination);
  const entries: ArchiveManifest["objects"] = [], budget = objectBytes(expected); let total = 0;
  const prefixes = (await readdir(source)).sort();
  if (prefixes.some(p => !/^[0-9a-f]{2}$/.test(p))) throw new AuthorityOrchestrationError("invalid_object_store", "unexpected object-store entry");
  for (const prefix of prefixes) {
    const dir = join(source, prefix), { ds, names } = await admitPrefix(dir);
    for (const name of names) {
      if (name.endsWith(".json")) continue;
      if (!name.startsWith(prefix) || !names.includes(`${name}.json`)) throw new AuthorityOrchestrationError("invalid_object_store", "object inventory contains an invalid path");
      const object = await admitObject(dir, name);
      total += Number(object.before.size);
      if (entries.length >= expected.length || total > budget) throw new AuthorityOrchestrationError("object_inventory_changed", "object store exceeds the authority manifest");
      entries.push(await copyObject(object, prefix, name, destination));
    }
    const finalDir = await lstat(dir, { bigint: true }); if (finalDir.ino !== ds.ino || finalDir.dev !== ds.dev || finalDir.mtimeNs !== ds.mtimeNs) throw new AuthorityOrchestrationError("source_changed", "object prefix changed");
  }
  if (root.ino !== (await lstat(source, { bigint: true })).ino) throw new AuthorityOrchestrationError("source_changed", "object root changed");
  return entries;
}

/** Creates an archive only from an adapter-supplied, already complete authority snapshot. */
export async function backupOwned(input: BackupInput, adapter: TrustedAuthorityAdapter): Promise<ArchiveManifest> {
  const root = resolve(input.root), destination = resolve(input.destination);
  await ownedDir(root); await ownedDir(dirname(destination)); await fresh(destination);
  if (destination.startsWith(root + "/")) throw new AuthorityOrchestrationError("unsafe_path", "overlapping roots");
  if (input.manifest.operation_id !== input.operationId) throw new AuthorityOrchestrationError("identity_conflict", "manifest operation mismatch");
  if (input.manifest.objects.length > ARCHIVE_LIMITS.objects || objectBytes(input.manifest.objects) > ARCHIVE_LIMITS.total_member_bytes) throw new AuthorityOrchestrationError("object_limit", "manifest exceeds the archive object limits");
  const cutoff = await adapter.revokeWriters();
  if (canonical(cutoff.cutoff) !== canonical(input.manifest.cutoff)) throw new AuthorityOrchestrationError("stale_epoch", "cutoff changed before dump");
  if (!input.manifest.authority) throw new AuthorityOrchestrationError("authority_snapshot_unavailable", "manifest authority evidence is absent");
  const authority = verifyAuthoritySnapshot(await adapter.authoritySnapshot(cutoff.epoch));
  if (canonical(authority) !== canonical(input.manifest.authority)) throw new AuthorityOrchestrationError("authority_snapshot_changed", "authority snapshot does not match cutoff manifest");
  const partial = `${destination}.${input.operationId}.partial`;
  await mkdir(partial, { mode: 0o700 });
  try {
    await mkdir(join(partial, "database"), { mode: 0o700 });
    const dumpPath = join(partial, "database", "neo4j.dump");
    await adapter.dumpOffline(dumpPath, cutoff.epoch);
    const expectedDump = input.manifest.members.find(m => m.role === "database_dump");
    const dumpBytes = await readFile(dumpPath); await fsync(dumpPath);
    if (!expectedDump) throw new AuthorityOrchestrationError("invalid_dump", "manifest has no database dump member");
    expectedDump.bytes = dumpBytes.byteLength;
    expectedDump.sha256 = hash(dumpBytes);
    const expectedMetadata = input.manifest.members.find(m => m.role === "dump_metadata");
    if (!expectedMetadata) throw new AuthorityOrchestrationError("invalid_dump", "manifest has no dump metadata member");
    const metadata = Buffer.from(canonical({ format: "anamnesis.archive-dump/1", database: "neo4j", dump_path: expectedDump.path, bytes: expectedDump.bytes, sha256: expectedDump.sha256, neo4j_version: input.manifest.compatibility.neo4j_version, neo4j_image_digest: input.manifest.compatibility.neo4j_image_digest }));
    expectedMetadata.bytes = metadata.byteLength;
    expectedMetadata.sha256 = hash(metadata);
    await publish(join(partial, "database", "neo4j.dump.metadata.json"), metadata);
    if (input.objectRoot) {
      const objects = await snapshotObjectStore(input.objectRoot, join(partial, "objects"), input.manifest.objects);
      if (canonical(objects) !== canonical(input.manifest.objects)) throw new AuthorityOrchestrationError("object_inventory_changed", "ObjectStore inventory does not match the authority manifest");
      if (objects.length === 0) await rm(join(partial, "objects"), { recursive: true });
      for (const member of input.manifest.members.filter(member => member.role === "object_sidecar")) {
        const sidecar = await readFile(join(partial, member.path));
        member.bytes = sidecar.byteLength; member.sha256 = hash(sidecar);
      }
    }
    await adapter.materializeMembers(partial, input.manifest);
    const manifestBytes = Buffer.from(canonical(input.manifest));
    await publish(join(partial, "manifest.json"), manifestBytes);
    const marker = canonical({ format: "anamnesis.archive-complete/1", operation_id: input.operationId, manifest_sha256: hash(manifestBytes), manifest_bytes: manifestBytes.length });
    await publish(join(partial, "backup.complete"), marker);
    await fsync(partial); await rename(partial, destination); await fsync(dirname(destination));
    await adapter.startAndReady(root, cutoff.epoch);
    return input.manifest;
  } catch (e) { await rm(partial, { recursive: true, force: true }); throw e; }
}

/** Stages and admits an archive; activation remains entirely behind the trusted adapter. */
export async function restoreOwned(input: RestoreInput, adapter: TrustedAuthorityAdapter) {
  const archive = resolve(input.archive), live = resolve(input.liveRoot), staging = resolve(input.stagingRoot), rollback = resolve(input.rollbackRoot);
  await ownedDir(archive); await ownedDir(dirname(live)); await fresh(staging); await fresh(rollback);
  const admitted = await preflightArchive(archive, input.compatibility);
  if (admitted.manifest.operation_id !== input.operationId) throw new AuthorityOrchestrationError("identity_conflict", "archive operation mismatch");
  if (!admitted.manifest.authority) throw new AuthorityOrchestrationError("authority_snapshot_unavailable", "manifest authority evidence is absent");
  await mkdir(staging, { mode: 0o700 });
  let promoted = false;
  try {
    // The adapter must perform the actual Neo4j restore and physical-link rebuild.
    await adapter.stop();
    await adapter.restoreOffline(archive, staging, admitted.manifest);
    await adapter.verifyPhysicalLinks(staging);
    await rename(live, rollback); await rename(staging, live); await fsync(dirname(live));
    promoted = true;
    const ready = await adapter.startRestored(live, admitted.manifest.cutoff.ingest_seq.toString());
    if (!ready.ready || ready.sourceId !== input.expectedSourceId || ready.epoch !== admitted.manifest.cutoff.ingest_seq.toString()) throw new AuthorityOrchestrationError("source_rebind_mismatch", "restored source is not the expected authority");
    const restored = verifyAuthoritySnapshot(await adapter.restoredAuthoritySnapshot());
    if (canonical(restored) !== canonical(admitted.manifest.authority)) throw new AuthorityOrchestrationError("authority_digest_mismatch", "restored authority does not match manifest");
    await adapter.rebindSource(input.expectedSourceId);
    await rm(rollback, { recursive: true });
    return admitted;
  } catch (e) {
    // A tree that failed verification after promotion must not stay live: move it back
    // to the staging name and return the rollback copy to live. Quarantine runs whether or
    // not that undo succeeded, so the container serving the rejected tree never outlives
    // the refusal; a cleanup failure is reported beside the refusal, never instead of it.
    let failure = e;
    if (promoted) {
      try { await rename(live, staging); await rename(rollback, live); await fsync(dirname(live)); }
      catch (undo) { failure = new AuthorityOrchestrationError("rollback_failed", `${String(e)}; undo: ${String(undo)}`); }
    }
    try { await adapter.quarantine(staging); }
    catch (cleanup) { failure = new AuthorityOrchestrationError("quarantine_failed", `${String(failure)}; quarantine: ${String(cleanup)}`); }
    throw failure;
  }
}
