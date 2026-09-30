import { logEvent } from "./log.ts";
import { createHash } from "node:crypto";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { asideSessionId, createMiscRawParser, type MiscRawContext } from "@anamnesis/backfill";
import { RpcRememberParams } from "@anamnesis/protocol";
import { RpcClient } from "./client.ts";
import { Revisions } from "./raw-lane.ts";
import { fingerprint, sha, textLines, walkTree } from "./source-files.ts";
import { ingestSnapshot, type SourceRecord } from "./source.ts";

const SEAL = "miscraw.snapshot.json";
const MAX_FILES = 16_384, MAX_ENTRIES = 65_536, MAX_DEPTH = 64;
const MAX_FILE_BYTES = 256 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 4 * 1024 * 1024 * 1024, MAX_INDEX_BYTES = 16 * 1024 * 1024;
const MAX_REVISIONS = 100_000;
const ASIDE = /^aside\/home\/\.aside\/u\/([^/]+)\/sessions\/([^/]+)\/messages\.jsonl$/;
const INDEX = /^aside\/home\/\.aside\/u\/([^/]+)\/sessions\.jsonl$/;
const ANTIGRAVITY = /^gemini-antigravity\/home\/\.gemini\/antigravity-cli\/brain\/([^/]+)\/\.system_generated\/logs\/transcript\.jsonl$/;
const OPENCODE = "opencode/home/.local/state/opencode/prompt-history.jsonl";
interface Tree { files: string[]; fingerprints: Map<string, string>; mtimes: Map<string, number>; }
async function tree(root: string): Promise<Tree> {
  const files: string[] = [], fingerprints = new Map<string, string>(), mtimes = new Map<string, number>();
  let bytes = 0, indexBytes = 0;
  await walkTree(root, { maxDepth: MAX_DEPTH, maxEntries: MAX_ENTRIES }, fingerprints, (path, local, info, name) => {
    if (!info.isFile()) throw new Error(`source_not_regular_file: ${path}`);
    // Do not open/copy databases or WAL. A JSONL export must be sealed by its
    // producer after a consistent read, including Aside's sessions index.
    if (/\.(db|sqlite|sqlite3)(-(wal|shm|journal))?$|-(wal|shm)$/.test(name)) throw new Error(`source_sqlite_unsupported: ${local}`);
    if (local !== SEAL && !ASIDE.test(local) && !INDEX.test(local) && !ANTIGRAVITY.test(local) && local !== OPENCODE) throw new Error(`source_format_unsupported: ${local}`);
    const limit = local === SEAL ? 1024 * 1024 : MAX_FILE_BYTES;
    if (info.size > BigInt(limit)) throw new Error(`source_file_too_large: ${local}`);
    bytes += Number(info.size);
    if (INDEX.test(local)) indexBytes += Number(info.size);
    if (bytes > MAX_SNAPSHOT_BYTES || indexBytes > MAX_INDEX_BYTES) throw new Error("source_snapshot_too_large");
    if (files.length >= MAX_FILES) throw new Error("source_file_limit");
    files.push(local); fingerprints.set(local, fingerprint(info)); mtimes.set(local, Number(info.mtimeNs / 1_000_000n));
  });
  if (!files.includes(SEAL)) throw new Error("source_producer_seal_required");
  if (!files.some(name => ASIDE.test(name) || ANTIGRAVITY.test(name) || name === OPENCODE)) throw new Error("source_no_export_files");
  return { files, fingerprints, mtimes };
}

function object(value: unknown): value is Record<string, unknown> { return value !== null && typeof value === "object" && !Array.isArray(value); }

/** One seal entry as [path, sha256]; the path must be a listed non-seal file, unique, and carry the opencode mtime. */
function sealEntry(entry: unknown, initial: Tree, manifest: Map<string, string>): [string, string] {
  if (!object(entry) || typeof entry["path"] !== "string" || typeof entry["sha256"] !== "string" || !/^[a-f0-9]{64}$/.test(entry["sha256"]) || Object.keys(entry).some(key => !["path", "sha256", "mtime_ms"].includes(key))) throw new Error("source_invalid_seal");
  const name = entry["path"];
  if (manifest.has(name) || name === SEAL || !initial.files.includes(name)) throw new Error("source_invalid_seal");
  if ((name === OPENCODE || entry["mtime_ms"] !== undefined) && entry["mtime_ms"] !== initial.mtimes.get(name)) throw new Error("source_mtime_mismatch");
  return [name, entry["sha256"]];
}
function laneContext(name: string, initial: Tree, index: Map<string, Record<string, string>>): MiscRawContext {
  const aside = ASIDE.exec(name), antigravity = ANTIGRAVITY.exec(name);
  if (aside) return { source: "aside", session: asideSessionId(aside[2]!), properties: index.get(JSON.stringify([aside[1], asideSessionId(aside[2]!)])) ?? {} };
  if (antigravity) return { source: "gemini-antigravity", session: antigravity[1]! };
  return { source: "opencode", occurredAt: initial.mtimes.get(name)! };
}

/** The producer seal: one JSON line naming every export file with its sha256 (and mtime for opencode); returns the manifest and the seal's own digest. */
async function readSeal(root: string, initial: Tree): Promise<{ manifest: Map<string, string>; sealHash: ReturnType<typeof createHash> }> {
  const sealHash = createHash("sha256"); let seal: unknown, sealLines = 0;
  for await (const { text } of textLines(root, SEAL, initial.fingerprints.get(SEAL)!, sealHash)) {
    if (++sealLines !== 1) throw new Error("source_invalid_seal");
    try { seal = JSON.parse(text); } catch (cause) { throw new Error("source_invalid_seal", { cause }); }
  }
  if (!object(seal) || seal["format"] !== "misc-raw-snapshot/1" || seal["sealed"] !== true || !Array.isArray(seal["files"]) || Object.keys(seal).some(key => !["format", "sealed", "files"].includes(key))) throw new Error("source_invalid_seal");
  const manifest = new Map<string, string>();
  for (const entry of seal["files"]) manifest.set(...sealEntry(entry, initial, manifest));
  if (manifest.size !== initial.files.length - 1) throw new Error("source_invalid_seal");
  for (const name of initial.files) {
    const aside = ASIDE.exec(name);
    if (aside && !manifest.has(`aside/home/.aside/u/${aside[1]}/sessions.jsonl`)) throw new Error("source_aside_index_required");
  }
  return { manifest, sealHash };
}
/** One sessions.jsonl row as [user+id key, session properties]; anything but id/title/cwd strings is invalid. */
function indexRow(text: string, user: string, at: string): [string, Record<string, string>] {
  let row: unknown;
  try { row = JSON.parse(text); } catch (cause) { throw new Error(`source_invalid_index: ${at}`, { cause }); }
  if (!object(row) || typeof row["id"] !== "string" || !row["id"] || Object.keys(row).some(key => !["id", "title", "cwd"].includes(key)) || [row["title"], row["cwd"]].some(value => value !== undefined && value !== null && typeof value !== "string")) throw new Error(`source_invalid_index: ${at}`);
  return [JSON.stringify([user, row["id"]]), { ...(row["title"] ? { session_title: row["title"] as string } : {}), ...(row["cwd"] ? { cwd: row["cwd"] as string } : {}) }];
}
type ParsedEpisode = ReturnType<ReturnType<typeof createMiscRawParser>>[number]["input"];
function episodeRecord(input: ParsedEpisode, at: string): { base: RpcRememberParams; body: Buffer | undefined } {
  const body = input.payload === undefined ? undefined : Buffer.from(input.payload);
  try { return { base: RpcRememberParams.parse({ episode: { schema: input.schema, time: input.time, content: input.content, origin: input.origin, properties: input.properties }, source_revision: input.source_revision, expected_previous_revision_key: null, ...(body ? { payload_hash: sha(body) } : {}) }), body }; }
  catch (cause) { throw new Error(`source_invalid_record: ${at}`, { cause }); }
}

/** Aside's per-user sessions index joined by user and session id, verified against the seal. */
async function readIndex(root: string, initial: Tree, manifest: Map<string, string>): Promise<Map<string, Record<string, string>>> {
  const index = new Map<string, Record<string, string>>();
  for (const name of initial.files.filter(name => INDEX.test(name))) {
    const hash = createHash("sha256"), user = INDEX.exec(name)![1]!;
    for await (const { text, line } of textLines(root, name, initial.fingerprints.get(name)!, hash)) {
      if (text.trim() === "") continue;
      const [key, properties] = indexRow(text, user, `${name}:${line}`);
      if (index.has(key)) throw new Error(`source_index_conflict: ${name}:${line}`);
      if (index.size >= MAX_REVISIONS) throw new Error("source_index_limit");
      index.set(key, properties);
    }
    if (hash.digest("hex") !== manifest.get(name)) throw new Error("source_seal_mismatch");
  }
  return index;
}

/** Offline subset only. Seal: one LF-terminated JSON object with format
 * misc-raw-snapshot/1, sealed:true, files:[{path,sha256,mtime_ms?}]. paths exactly
 * cover supported source/index files. OpenCode requires its producer mtime_ms.
 * Aside requires sessions.jsonl (id,title,cwd rows; empty if no index exists).
 * No live SQLite, protobuf, transcript_full, arbitrary normalized stores or
 * rotation; physical file/line order is observed revision occurrence order. */
export async function ingestMiscRaw(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  root = resolve(root);
  const cp = relative(root, resolve(checkpoint));
  if (!isAbsolute(cp) && cp !== ".." && !cp.startsWith(".." + sep)) throw new Error("source_checkpoint_path_conflict");
  await ingestSnapshot(checkpoint, client, async () => {
    const initial = await tree(root);
    const assertUnchanged = async () => {
      const now = await tree(root);
      if (JSON.stringify([...now.fingerprints]) !== JSON.stringify([...initial.fingerprints])) throw new Error("source_changed");
    };
    const { manifest, sealHash } = await readSeal(root, initial);
    const index = await readIndex(root, initial, manifest);
    async function* records(): AsyncGenerator<SourceRecord> {
      const revisions = new Revisions();
      for (const name of initial.files) {
        if (name === SEAL || INDEX.test(name)) continue;
        const context = laneContext(name, initial, index);
        const parse = createMiscRawParser(context, true), hash = createHash("sha256");
        for await (const { text, line } of textLines(root, name, initial.fingerprints.get(name)!, hash)) {
          let episodes;
          try { episodes = parse(text); } catch (cause) { throw new Error(`source_invalid_record: ${name}:${line}`, { cause }); }
          for (const { input } of episodes) {
            const { base, body } = episodeRecord(input, `${name}:${line}`);
            const { params, native } = revisions.admit(base, false, `${name}:${line}`);
            yield { params, context: { file: name, line, native_source_revision: native }, ...(body ? { payload: { bytes_b64: body.toString("base64"), media_type: input.payload_media_type! } } : {}) };
          }
        }
        if (hash.digest("hex") !== manifest.get(name)) throw new Error("source_seal_mismatch");
      }
    }
    // Full preflight before RPC, bounded bodies discarded; replay reconstructs
    // ordinals/index joins/occurrences and verifies the producer's byte hashes.
    for await (const _record of records()) { /* bounded validation */ }
    await assertUnchanged();
    return { sourceHash: sha(JSON.stringify({ format: "misc-raw-snapshot/1", seal: sealHash.digest("hex"), fingerprints: [...initial.fingerprints] })), records: records(), assertUnchanged };
  });
  logEvent("info", "source_scope", { source: "misc-raw", snapshot: "producer-sealed", sqlite: "unsupported", live_tail: "unsupported", rotation: "unsupported" });
}
