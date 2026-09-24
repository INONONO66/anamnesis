#!/usr/bin/env bun
// Hub vault -> ingest snapshots (#215).
//
//   bun scripts/ops/vault-to-snapshots.ts --root <vault source dir> --out <dir> [--limit-sessions N] [--since ISO]
//
// Reads ONE vault source directory (`<root>/<session_id>/<collected_ts>.<role>.<hash>.json`, one
// `schema_version: 1` record per file) and writes `snapshot-NNNN.jsonl` shards plus `manifest.json`
// that `anamnesis-ops ingest <snapshot.jsonl> <checkpoint.json>` consumes. Every JSONL line is a bare
// RpcRememberParams object: `ingestSource` (app/anamnesis/source.ts) parses each line with the strict
// schema, so no wrapper/context keys are allowed on the line.
//
// Order: sessions newest-first by the newest emitted record, records ascending within a session, and
// a session never straddles a shard boundary unless it alone exceeds the shard limit (then it spans
// consecutive shards and is listed in `manifest.split_sessions`; ingest shards in file order).
//
// Revision chain: `expected_previous_revision_key` is a CAS against the daemon's OriginHead for the
// tuple (source, session, actor, record) (docs/runtime-storage-development.md, decision log D-OriginHead).
// It therefore links successive revisions of the SAME record (same message_id, new content_hash) and is
// null for the first revision of each record, exactly as app/anamnesis/codexraw.ts does. Chaining
// consecutive records of a session would make the daemon reject every record after the first
// (`stale_revision`).
//
// Memory: two passes over the tree. Pass 1 retains only `{ session, newest }` per session directory;
// pass 2 re-reads one session at a time. Nothing beyond one session's records is ever held.
import { createHash } from "node:crypto";
import { mkdir, open, opendir, readdir, readFile, writeFile, type FileHandle } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { RPC_LIMITS, RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { SOURCE_MAX_BYTES, sourceRevisionKey } from "../../app/anamnesis/source.ts";

/** Headroom under the consumer's fixed allocation (source.ts reads at most SOURCE_MAX_BYTES). */
export const SHARD_MAX_BYTES = 15 * 1024 * 1024;
const READ_CONCURRENCY = 32;
const encoder = new TextEncoder();
const sha = (text: string) => createHash("sha256").update(text).digest("hex");
const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;

type Json = Record<string, unknown>;
const obj = (value: unknown): Json | null => value !== null && typeof value === "object" && !Array.isArray(value) ? value as Json : null;
const str = (value: unknown): string | null => typeof value === "string" ? value : null;

/** A string passes through; an array yields its `{ type, text }` blocks whose type is admitted. */
function textBlocks(value: unknown, types: readonly string[]): string | null {
  if (typeof value === "string") return value;
  if (!Array.isArray(value)) return null;
  const parts: string[] = [];
  for (const block of value) {
    const b = obj(block);
    if (b && types.includes(str(b["type"]) ?? "") && typeof b["text"] === "string") parts.push(b["text"]);
  }
  return parts.length ? parts.join("\n") : null;
}
const TEXT = ["text"] as const;
const CODEX_TEXT = ["input_text", "output_text", "text"] as const;
/** gjc / pi: `raw.message` is a string or `{ role, content }` with string or text-block content. */
const messageText = (raw: unknown): string | null => {
  const message = obj(raw)?.["message"];
  return typeof message === "string" ? message : textBlocks(obj(message)?.["content"], TEXT);
};
/** Plain-text extraction per vault `source`; null means "no text, skip the record". */
export const EXTRACTORS: Readonly<Record<string, (raw: unknown) => string | null>> = {
  codex: raw => { const payload = obj(obj(raw)?.["payload"]); return payload?.["type"] === "message" ? textBlocks(payload["content"], CODEX_TEXT) : null; },
  "claude-code": raw => textBlocks(obj(raw)?.["content"], TEXT),
  opencode: raw => { const part = obj(obj(raw)?.["part_data"]); return part?.["type"] === "text" ? str(part["text"]) : null; },
  slack: raw => str(obj(raw)?.["text"]),
  discord: raw => str(obj(raw)?.["content"]),
  gjc: messageText,
  pi: messageText,
};

interface VaultRecord { source: string; session: string; record: string | null; role: unknown; time: number; content_hash: string; scalars: Record<string, string | number>; raw: unknown; }
/** Envelope checks only; anything structurally unusable is `invalid` (the RPC schema judges the rest). */
function parseVaultRecord(text: string): VaultRecord | null {
  let data: unknown;
  try { data = JSON.parse(text); } catch { return null; }
  const r = obj(data);
  if (!r || r["schema_version"] !== 1) return null;
  const source = str(r["source"]), session = str(r["session_id"]), content_hash = str(r["content_hash"]);
  if (!source || !session || !content_hash) return null;
  const iso = str(r["occurred_at"]) ?? str(r["collected_at"]);
  const time = iso === null ? NaN : Date.parse(iso);
  if (!Number.isFinite(time)) return null;
  const id = r["message_id"];
  const scalars: Record<string, string | number> = {};
  for (const [key, field] of [["vault_channel_id", "channel_id"], ["vault_thread_id", "thread_id"], ["vault_user_id", "user_id"], ["vault_ts_source", "ts_source"]] as const) {
    const value = r[field];
    if (typeof value === "string" || typeof value === "number") scalars[key] = value;
  }
  return { source, session, record: typeof id === "string" || typeof id === "number" ? String(id) : null, role: r["role"], time, content_hash, scalars, raw: r["raw"] };
}

/** Cut at a UTF-8 boundary so the result is well-formed and at most `maxBytes` (code units <= bytes). */
function truncateUtf8(text: string, maxBytes: number): { text: string; truncated: boolean } {
  const bytes = encoder.encode(text);
  if (bytes.byteLength <= maxBytes) return { text, truncated: false };
  let end = maxBytes;
  while (end > 0 && (bytes[end]! & 0xc0) === 0x80) end--;
  return { text: new TextDecoder().decode(bytes.subarray(0, end)), truncated: true };
}

/** D49 lineage, identical to app/anamnesis/codexraw.ts: only conversational turns are semantic-eligible. */
const lineage = (actor: string) => actor === "user" || actor === "assistant" ? { origin_role: actor, lineage_mode: "direct", parent_recall_ids: [] } : {};

export interface Skipped { no_text: number; unsupported_role: number; invalid: number; duplicate: number; }
const noSkips = (): Skipped => ({ no_text: 0, unsupported_role: 0, invalid: 0, duplicate: 0 });
interface Loaded { params: RpcRememberParams; time: number; }
interface SessionLoad { records: Loaded[]; skipped: Skipped; files: number; }

/** One session directory -> validated, time-ordered, revision-chained params. Throws only on I/O and unknown sources. */
async function loadSession(dir: string): Promise<SessionLoad> {
  const skipped = noSkips();
  const names = (await readdir(dir, { withFileTypes: true })).filter(e => e.isFile() && e.name.endsWith(".json") && !e.name.startsWith(".")).map(e => e.name).sort(compare);
  const staged: { base: RpcRememberParams; time: number; name: string }[] = [];
  for (let offset = 0; offset < names.length; offset += READ_CONCURRENCY) {
    const batch = names.slice(offset, offset + READ_CONCURRENCY);
    const texts = await Promise.all(batch.map(name => readFile(join(dir, name), "utf8")));
    for (let i = 0; i < batch.length; i++) {
      const name = batch[i]!, record = parseVaultRecord(texts[i]!);
      if (!record) { skipped.invalid++; continue; }
      const extract = EXTRACTORS[record.source];
      if (!extract) throw new Error(`vault_source_unsupported: ${record.source} (${join(dir, name)})`);
      const text = extract(record.raw);
      if (text === null || !text.trim()) { skipped.no_text++; continue; }
      const role = str(record.role);
      if (!role || !role.trim()) { skipped.unsupported_role++; continue; }
      const content = truncateUtf8(text, RPC_LIMITS.content_bytes);
      const hex = record.content_hash.replace(/^[a-z0-9_-]+:/i, "");
      try {
        staged.push({ name, time: record.time, base: RpcRememberParams.parse({
          episode: {
            schema: "anamnesis.original-message/1", time: { value: new Date(record.time).toISOString(), precision: "second" }, content: content.text,
            origin: { source: `vault-${record.source}`, session: record.session, actor: role, record: record.record ?? hex }, mass: 0.5,
            properties: { ...record.scalars, vault_content_hash: record.content_hash, ...(content.truncated ? { vault_truncated: true } : {}) },
          },
          source_revision: hex, expected_previous_revision_key: null,
        }) });
      } catch { skipped.invalid++; }
    }
  }
  staged.sort((a, b) => a.time - b.time || compare(a.name, b.name));
  const heads = new Map<string, string>(), seen = new Set<string>(), records: Loaded[] = [];
  for (const { base, time } of staged) {
    const o = base.episode.origin, origin = sha(JSON.stringify([o.source, o.session, o.actor, o.record])), occurrence = sha(JSON.stringify([origin, base.source_revision]));
    if (seen.has(occurrence)) { skipped.duplicate++; continue; }
    seen.add(occurrence);
    const params = RpcRememberParams.parse({ ...base, expected_previous_revision_key: heads.get(origin) ?? null, ...lineage(o.actor) });
    heads.set(origin, sourceRevisionKey(params));
    records.push({ params, time });
  }
  return { records, skipped, files: names.length };
}

export interface ShardEntry { file: string; sessions: number; records: number; bytes: number; newest_time: string | null; oldest_time: string | null; }
class ShardWriter {
  readonly shards: ShardEntry[] = [];
  readonly split: string[] = [];
  private handle: FileHandle | null = null;
  private current: ShardEntry | null = null;
  private sessions = new Set<string>();
  private newest = -Infinity;
  private oldest = Infinity;
  constructor(private readonly out: string, private readonly limit: number) {}
  private async open(): Promise<ShardEntry> {
    const file = `snapshot-${String(this.shards.length).padStart(4, "0")}.jsonl`;
    this.handle = await open(join(this.out, file), "wx");
    this.sessions.clear(); this.newest = -Infinity; this.oldest = Infinity;
    return this.current = { file, sessions: 0, records: 0, bytes: 0, newest_time: null, oldest_time: null };
  }
  private async rotate(): Promise<void> {
    if (!this.handle || !this.current) return;
    await this.handle.close();
    const shard = { ...this.current, newest_time: new Date(this.newest).toISOString(), oldest_time: new Date(this.oldest).toISOString() };
    this.shards.push(shard);
    console.error(JSON.stringify({ event: "shard_written", ...shard }));
    this.handle = null; this.current = null;
  }
  async addSession(session: string, records: Loaded[]): Promise<void> {
    const lines = records.map(r => JSON.stringify(r.params) + "\n"), sizes = lines.map(line => Buffer.byteLength(line));
    const total = sizes.reduce((sum, size) => sum + size, 0);
    // A session that no longer fits beside earlier sessions moves whole to the next shard.
    if (this.current && this.current.bytes + total > this.limit) await this.rotate();
    let start = 0, pieces = 0;
    while (start < lines.length) {
      const shard = this.current ?? await this.open();
      let end = start, size = 0;
      // Only a session larger than the limit stops early here; a single line always enters an empty shard.
      while (end < lines.length && (shard.bytes + size + sizes[end]! <= this.limit || (shard.bytes === 0 && end === start))) size += sizes[end++]!;
      if (end === start) { await this.rotate(); continue; }
      await this.handle!.write(lines.slice(start, end).join(""));
      for (const { time } of records.slice(start, end)) { if (time > this.newest) this.newest = time; if (time < this.oldest) this.oldest = time; }
      this.sessions.add(session);
      shard.sessions = this.sessions.size; shard.records += end - start; shard.bytes += size;
      start = end; pieces++;
    }
    if (pieces > 1) this.split.push(session);
  }
  async close(): Promise<ShardEntry[]> { await this.rotate(); return this.shards; }
}

export interface ConvertOptions { root: string; out: string; limitSessions?: number; since?: string; shardBytes?: number; }
export interface Manifest {
  source: string; root: string; out: string; generated_at: string;
  sessions: number; files: number; records: number; shards: ShardEntry[]; split_sessions: string[]; skipped: Skipped;
}
/** `since` selects whole sessions whose newest emitted record is at or after it; `limitSessions` keeps the newest N. */
export async function convertVault(options: ConvertOptions): Promise<Manifest> {
  const root = resolve(options.root), out = resolve(options.out), limit = options.shardBytes ?? SHARD_MAX_BYTES;
  if (!Number.isInteger(limit) || limit < 1 || limit > SOURCE_MAX_BYTES) throw new Error("vault_shard_bytes_invalid");
  const since = options.since === undefined ? null : Date.parse(options.since);
  if (since !== null && !Number.isFinite(since)) throw new Error("vault_since_invalid");
  await mkdir(out, { recursive: true });
  // Stale shards from an earlier run would be ingested alongside the new ones.
  if ((await readdir(out)).length) throw new Error(`vault_out_not_empty: ${out}`);
  const index: { session: string; newest: number | null }[] = [];
  for await (const entry of await opendir(root)) {
    if (!entry.isDirectory() || entry.name.startsWith(".")) continue;
    const { records } = await loadSession(join(root, entry.name));
    let newest: number | null = null;
    for (const { time } of records) if (newest === null || time > newest) newest = time;
    if (since !== null && (newest === null || newest < since)) continue;
    index.push({ session: entry.name, newest });
  }
  // Newest first; sessions that emit nothing sort last so their skip counts are still reported.
  index.sort((a, b) => (b.newest ?? -Infinity) - (a.newest ?? -Infinity) || compare(a.session, b.session));
  const selected = options.limitSessions === undefined ? index : index.slice(0, options.limitSessions);
  const writer = new ShardWriter(out, limit), skipped = noSkips();
  let files = 0, records = 0;
  for (const { session } of selected) {
    const load = await loadSession(join(root, session));
    for (const key of Object.keys(skipped) as (keyof Skipped)[]) skipped[key] += load.skipped[key];
    files += load.files; records += load.records.length;
    if (load.records.length) await writer.addSession(session, load.records);
  }
  const shards = await writer.close();
  const manifest: Manifest = { source: basename(root), root, out, generated_at: new Date().toISOString(), sessions: selected.length, files, records, shards, split_sessions: writer.split, skipped };
  await writeFile(join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  return manifest;
}

// Not `import.meta.main`: bun's node-target bundle rewrites it into an undefined `__require`, so dist/ would crash.
const invokedDirectly = process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) {
  const usage = "usage: bun scripts/ops/vault-to-snapshots.ts --root <vault source dir> --out <dir> [--limit-sessions N] [--since ISO]";
  const { values } = parseArgs({ options: { root: { type: "string" }, out: { type: "string" }, "limit-sessions": { type: "string" }, since: { type: "string" } }, strict: true });
  if (!values.root || !values.out) { console.error(usage); process.exit(2); }
  const limitSessions = values["limit-sessions"] === undefined ? undefined : Number(values["limit-sessions"]);
  if (limitSessions !== undefined && (!Number.isInteger(limitSessions) || limitSessions < 0)) { console.error(`--limit-sessions must be a non-negative integer\n${usage}`); process.exit(2); }
  const manifest = await convertVault({ root: values.root, out: values.out, ...(limitSessions === undefined ? {} : { limitSessions }), ...(values.since === undefined ? {} : { since: values.since }) });
  const { shards, ...summary } = manifest;
  console.log(JSON.stringify({ event: "vault_snapshots", ...summary, shards: shards.length, bytes: shards.reduce((sum, shard) => sum + shard.bytes, 0), manifest: join(manifest.out, "manifest.json") }));
}
