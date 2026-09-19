import { createHash } from "node:crypto";
import { lstat, open, readFile, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { RPC_LIMITS, RpcHash, RpcIngestStatusParams, RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { acquireInstallation, atomicJson, hasCode, syncDirectory } from "./config.ts";
import { RpcClient } from "./client.ts";

// This adapter consumes immutable JSONL snapshots, not live/tailing source logs.
// A fixed allocation bounds the snapshot even if another process grows the file.
export const SOURCE_MAX_BYTES = 16 * 1024 * 1024;
interface Checkpoint { version: 1; source_hash: string; data_incarnation: string; next: number; last: RpcIngestStatusParams | null; }
function parseCheckpoint(value: unknown): Checkpoint {
  if (!value || typeof value !== "object") throw new Error("source_checkpoint_invalid");
  const data = value as Record<string, unknown>;
  if (data["version"] !== 1 || typeof data["next"] !== "number" || !Number.isSafeInteger(data["next"]) || data["next"] < 0) throw new Error("source_checkpoint_invalid");
  return { version: 1, source_hash: RpcHash.parse(data["source_hash"]), data_incarnation: RpcIngestStatusParams.shape.data_incarnation.parse(data["data_incarnation"]), next: data["next"], last: data["last"] === null ? null : RpcIngestStatusParams.parse(data["last"]) };
}
export interface SourceRecord {
  params: RpcRememberParams;
  payload?: { bytes_b64: string; media_type: string };
  context?: { file: string; line: number; native_source_revision: string };
}
export interface SourceSnapshot {
  sourceHash: string;
  records: AsyncIterable<SourceRecord>;
  assertUnchanged(): Promise<void>;
}
interface Pending extends SourceRecord { version: 1; source_hash: string; index: number; identity: RpcIngestStatusParams; }
function parsePending(value: unknown): Pending {
  if (!value || typeof value !== "object") throw new Error("source_pending_invalid");
  const data = value as Record<string, unknown>;
  if (data["version"] !== 1 || typeof data["index"] !== "number" || !Number.isSafeInteger(data["index"]) || data["index"] < 0) throw new Error("source_pending_invalid");
  // Optional payload/context are not acted on until exact replay comparison.
  return { version: 1, source_hash: RpcHash.parse(data["source_hash"]), index: data["index"], params: RpcRememberParams.parse(data["params"]), identity: RpcIngestStatusParams.parse(data["identity"]),
    ...(data["payload"] === undefined ? {} : { payload: data["payload"] as NonNullable<SourceRecord["payload"]> }),
    ...(data["context"] === undefined ? {} : { context: data["context"] as NonNullable<SourceRecord["context"]> }) };
}
// Match the versioned RPC admission digest, including normalized defaults and
// explicit origin tuple order. A resolvable receipt alone is not a source cursor.
function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, member]) => `${JSON.stringify(key)}:${canonical(member)}`).join(",")}}`;
}
const sha = (value: string) => createHash("sha256").update(value).digest("hex");
export function sourceRevisionKey(params: RpcRememberParams): string {
  const o = params.episode.origin;
  return sha(JSON.stringify([sha(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision]));
}
function deliveryIdentity(params: RpcRememberParams, incarnation: string): RpcIngestStatusParams {
  return { revision_key: sourceRevisionKey(params),
    body_digest: sha(canonical({ digest_version: 1, params })), data_incarnation: incarnation };
}
function sameIdentity(a: RpcIngestStatusParams, b: RpcIngestStatusParams): boolean {
  return a.revision_key === b.revision_key && a.body_digest === b.body_digest && a.data_incarnation === b.data_incarnation;
}
const pendingFailure = (state: string) => Object.assign(new Error(`source_pending_${state}`), { code: `source_pending_${state}` });
interface SourceFile { bytes: Buffer; fingerprint: string; }
async function sourceFingerprint(path: string): Promise<string> {
  const info = await lstat(path, { bigint: true });
  if (!info.isFile()) throw new Error("source_not_regular_file");
  return [info.dev, info.ino, info.size, info.mtimeNs, info.ctimeNs, info.mode].join(":");
}
async function snapshot(path: string): Promise<SourceFile> {
  const before = await sourceFingerprint(path);
  const file = await open(path, "r");
  try {
    const bytes = Buffer.allocUnsafe(SOURCE_MAX_BYTES + 1);
    let length = 0;
    while (length < bytes.length) {
      const { bytesRead } = await file.read(bytes, length, bytes.length - length, null);
      if (!bytesRead) {
        const after = await sourceFingerprint(path);
        if (after !== before) throw new Error("source_changed");
        return { bytes: bytes.subarray(0, length), fingerprint: before };
      }
      length += bytesRead;
    }
    if (await sourceFingerprint(path) !== before) throw new Error("source_changed");
    throw new Error("source_too_large");
  } finally { await file.close(); }
}

export async function ingestSource(source: string, checkpointPath: string, client: RpcClient): Promise<void> {
  if ([checkpointPath, checkpointPath + ".pending.json"].some(path => resolve(source) === resolve(path))) throw new Error("source_checkpoint_path_conflict");
  await ingestSnapshot(checkpointPath, client, async () => {
    const initial = await snapshot(source);
    const sourceHash = createHash("sha256").update(initial.bytes).digest("hex");
    const lines = new TextDecoder("utf-8", { fatal: true }).decode(initial.bytes).split("\n");
    if (lines.at(-1) === "") lines.pop();
    return { sourceHash, records: (async function* () {
      for (const line of lines) {
        if (Buffer.byteLength(line) > RPC_LIMITS.frame_bytes) throw new Error("source_record_too_large");
        yield { params: RpcRememberParams.parse(JSON.parse(line)) };
      }
    })(), async assertUnchanged() {
      const current = await snapshot(source);
      if (current.fingerprint !== initial.fingerprint || createHash("sha256").update(current.bytes).digest("hex") !== sourceHash) throw new Error("source_changed");
    } };
  });
}

async function uploadPayload(client: RpcClient, record: SourceRecord): Promise<void> {
  const payload = record.payload!;
  const bytes = Buffer.from(payload.bytes_b64, "base64");
  const hash = createHash("sha256").update(bytes).digest("hex");
  if (hash !== record.params.payload_hash) throw pendingFailure("payload_mismatch");
  const begin = await client.request("object.begin", { sha256: hash, size: bytes.length, media_type: payload.media_type });
  let object;
  if (begin.state === "committed") object = begin.object;
  else {
    for (let offset = 0, seq = 0; offset < bytes.length; offset += RPC_LIMITS.chunk_bytes, seq++) {
      const result = await client.request("object.chunk", { upload_id: begin.upload_id, seq, bytes_b64: bytes.subarray(offset, offset + RPC_LIMITS.chunk_bytes).toString("base64") });
      if (result.next_seq !== seq + 1) throw pendingFailure("upload_sequence");
    }
    object = await client.request("object.commit", { upload_id: begin.upload_id });
  }
  if (object.hash !== hash || object.size !== bytes.length || object.media_type !== payload.media_type) throw pendingFailure("payload_mismatch");
}

/** Durable pending (including normalized defaults/object body) -> confirmed
 * COMMIT -> durable checkpoint -> retire. Replay is bounded by the adapter and
 * validates the immutable anchor before reconciliation, never blind resends. */
export async function ingestSnapshot(checkpointPath: string, client: RpcClient, prepare: () => Promise<SourceSnapshot>): Promise<void> {
  const pendingPath = checkpointPath + ".pending.json";
  const lease = await acquireInstallation(resolve(checkpointPath) + ".lease");
  try {
    const snapshot = await prepare();
    const { sourceHash } = snapshot;
    const status = await client.request("status", {});
    let pending: Pending | null;
    try { pending = parsePending(JSON.parse(await readFile(pendingPath, "utf8"))); }
    catch (error) { if (!hasCode(error, "ENOENT")) throw error; pending = null; }
    let checkpoint: Checkpoint;
    try { checkpoint = parseCheckpoint(JSON.parse(await readFile(checkpointPath, "utf8"))); }
    catch (error) {
      if (!hasCode(error, "ENOENT")) throw error;
      if (pending) throw new Error("source_checkpoint_missing");
      checkpoint = { version: 1, source_hash: sourceHash, data_incarnation: status.data_incarnation, next: 0, last: null };
      await atomicJson(checkpointPath, checkpoint);
    }
    if (checkpoint.source_hash !== sourceHash) throw new Error("source_changed");
    if (checkpoint.data_incarnation !== status.data_incarnation) throw new Error("incarnation_mismatch");
    if ((checkpoint.next === 0) !== (checkpoint.last === null)) throw new Error("source_checkpoint_invalid");
    if (pending && (pending.source_hash !== sourceHash ||
      (pending.index !== checkpoint.next && pending.index !== checkpoint.next - 1) ||
      !sameIdentity(pending.identity, deliveryIdentity(pending.params, checkpoint.data_incarnation)))) throw pendingFailure("invalid");
    const retire = async () => {
      await lease.assertOwned();
      await rm(pendingPath);
      await syncDirectory(dirname(checkpointPath));
      pending = null;
    };
    const resumeNext = checkpoint.next;
    let count = 0;
    for await (const record of snapshot.records) {
      const next = count++;
      const identity = deliveryIdentity(record.params, checkpoint.data_incarnation);
      if (pending && pending.index === next && canonical({ params: pending.params, ...(pending.payload === undefined ? {} : { payload: pending.payload }), ...(pending.context === undefined ? {} : { context: pending.context }) }) !== canonical(record)) throw pendingFailure("invalid");
      if (next === resumeNext - 1) {
        if (!checkpoint.last || !sameIdentity(checkpoint.last, identity)) throw new Error("source_checkpoint_invalid");
        const result = await client.request("ingest.status", checkpoint.last);
        if (!sameIdentity(result, checkpoint.last)) throw new Error("source_checkpoint_invalid");
        if (result.state !== "committed") throw new Error(`source_checkpoint_${result.state}`);
        if (pending && pending.index === next) await retire();
      }
      if (next < resumeNext) continue;
      await snapshot.assertUnchanged();
      const recovering = pending !== null;
      if (!pending) {
        pending = { version: 1, source_hash: sourceHash, index: next, ...record, identity };
        await lease.assertOwned();
        await atomicJson(pendingPath, pending);
      }
      if (!recovering && pending.payload) await uploadPayload(client, pending);
      const result = recovering
        ? await client.request("ingest.status", pending.identity)
        : await client.request("remember", pending.params);
      if (!sameIdentity(result, pending.identity)) throw pendingFailure("identity_mismatch");
      if (result.state !== "committed") throw pendingFailure(result.state);
      if (recovering) console.log(JSON.stringify({ event: "source_reconciled", index: next, state: result.state, identity: pending.identity }));
      await snapshot.assertUnchanged();
      checkpoint = { ...checkpoint, next: next + 1, last: pending.identity };
      await lease.assertOwned();
      await atomicJson(checkpointPath, checkpoint);
      await retire();
      console.log(JSON.stringify({ event: "source_checkpoint", next: checkpoint.next, state: result.state }));
    }
    if (count < resumeNext) throw new Error("source_checkpoint_invalid");
    if (pending) throw pendingFailure("invalid");
    await snapshot.assertUnchanged();
    console.log(JSON.stringify({ event: "source_complete", next: checkpoint.next, source_hash: sourceHash }));
  } finally { await lease.release(); }
}
