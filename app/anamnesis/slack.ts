import { logEvent } from "./log.ts";
import { createHash } from "node:crypto";
import { lstat, opendir } from "node:fs/promises";
import { basename, join, relative, resolve } from "node:path";
import { RPC_LIMITS, RpcRememberParams } from "@anamnesis/protocol";
import { isSlackSlop, parseSlackMessage, slackEpisode } from "@anamnesis/backfill";
import { fingerprint, sha, textLines } from "./source-files.ts";
import { ingestSnapshot, type SourceRecord } from "./source.ts";
import { RpcClient } from "./client.ts";
import { Revisions } from "./raw-lane.ts";

const MAX_FILES = 2048;
const MAX_ENTRIES = 8192;
const MAX_INDEX_BYTES = 4 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 256 * 1024 * 1024;
const unicode = /^(?:[^\uD800-\uDFFF]|[\uD800-\uDBFF][\uDC00-\uDFFF])*$/;
async function fileInfo(path: string) {
  const info = await lstat(path, { bigint: true });
  if (info.isSymbolicLink()) throw new Error(`source_symlink: ${path}`);
  return info;
}
interface Tree { files: string[]; fingerprints: Map<string, string>; }
/** index.jsonl first, then every channels/ and threads/ export in code-unit order; the two directories are fingerprinted. */
async function listExportFiles(root: string, fingerprints: Map<string, string>): Promise<string[]> {
  const files = ["index.jsonl"];
  let entries = 0;
  for (const dir of ["channels", "threads"]) {
    const path = join(root, dir), info = await fileInfo(path);
    if (!info.isDirectory()) throw new Error(`source_not_directory: ${dir}`);
    fingerprints.set(dir, fingerprint(info));
    const names: string[] = [];
    for await (const entry of await opendir(path)) {
      if (++entries > MAX_ENTRIES) throw new Error("source_entry_limit");
      if (entry.name.endsWith(".jsonl") && !entry.name.startsWith("._")) {
        if (files.length + names.length >= MAX_FILES) throw new Error("source_file_limit");
        names.push(entry.name);
      }
    }
    files.push(...names.sort().map(name => join(dir, name)));
  }
  return files;
}
async function sourceFiles(root: string, checkpoint: string): Promise<Tree> {
  if (!(await fileInfo(root)).isDirectory()) throw new Error("source_not_directory");
  const fingerprints = new Map<string, string>();
  const files = await listExportFiles(root, fingerprints);
  let bytes = 0;
  for (const name of files) {
    const path = join(root, name);
    if ([checkpoint, checkpoint + ".pending.json"].some(cp => resolve(cp) === path)) throw new Error("source_checkpoint_path_conflict");
    const info = await fileInfo(path);
    if (!info.isFile()) throw new Error(`source_not_regular_file: ${name}`);
    if (name === "index.jsonl" && info.size > BigInt(MAX_INDEX_BYTES)) throw new Error("source_index_too_large");
    bytes += Number(info.size);
    if (bytes > MAX_SNAPSHOT_BYTES) throw new Error("source_snapshot_too_large");
    fingerprints.set(name, fingerprint(info));
  }
  return { files, fingerprints };
}

// Fixed byte buffers bound a physical line BEFORE UTF-8 decoding or JSON.parse.
// O_NOFOLLOW/O_NONBLOCK also reject a substituted symlink/FIFO without hanging.
function channelId(path: string): string {
  const name = basename(path, ".jsonl");
  return path.startsWith("channels/") ? name : name.split("-")[0]!;
}

/** Revision admission; a rejected re-parse (occurrence suffix or predecessor key) is an invalid record, chain errors pass through. */
function admitSlack(revisions: Revisions, base: RpcRememberParams, at: string): { params: RpcRememberParams; native: string } {
  try { return revisions.admit(base, false, at); }
  catch (cause) { if (!(cause instanceof Error && cause.name === "ZodError")) throw cause; throw new Error(`source_invalid_record: ${at}`, { cause }); }
}
/** One export line as remember params, with the oversized body kept aside; null for slop and empty turns. Throws on malformed input. */
function slackRecord(text: string, channel: string, channelName: string): { base: RpcRememberParams; body: Buffer | undefined } | null {
  const parsed = parseSlackMessage(text);
  if (isSlackSlop(parsed)) return null;
  const { input } = slackEpisode(parsed, channel, channelName);
  if (!input.content) return null;
  if (!unicode.test(input.content)) throw new Error("malformed Unicode");
  let content = input.content, body: Buffer | undefined;
  if (Buffer.byteLength(content) > RPC_LIMITS.content_bytes) {
    body = Buffer.from(content);
    let end = RPC_LIMITS.content_bytes;
    while ((body[end]! & 0xc0) === 0x80) end--;
    content = body.subarray(0, end).toString("utf8");
  }
  const base = RpcRememberParams.parse({ episode: { schema: input.schema, time: input.time, content, origin: input.origin, properties: input.properties }, source_revision: input.source_revision, expected_previous_revision_key: null, ...(body ? { payload_hash: sha(body) } : {}) });
  return { base, body };
}

/** Producer-sealed export only. Physical file/line order defines observed
 * revisions, not inferred event history; the manifest is this snapshot's anchor.
 * Validate the entire export (including revision mapping) before any delivery,
 * retaining only bounded metadata, then replay the same bounded parser. */
export async function ingestSlack(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  root = resolve(root);
  const cp = relative(root, resolve(checkpoint));
  if (["channels", "threads"].some(dir => cp === dir || cp.startsWith(dir + "/"))) throw new Error("source_checkpoint_path_conflict");
  await ingestSnapshot(checkpoint, client, async () => {
    const initial = await sourceFiles(root, checkpoint);
    const assertUnchanged = async () => {
      const now = await sourceFiles(root, checkpoint);
      if (JSON.stringify(now.files) !== JSON.stringify(initial.files) || JSON.stringify([...now.fingerprints]) !== JSON.stringify([...initial.fingerprints])) throw new Error("source_changed");
    };
    const names = new Map<string, string>();
    const manifest: { file: string; sha256: string }[] = [];
    const indexHash = createHash("sha256");
    for await (const { text, line } of textLines(root, "index.jsonl", initial.fingerprints.get("index.jsonl")!, indexHash, { maxLineBytes: RPC_LIMITS.properties_bytes, ignoreBOM: true })) {
      if (!text.trim()) continue;
      try {
        const value: unknown = JSON.parse(text);
        if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("invalid channel");
        const { id, name } = value as Record<string, unknown>;
        for (const v of [id, name]) if (typeof v !== "string" || !v || !unicode.test(v) || Buffer.byteLength(v) > RPC_LIMITS.identifier_bytes) throw new Error("invalid channel metadata");
        if (names.size >= MAX_ENTRIES && !names.has(id as string)) throw new Error("channel limit");
        names.set(id as string, name as string);
      } catch (cause) { throw new Error(`source_invalid_index: index.jsonl:${line}`, { cause }); }
    }
    manifest.push({ file: "index.jsonl", sha256: indexHash.digest("hex") });
    async function* records(validating = false): AsyncGenerator<SourceRecord> {
      const revisions = new Revisions();
      for (const name of initial.files.slice(1)) {
        const channel = channelId(name), hash = createHash("sha256");
        for await (const { text, line } of textLines(root, name, initial.fingerprints.get(name)!, hash, { ignoreBOM: true })) {
          if (!text.trim()) continue;
          let admitted: { base: RpcRememberParams; body: Buffer | undefined } | null;
          try { admitted = slackRecord(text, channel, names.get(channel) ?? channel); }
          catch (cause) { throw new Error(`source_invalid_record: ${name}:${line}`, { cause }); }
          if (!admitted) continue;
          const { base, body } = admitted;
          const { params, native } = admitSlack(revisions, base, `${name}:${line}`);
          yield { params, context: { file: name, line, native_source_revision: native }, ...(body ? { payload: { bytes_b64: body.toString("base64"), media_type: "text/plain" } } : {}) };
        }
        const digest = hash.digest("hex");
        if (validating) manifest.push({ file: name, sha256: digest });
        else if (digest !== manifest.find(entry => entry.file === name)!.sha256) throw new Error("source_changed");
      }
    }
    for await (const _record of records(true)) { /* complete bounded preflight */ }
    await assertUnchanged();
    const sourceHash = sha(JSON.stringify({ format: "slack-export-snapshot/1", manifest }));
    return { sourceHash, records: records(), assertUnchanged };
  });
  logEvent("info", "source_scope", { source: "slack", snapshot: "complete", tail: "incomplete", rotation: "incomplete" });
}
