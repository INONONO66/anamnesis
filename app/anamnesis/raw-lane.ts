import { logEvent } from "./log.ts";
import { createHash } from "node:crypto";
import { isAbsolute, relative, resolve, sep } from "node:path";
import type { RememberInput } from "@anamnesis/core";
import { RpcRememberParams } from "@anamnesis/protocol";
import { RpcClient } from "./client.ts";
import { ingestSnapshot, sourceRevisionKey, type SourceRecord } from "./source.ts";
import { lines, sha, snapshotTree, type FileInfo } from "./source-files.ts";

const MAX_FILES = 16_384;
const MAX_ENTRIES = 65_536;
const MAX_DEPTH = 64;
const MAX_FILE_BYTES = 256 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 4 * 1024 * 1024 * 1024;
const MAX_REVISIONS = 100_000;

/** One caller-owned, offline, producer-sealed raw export format. `selects`
 * decides which files under the root belong to the lane, `parser` builds the
 * per-file record parser, and `lineage` marks lanes whose conversational
 * turns carry explicit D49 lineage metadata. */
export interface RawLane {
  source: string;
  format: string;
  lineage: boolean;
  selects(local: string): boolean;
  parser(name: string): LaneParser;
}
export type LaneParser = (record: Uint8Array) => ReadonlyArray<{ input: RememberInput }>;
/** Adapts a line-oriented text parser to the byte records the lane reads. */
export const textParser = (parse: (line: string) => ReadonlyArray<{ input: RememberInput }>): LaneParser => record => parse(new TextDecoder("utf-8", { fatal: true }).decode(record));
/** Selects .jsonl files whose path has no segment starting with `prefix`. */
export const jsonlOutside = (prefix: string) => (local: string): boolean => local.endsWith(".jsonl") && !local.split(sep).some(part => part.startsWith(prefix));

/** Map a transcript actor to explicit lineage metadata (D49).
 * Only conversational turns (user, assistant) are admitted as semantic-eligible; tool and system records stay
 * metadata-free on purpose ("tool" is a legal origin_role, but tool output is not claim material and would only
 * cost extraction calls). The mapping depends on the record alone, so a retry repeats the identical lineage body. */
function getLineageMetadata(actor: string): { origin_role: "user" | "assistant"; lineage_mode: "direct"; parent_recall_ids: [] } | Record<never, never> {
  if (actor === "user") return { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] };
  if (actor === "assistant") return { origin_role: "assistant", lineage_mode: "direct", parent_recall_ids: [] };
  return {};
}
interface Tree { files: string[]; fingerprints: Map<string, string>; }
/** Admits one selected export file and returns its byte size. */
function admitFile(path: string, info: FileInfo): number {
  if (!info.isFile()) throw new Error(`source_not_regular_file: ${path}`);
  if (info.size > BigInt(MAX_FILE_BYTES)) throw new Error(`source_file_too_large: ${path}`);
  return Number(info.size);
}
function tree(root: string, lane: RawLane): Promise<Tree> {
  // Bounded depth-first code-unit file order; no export-wide episode sort.
  return snapshotTree(root, { maxDepth: MAX_DEPTH, maxEntries: MAX_ENTRIES }, { maxFiles: MAX_FILES, maxSnapshotBytes: MAX_SNAPSHOT_BYTES }, (path, local, info) => lane.selects(local) ? admitFile(path, info) : undefined);
}

interface Head { native: string; revision: string; key: string; previous: string | null; signature: string }
/** Physical file/line/block order defines observed occurrences; full validated
 * replay reconstructs session headers and revision predecessors. */
export class Revisions {
  private readonly heads = new Map<string, Head>();
  private readonly seen = new Set<string>();
  private ordinal = 0;
  admit(base: RpcRememberParams, lineage: boolean, at: string): { params: RpcRememberParams; native: string } {
    this.ordinal++;
    const o = base.episode.origin, origin = sha(JSON.stringify([o.source, o.session, o.actor, o.record]));
    const native = base.source_revision, signature = sha(JSON.stringify(base)), head = this.heads.get(origin), duplicate = head?.native === native;
    if (duplicate && head.signature !== signature) throw new Error(`source_revision_conflict: ${at}`);
    const seenKey = sha(JSON.stringify([origin, native]));
    const revision = duplicate ? head.revision : this.seen.has(seenKey) ? `${native}:occurrence:${this.ordinal}` : native;
    const lineageMetadata = lineage ? getLineageMetadata(base.episode.origin.actor) : {};
    const params = RpcRememberParams.parse({ ...base, source_revision: revision, expected_previous_revision_key: duplicate ? head.previous : head?.key ?? null, ...lineageMetadata });
    if (!duplicate) {
      if (this.seen.size >= MAX_REVISIONS || this.heads.size >= MAX_REVISIONS) throw new Error("source_revision_limit");
      this.seen.add(seenKey);
      this.heads.set(origin, { native, revision, key: sourceRevisionKey(params), previous: params.expected_previous_revision_key, signature });
    }
    return { params, native };
  }
}
function parseRecord(parse: LaneParser, bytes: Uint8Array, at: string): ReadonlyArray<{ input: RememberInput }> {
  try { return parse(bytes); }
  catch (cause) { throw new Error(`source_invalid_record: ${at}`, { cause }); }
}
function baseParams(input: RememberInput, body: Buffer | undefined, at: string): RpcRememberParams {
  try {
    return RpcRememberParams.parse({ episode: { schema: input.schema, time: input.time, content: input.content, origin: input.origin, properties: input.properties }, source_revision: input.source_revision, expected_previous_revision_key: null, ...(body ? { payload_hash: sha(body) } : {}) });
  } catch (cause) { throw new Error(`source_invalid_record: ${at}`, { cause }); }
}
const payloadBody = (input: RememberInput): Buffer | undefined => input.payload === undefined ? undefined : Buffer.from(input.payload);
function sourceRecord(input: RememberInput, body: Buffer | undefined, params: RpcRememberParams, native: string, name: string, line: number): SourceRecord {
  return { params, context: { file: name, line, native_source_revision: native }, ...(body ? { payload: { bytes_b64: body.toString("base64"), media_type: input.payload_media_type! } } : {}) };
}
type Manifest = { file: string; sha256: string }[];
function sealFile(manifest: Manifest, name: string, digest: string, validating: boolean): void {
  if (validating) manifest.push({ file: name, sha256: digest });
  else if (digest !== manifest.find(entry => entry.file === name)!.sha256) throw new Error("source_changed");
}
async function* laneRecords(root: string, lane: RawLane, initial: Tree, manifest: Manifest, validating: boolean): AsyncGenerator<SourceRecord> {
  const revisions = new Revisions();
  for (const name of initial.files) {
    const parse = lane.parser(name);
    const hash = createHash("sha256");
    for await (const { bytes, line } of lines(root, name, initial.fingerprints.get(name)!, hash)) {
      const at = `${name}:${line}`;
      for (const { input } of parseRecord(parse, bytes, at)) {
        const body = payloadBody(input);
        const { params, native } = revisions.admit(baseParams(input, body, at), lane.lineage, at);
        yield sourceRecord(input, body, params, native, name, line);
      }
    }
    sealFile(manifest, name, hash.digest("hex"), validating);
  }
}

/** Caller-owned, offline, producer-sealed raw export only. LF termination is
 * required admission, NOT proof of completion. No live tail/rotation or inferred
 * missing history. Complete validation (including occurrence/body mapping) runs
 * before any RPC, retaining only finite metadata per record. */
export async function ingestRawLane(root: string, checkpoint: string, client: RpcClient, lane: RawLane): Promise<void> {
  root = resolve(root);
  const cp = relative(root, resolve(checkpoint));
  if (!isAbsolute(cp) && cp !== ".." && !cp.startsWith(".." + sep)) throw new Error("source_checkpoint_path_conflict");
  await ingestSnapshot(checkpoint, client, async () => {
    const initial = await tree(root, lane);
    const assertUnchanged = async () => {
      const now = await tree(root, lane);
      if (JSON.stringify(now.files) !== JSON.stringify(initial.files) || JSON.stringify([...now.fingerprints]) !== JSON.stringify([...initial.fingerprints])) throw new Error("source_changed");
    };
    const manifest: Manifest = [];
    for await (const _record of laneRecords(root, lane, initial, manifest, true)) { /* bounded preflight */ }
    const sourceHash = sha(JSON.stringify({ format: lane.format, manifest, fingerprints: [...initial.fingerprints] }));
    return { sourceHash, records: laneRecords(root, lane, initial, manifest, false), assertUnchanged };
  });
  logEvent("info", "source_scope", { source: lane.source, snapshot: "complete", live_tail: "unsupported", rotation: "unsupported", producer_sealed: "required" });
}
