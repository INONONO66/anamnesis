import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { createReadStream, createWriteStream } from "node:fs";
import { mkdir, open, rename, rm, lstat, readdir, readFile, writeFile } from "node:fs/promises";
import { Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { dirname, join, resolve } from "node:path";
import { preflightArchive, verifyAuthoritySnapshot, type ArchiveCompatibility, type ArchiveManifest, type AuthoritySnapshot } from "./archive-manifest.ts";

/** The only boundary at which process, Neo4j, spool, or source identity effects may occur. */
export interface TrustedAuthorityAdapter {
  revokeWriters(): Promise<{ epoch: string; cutoff: ArchiveManifest["cutoff"] }>;
  authoritySnapshot(epoch: string, limit?: number): Promise<AuthoritySnapshot>;
  dumpOffline(destination: string, epoch: string): Promise<{ metadata: Uint8Array; neo4jVersion: string; imageDigest: string }>;
  materializeMembers(root: string, manifest: ArchiveManifest): Promise<void>;
  startAndReady(root: string, epoch: string): Promise<{ sourceId: string; epoch: string; ready: boolean }>;
  stop(): Promise<void>;
  restoreOffline(archive: string, staging: string, manifest: ArchiveManifest): Promise<void>;
  rebindSource(sourceId: string): Promise<void>;
  verifyPhysicalLinks(root: string): Promise<void>;
  quarantine(root: string): Promise<void>;
}

export interface BackupInput { root: string; destination: string; operationId: string; compatibility: ArchiveCompatibility; manifest: ArchiveManifest; objectRoot?: string; }
export interface RestoreInput { archive: string; liveRoot: string; stagingRoot: string; rollbackRoot: string; operationId: string; compatibility: ArchiveCompatibility; expectedSourceId: string; }
export class AuthorityOrchestrationError extends Error { constructor(public readonly code: string, message: string) { super(`${code}: ${message}`); } }
const hash = (b: Uint8Array | string) => createHash("sha256").update(b).digest("hex");
const canonical = (v: unknown): string => Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : v !== null && typeof v === "object" ? `{${Object.keys(v as object).sort().map(k => `${JSON.stringify(k)}:${canonical((v as Record<string, unknown>)[k])}`).join(",")}}` : JSON.stringify(v);
const ownedDir = async (p: string) => { const s = await lstat(p, { bigint: true }); if (!s.isDirectory() || s.isSymbolicLink() || s.uid !== BigInt(process.getuid!()) || (s.mode & 0o022n)) throw new AuthorityOrchestrationError("unsafe_path", "owned private directory required"); return s; };
const fsync = async (p: string) => { const f = await open(p, constants.O_RDONLY); try { await f.sync(); } finally { await f.close(); } };
const fresh = async (p: string) => { try { await lstat(p); throw new AuthorityOrchestrationError("destination_exists", p); } catch (e) { if (!(e instanceof Error && "code" in e && (e as NodeJS.ErrnoException).code === "ENOENT")) throw e; } };
const HASH = /^[0-9a-f]{64}$/;
const OBJECT_LIMIT = 10_000, OBJECT_BYTES = 256 * 1024 ** 2, TOTAL_OBJECT_BYTES = 128 * 1024 ** 3;

/** Enumerates and copies committed ObjectStore pairs without buffering payloads.
 * The source is fenced by owner/device/inode/stat checks; every destination is
 * exclusive and published only after a streamed hash and fsync. */
export async function snapshotObjectStore(sourceRoot: string, destinationRoot: string): Promise<ArchiveManifest["objects"]> {
  const source = resolve(sourceRoot), destination = resolve(destinationRoot);
  const root = await ownedDir(source), destinationParent = await ownedDir(dirname(destination));
  void destinationParent;
  await fresh(destination); await mkdir(destination, { mode: 0o700 }); await ownedDir(destination);
  const entries: ArchiveManifest["objects"] = []; let total = 0;
  try {
    const prefixes = (await readdir(source)).sort();
    if (prefixes.some(p => !/^[0-9a-f]{2}$/.test(p))) throw new AuthorityOrchestrationError("invalid_object_store", "unexpected object-store entry");
    for (const prefix of prefixes) {
      const dir = join(source, prefix), ds = await lstat(dir, { bigint: true });
      if (!ds.isDirectory() || ds.isSymbolicLink() || ds.uid !== BigInt(process.getuid!()) || (ds.mode & 0o022n) !== 0n) throw new AuthorityOrchestrationError("unsafe_path", "object prefix is not owned/private");
      const names = (await readdir(dir)).sort();
      if (names.some(name => name.endsWith(".json") && (!HASH.test(name.slice(0, -5)) || !names.includes(name.slice(0, -5))))) throw new AuthorityOrchestrationError("invalid_object_store", "orphan object sidecar");
      for (const name of names) {
        if (name.endsWith(".json")) continue;
        if (!HASH.test(name) || !name.startsWith(prefix) || !names.includes(`${name}.json`)) throw new AuthorityOrchestrationError("invalid_object_store", "object inventory contains an invalid path");
        if (entries.length >= OBJECT_LIMIT) throw new AuthorityOrchestrationError("object_limit", "object count limit");
        const data = join(dir, name), sidecar = `${data}.json`, before = await lstat(data, { bigint: true });
        if (!before.isFile() || before.isSymbolicLink() || before.nlink !== 1n || before.uid !== BigInt(process.getuid!()) || (before.mode & 0o022n) !== 0n || before.size > BigInt(OBJECT_BYTES)) throw new AuthorityOrchestrationError("unsafe_path", "object is not a bounded owned regular file");
        const raw = JSON.parse(await readFile(sidecar, "utf8")) as Record<string, unknown>;
        if (raw.hash !== name || raw.size !== Number(before.size) || typeof raw.mediaType !== "string") throw new AuthorityOrchestrationError("object_corrupt", "object sidecar disagrees with data");
        total += Number(before.size); if (total > TOTAL_OBJECT_BYTES) throw new AuthorityOrchestrationError("object_limit", "total object bytes limit");
        const targetDir = join(destination, "objects", prefix); await mkdir(targetDir, { recursive: true, mode: 0o700 });
        const target = join(targetDir, name), temp = `${target}.${inputTemp()}.tmp`; await fresh(target); await fresh(`${target}.json`);
        const digest = createHash("sha256");
        await pipeline(createReadStream(data, { flags: "r" }), new Transform({ transform(chunk, _encoding, callback) { digest.update(chunk); callback(null, chunk); } }), createWriteStream(temp, { flags: "wx", mode: 0o600 }));
        const after = await lstat(data, { bigint: true }); if (after.ino !== before.ino || after.dev !== before.dev || after.size !== before.size || after.mtimeNs !== before.mtimeNs) throw new AuthorityOrchestrationError("source_changed", "object changed during snapshot");
        if (digest.digest("hex") !== name) throw new AuthorityOrchestrationError("object_corrupt", "object hash mismatch");
        await fsync(temp); await rename(temp, target); await fsync(targetDir);
        await writeFile(`${target}.json`, JSON.stringify({ hash: name, size: Number(before.size), mediaType: raw.mediaType }), { flag: "wx", mode: 0o600 }); await fsync(`${target}.json`); await fsync(targetDir);
        entries.push({ hash: name, size: Number(before.size), media_type: raw.mediaType });
      }
      const finalDir = await lstat(dir, { bigint: true }); if (finalDir.ino !== ds.ino || finalDir.dev !== ds.dev || finalDir.mtimeNs !== ds.mtimeNs) throw new AuthorityOrchestrationError("source_changed", "object prefix changed");
    }
    if (root.ino !== (await lstat(source, { bigint: true })).ino) throw new AuthorityOrchestrationError("source_changed", "object root changed");
    return entries.sort((a, b) => a.hash < b.hash ? -1 : 1);
  } catch (error) { await rm(destination, { recursive: true, force: true }); throw error; }
}
function inputTemp(): string { return `${process.pid}-${Math.random().toString(16).slice(2)}`; }

/** Creates an archive only from an adapter-supplied, already complete authority snapshot. */
export async function backupOwned(input: BackupInput, adapter: TrustedAuthorityAdapter): Promise<ArchiveManifest> {
  const root = resolve(input.root), destination = resolve(input.destination);
  await ownedDir(root); await ownedDir(dirname(destination)); await fresh(destination);
  if (destination.startsWith(root + "/") || root.startsWith(destination + "/")) throw new AuthorityOrchestrationError("unsafe_path", "overlapping roots");
  if (input.manifest.operation_id !== input.operationId) throw new AuthorityOrchestrationError("identity_conflict", "manifest operation mismatch");
  const cutoff = await adapter.revokeWriters();
  if (canonical(cutoff.cutoff) !== canonical(input.manifest.cutoff)) throw new AuthorityOrchestrationError("stale_epoch", "cutoff changed before dump");
  if (!input.manifest.authority) throw new AuthorityOrchestrationError("authority_snapshot_unavailable", "manifest authority evidence is absent");
  const authority = verifyAuthoritySnapshot(await adapter.authoritySnapshot(cutoff.epoch));
  if (canonical(authority) !== canonical(input.manifest.authority)) throw new AuthorityOrchestrationError("authority_snapshot_changed", "authority snapshot does not match cutoff manifest");
  const partial = `${destination}.${input.operationId}.partial`;
  await mkdir(partial, { mode: 0o700 });
  try {
    await mkdir(join(partial, "database"), { mode: 0o700 });
      const dump = await adapter.dumpOffline(join(partial, "database", "neo4j.dump"), cutoff.epoch);
    const expectedDump = input.manifest.members.find(m => m.role === "database_dump");
    const dumpPath = join(partial, "database", "neo4j.dump");
    const dumpBytes = await readFile(dumpPath);
    if (!expectedDump) throw new AuthorityOrchestrationError("invalid_dump", "manifest has no database dump member");
    expectedDump.bytes = dumpBytes.byteLength;
    expectedDump.sha256 = hash(dumpBytes);
    const expectedMetadata = input.manifest.members.find(m => m.role === "dump_metadata");
    if (!expectedMetadata) throw new AuthorityOrchestrationError("invalid_dump", "manifest has no dump metadata member");
    const metadata = Buffer.from(canonical({ format: "anamnesis.archive-dump/1", database: "neo4j", dump_path: expectedDump.path, bytes: expectedDump.bytes, sha256: expectedDump.sha256, neo4j_version: input.manifest.compatibility.neo4j_version, neo4j_image_digest: input.manifest.compatibility.neo4j_image_digest }));
    expectedMetadata.bytes = metadata.byteLength;
    expectedMetadata.sha256 = hash(metadata);
    await writeFile(join(partial, "database", "neo4j.dump.metadata.json"), metadata, { flag: "wx", mode: 0o600 });
    void dump;
    if (input.objectRoot) {
      const objects = await snapshotObjectStore(input.objectRoot, join(partial, "objects"));
      if (objects.length === 0) await rm(join(partial, "objects"), { recursive: true, force: true });
      for (const member of input.manifest.members.filter(member => member.role === "object_sidecar")) {
        const sidecar = await readFile(join(partial, member.path));
        member.bytes = sidecar.byteLength; member.sha256 = hash(sidecar);
      }
      if (canonical(objects) !== canonical(input.manifest.objects)) throw new AuthorityOrchestrationError("object_inventory_changed", "ObjectStore inventory does not match the authority manifest");
    }
    await adapter.materializeMembers(partial, input.manifest);
    const manifestBytes = Buffer.from(canonical(input.manifest));
    await writeFile(join(partial, "manifest.json"), manifestBytes, { flag: "wx", mode: 0o600 });
    await fsync(join(partial, "manifest.json")); await fsync(join(partial, "database", "neo4j.dump")); await fsync(join(partial, "database", "neo4j.dump.metadata.json"));
    const marker = canonical({ format: "anamnesis.archive-complete/1", operation_id: input.operationId, manifest_sha256: hash(manifestBytes), manifest_bytes: manifestBytes.length });
    await writeFile(join(partial, "backup.complete"), marker, { flag: "wx", mode: 0o600 });
    await fsync(join(partial, "backup.complete")); await fsync(partial); await rename(partial, destination); await fsync(dirname(destination));
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
  await mkdir(staging, { mode: 0o700 });
  try {
    // The adapter must perform the actual Neo4j restore and physical-link rebuild.
    await adapter.stop();
    await adapter.restoreOffline(archive, staging, admitted.manifest);
    await adapter.verifyPhysicalLinks(staging);
    await rename(live, rollback); await rename(staging, live); await fsync(dirname(live));
    const ready = await adapter.startAndReady(live, admitted.manifest.cutoff.ingest_seq.toString());
    if (!ready.ready || ready.sourceId !== input.expectedSourceId || ready.epoch !== admitted.manifest.cutoff.ingest_seq.toString()) throw new AuthorityOrchestrationError("source_rebind_mismatch", "restored source is not the expected authority");
    await adapter.rebindSource(input.expectedSourceId);
    await rm(rollback, { recursive: true, force: false });
    return admitted;
  } catch (e) { await adapter.quarantine(staging).catch(() => undefined); throw e; }
}
