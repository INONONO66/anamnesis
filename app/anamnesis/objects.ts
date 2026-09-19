import { createHash, randomUUID, type Hash } from "node:crypto";
import { lstat, mkdir, open, readdir, readFile, unlink, type FileHandle } from "node:fs/promises";
import { join } from "node:path";
import { ObjectStore } from "../../packages/core/src/objects.ts";
import { RPC_LIMITS, RpcObjectMetadata } from "../../packages/protocol/src/rpc.ts";
import { hasCode, syncDirectory } from "./config.ts";
import { RpcFault } from "./wire.ts";

interface Upload {
  owner: object;
  path: string;
  file: FileHandle;
  hash: Hash;
  expected: RpcObjectMetadata;
  received: number;
  next: number;
  deadline: number;
  actual: number;
  failed: boolean;
  invalid: boolean;
}
export interface UploadClock {
  now(): number;
  setTimeout(callback: () => void, delay: number): unknown;
  clearTimeout(timer: unknown): void;
}
export interface UploadLifecycle {
  /** The daemon's serial mutation owner, not a concurrent timer writer. */
  enqueue?: (job: () => Promise<void>) => void;
  clock?: UploadClock;
}
const HOUR = 60 * 60 * 1000;
const clock: UploadClock = {
  now: () => performance.now(),
  setTimeout: (callback, delay) => { const timer = setTimeout(callback, delay); timer.unref(); return timer; },
  clearTimeout: timer => clearTimeout(timer as ReturnType<typeof setTimeout>),
};
const temporaryId = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
export class Uploads {
  readonly store: ObjectStore;
  private readonly uploads = new Map<string, Upload>();
  private survivingBytes = 0;
  private survivingCount = 0;
  private ready = false;
  private closed = false;
  private timer: unknown;
  private generation = 0;
  private readonly clock: UploadClock;
  constructor(readonly root: string, private readonly temporary: string, private readonly lifecycle: UploadLifecycle = {}) {
    this.store = new ObjectStore(root);
    this.clock = lifecycle.clock ?? clock;
  }
  async init(): Promise<void> {
    await mkdir(this.root, { recursive: true, mode: 0o700 });
    await mkdir(this.temporary, { recursive: true, mode: 0o700 });
    const directory = await lstat(this.temporary);
    if (!directory.isDirectory() || directory.uid !== process.getuid?.() || (directory.mode & 0o777) !== 0o700) {
      throw new Error("upload temporary directory must be private and owned");
    }
    const abandoned: Array<{ path: string; size: number; ino: number; dev: number }> = [];
    // Installation ownership is acquired before init. Never recover session
    // handles. Count everything first; only this implementation's private,
    // single-link UUID files are eligible for abandoned-temp reclamation.
    for (const name of await readdir(this.temporary)) {
      const path = join(this.temporary, name), info = await lstat(path);
      if (!info.isFile()) throw new Error("unrecognized upload artifact; admission stopped");
      this.survivingCount++;
      this.survivingBytes += info.size;
      if (temporaryId.test(name) && info.uid === process.getuid?.() && (info.mode & 0o777) === 0o600 && info.nlink === 1) {
        abandoned.push({ path, size: info.size, ino: info.ino, dev: info.dev });
      }
    }
    for (const artifact of abandoned) {
      const info = await lstat(artifact.path);
      if (info.ino !== artifact.ino || info.dev !== artifact.dev || info.size !== artifact.size || !info.isFile() || info.nlink !== 1) {
        throw new Error("upload artifact changed during recovery");
      }
      await unlink(artifact.path);
      this.survivingBytes -= artifact.size; this.survivingCount--;
    }
    await syncDirectory(this.temporary);
    this.ready = true;
  }
  private schedule(): void {
    if (this.timer !== undefined) this.clock.clearTimeout(this.timer);
    this.timer = undefined;
    const generation = ++this.generation;
    if (this.closed || !this.lifecycle.enqueue) return;
    const deadline = Math.min(...[...this.uploads.values()].filter(u => !u.invalid).map(u => u.deadline));
    if (!Number.isFinite(deadline)) return;
    this.timer = this.clock.setTimeout(() => {
      this.timer = undefined;
      this.lifecycle.enqueue!(async () => {
        if (!this.closed && generation === this.generation) await this.expire();
      });
    }, Math.max(0, deadline - this.clock.now()));
  }
  /** Called only inside the same serial owner as begin/chunk/commit/disconnect. */
  async expire(): Promise<void> {
    try {
      for (const [id, upload] of this.uploads) {
        if (!upload.invalid && upload.deadline <= this.clock.now()) await this.remove(id, upload);
      }
    } finally { this.schedule(); }
  }
  /** Hash verification plus renewed fsync, not stat-only reuse/adoption. */
  async metadata(hash: string): Promise<RpcObjectMetadata | null> {
    const path = join(this.root, hash.slice(0, 2), hash);
    let raw: string;
    try { raw = await readFile(path + ".json", "utf8"); }
    catch (error) { if (hasCode(error, "ENOENT")) return null; throw error; }
    const metadata = JSON.parse(raw) as Record<string, unknown>;
    const parsed = RpcObjectMetadata.safeParse({ hash: metadata["hash"], size: metadata["size"], media_type: metadata["mediaType"] });
    if (!parsed.success || parsed.data.hash !== hash || !await this.store.has(hash)) {
      throw new RpcFault("object_corrupt", "object data or metadata failed verification");
    }
    for (const name of [path, path + ".json"]) {
      const file = await open(name, "r");
      try { await file.sync(); } finally { await file.close(); }
    }
    await syncDirectory(join(this.root, hash.slice(0, 2)));
    await syncDirectory(this.root);
    return { hash, size: parsed.data.size, media_type: parsed.data.media_type };
  }
  async begin(owner: object, expected: RpcObjectMetadata) {
    if (!this.ready || this.closed) throw new RpcFault("resource_exhausted", "upload admission is stopped");
    await this.expire();
    const existing = await this.metadata(expected.hash);
    if (existing) {
      if (existing.size !== expected.size || existing.media_type !== expected.media_type) {
        throw new RpcFault("object_metadata_conflict", "hash already has different immutable metadata");
      }
      return { state: "committed" as const, object: existing };
    }
    const all = [...this.uploads.values()];
    const owned = all.filter(upload => upload.owner === owner);
    if (all.length + this.survivingCount >= RPC_LIMITS.uploads || owned.length >= RPC_LIMITS.uploads_per_connection ||
      all.reduce((n, u) => n + Math.max(u.expected.size, u.actual), expected.size + this.survivingBytes) > RPC_LIMITS.upload_temp_bytes ||
      owned.reduce((n, u) => n + Math.max(u.expected.size, u.actual), expected.size) > RPC_LIMITS.upload_temp_bytes_per_connection) {
      throw new RpcFault("resource_exhausted", "upload reservation limit reached");
    }
    const id = randomUUID();
    const path = join(this.temporary, id);
    let file: FileHandle;
    try { file = await open(path, "wx", 0o600); }
    catch (error) {
      if (hasCode(error, "ENOSPC") || hasCode(error, "EDQUOT") || hasCode(error, "EMFILE") || hasCode(error, "ENFILE")) {
        throw new RpcFault("resource_exhausted", `upload open failed: ${String(error)}`);
      }
      throw error;
    }
    this.uploads.set(id, { owner, path, file, expected, hash: createHash("sha256"), received: 0, next: 0,
      deadline: this.clock.now() + HOUR, actual: 0, failed: false, invalid: false });
    this.schedule();
    return { state: "uploading" as const, upload_id: id, next_seq: 0, chunk_bytes_max: RPC_LIMITS.chunk_bytes };
  }
  private find(owner: object, id: string): Upload {
    const upload = this.uploads.get(id);
    if (!upload || upload.owner !== owner || upload.invalid || upload.deadline <= this.clock.now() || this.closed) {
      throw new RpcFault("upload_not_found", "upload does not belong to this connection or has expired");
    }
    if (upload.failed) throw new RpcFault("resource_exhausted", "upload rollback failed; upload is stopped until cleanup");
    return upload;
  }
  async chunk(owner: object, id: string, seq: number, bytes: Buffer) {
    const upload = this.find(owner, id);
    if (seq !== upload.next) throw new RpcFault("upload_sequence_mismatch", "unexpected chunk sequence");
    if (upload.received + bytes.length > upload.expected.size) throw new RpcFault("object_size_mismatch", "upload exceeds declared size");
    try {
      const { bytesWritten } = await upload.file.write(bytes, 0, bytes.length, upload.received);
      if (bytesWritten !== bytes.length) throw new Error("short upload write");
    } catch (error) {
      // Reserve the attempted length until truncate, stat and fsync prove the
      // prior verified prefix restored. Never advance digest or sequence early.
      upload.actual = upload.received + bytes.length;
      try {
        await upload.file.truncate(upload.received);
        const info = await upload.file.stat();
        if (info.size !== upload.received) throw new Error("upload rollback length mismatch");
        await upload.file.sync();
        upload.actual = upload.received;
      } catch (rollback) {
        upload.failed = true;
        throw new RpcFault("resource_exhausted", `upload write failed: ${String(error)}; rollback failed: ${String(rollback)}`);
      }
      throw new RpcFault("resource_exhausted", `upload write rolled back: ${String(error)}`);
    }
    upload.actual = upload.received + bytes.length;
    upload.hash.update(bytes);
    upload.received += bytes.length;
    upload.next++;
    return { upload_id: id, next_seq: upload.next };
  }
  async commit(owner: object, id: string): Promise<RpcObjectMetadata> {
    const upload = this.find(owner, id);
    if (upload.received !== upload.expected.size) throw new RpcFault("object_size_mismatch", "upload is incomplete");
    try {
      if (upload.hash.digest("hex") !== upload.expected.hash) throw new RpcFault("object_hash_mismatch", "upload digest differs from declared hash");
      await upload.file.sync();
      const existing = await this.metadata(upload.expected.hash);
      if (existing && existing.media_type !== upload.expected.media_type) throw new RpcFault("object_metadata_conflict", "object metadata changed during upload");
      // Current Engine/ObjectStore consume Uint8Array; only this bounded,
      // serialized commit materializes up to object_bytes, never each chunk.
      await this.store.put(await readFile(upload.path), upload.expected.media_type);
      const committed = await this.metadata(upload.expected.hash);
      if (!committed) throw new RpcFault("object_corrupt", "object publication lacks durable metadata");
      return committed;
    } finally { try { await this.remove(id, upload); } finally { this.schedule(); } }
  }
  private async remove(id: string, upload: Upload): Promise<void> {
    upload.invalid = true; // Cleanup failure must not resurrect a public handle.
    await upload.file.close();
    try { await unlink(upload.path); }
    catch (error) { if (!hasCode(error, "ENOENT")) throw error; }
    this.uploads.delete(id); // Retain count and byte reservation until deletion.
  }
  async disconnect(owner: object): Promise<void> {
    const owned = [...this.uploads].filter(([, upload]) => upload.owner === owner);
    for (const [, upload] of owned) upload.invalid = true;
    try { for (const [id, upload] of owned) await this.remove(id, upload); }
    finally { this.schedule(); }
  }
  async close(): Promise<void> {
    this.closed = true; this.ready = false; this.schedule();
    for (const [id, upload] of this.uploads) await this.remove(id, upload);
    // Unknown artifacts are retained and charged on the next startup, never
    // recursively erased. An empty private directory is safe to retain too.
  }
}
