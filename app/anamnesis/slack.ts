import { createHash, type Hash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, opendir } from "node:fs/promises";
import { basename, join, relative, resolve } from "node:path";
import { RPC_LIMITS, RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { isSlackSlop, parseSlackMessage, slackEpisode } from "../../packages/backfill/src/slack.ts";
import { ingestSnapshot, sourceRevisionKey, type SourceRecord } from "./source.ts";
import { RpcClient } from "./client.ts";

const MAX_FILES = 2048;
const MAX_ENTRIES = 8192;
const MAX_REVISIONS = 100_000;
const MAX_LINE_BYTES = 8 * 1024 * 1024;
const MAX_INDEX_BYTES = 4 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 256 * 1024 * 1024;
const sha = (v: string | Uint8Array) => createHash("sha256").update(v).digest("hex");
const fingerprint = (s: Awaited<ReturnType<typeof fileInfo>>) => [s.dev, s.ino, s.size, s.mtimeNs, s.ctimeNs, s.mode].join(":");
const unicode = /^(?:[^\uD800-\uDFFF]|[\uD800-\uDBFF][\uDC00-\uDFFF])*$/;
async function fileInfo(path: string) {
  const info = await lstat(path, { bigint: true });
  if (info.isSymbolicLink()) throw new Error(`source_symlink: ${path}`);
  return info;
}
interface Tree { files: string[]; fingerprints: Map<string, string>; }
async function sourceFiles(root: string, checkpoint: string): Promise<Tree> {
  if (!(await fileInfo(root)).isDirectory()) throw new Error("source_not_directory");
  const files = ["index.jsonl"], fingerprints = new Map<string, string>();
  let entries = 0, bytes = 0;
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
async function* lines(root: string, name: string, expected: string, hash?: Hash): AsyncGenerator<{ text: string; line: number }> {
  const file = await open(join(root, name), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await file.stat({ bigint: true });
    if (!info.isFile() || fingerprint(info) !== expected) throw new Error("source_changed");
    const max = name === "index.jsonl" ? RPC_LIMITS.properties_bytes : MAX_LINE_BYTES;
    const lineBuffer = Buffer.allocUnsafe(max), chunk = Buffer.allocUnsafe(64 * 1024);
    let length = 0, line = 1, total = 0;
    const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
    while (true) {
      const { bytesRead } = await file.read(chunk, 0, chunk.length, null);
      if (!bytesRead) break;
      total += bytesRead;
      if (total > Number(info.size)) throw new Error("source_changed");
      const bytes = chunk.subarray(0, bytesRead); hash?.update(bytes);
      let start = 0;
      while (start < bytes.length) {
        const newline = bytes.indexOf(10, start), end = newline < 0 ? bytes.length : newline;
        if (length + end - start > max) throw new Error(`source_record_too_large: ${name}:${line}`);
        bytes.copy(lineBuffer, length, start, end); length += end - start;
        if (newline < 0) break;
        let text: string;
        try { text = decoder.decode(lineBuffer.subarray(0, length)); }
        catch (cause) { throw new Error(`source_invalid_utf8: ${name}:${line}`, { cause }); }
        yield { text, line };
        line++; length = 0; start = newline + 1;
      }
    }
    if (length) throw new Error(`source_partial_final_line: ${name}:${line}`);
    if (total !== Number(info.size) || fingerprint(await file.stat({ bigint: true })) !== expected) throw new Error("source_changed");
  } finally { await file.close(); }
}
function channelId(path: string): string {
  const name = basename(path, ".jsonl");
  return path.startsWith("channels/") ? name : name.split("-")[0]!;
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
    for await (const { text, line } of lines(root, "index.jsonl", initial.fingerprints.get("index.jsonl")!, indexHash)) {
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
      const heads = new Map<string, { native: string; revision: string; key: string; previous: string | null; signature: string }>();
      const seen = new Set<string>();
      let ordinal = 0;
      for (const name of initial.files.slice(1)) {
        const channel = channelId(name), hash = createHash("sha256");
        for await (const { text, line } of lines(root, name, initial.fingerprints.get(name)!, hash)) {
          if (!text.trim()) continue;
          let base: RpcRememberParams, body: Buffer | undefined;
          try {
            const parsed = parseSlackMessage(text);
            if (isSlackSlop(parsed)) continue;
            const { input } = slackEpisode(parsed, channel, names.get(channel) ?? channel);
            if (!input.content) continue;
            if (!unicode.test(input.content)) throw new Error("malformed Unicode");
            let content = input.content;
            if (Buffer.byteLength(content) > RPC_LIMITS.content_bytes) {
              body = Buffer.from(content);
              let end = RPC_LIMITS.content_bytes;
              while ((body[end]! & 0xc0) === 0x80) end--;
              content = body.subarray(0, end).toString("utf8");
            }
            base = RpcRememberParams.parse({ episode: { schema: input.schema, time: input.time, content, origin: input.origin, properties: input.properties }, source_revision: input.source_revision, expected_previous_revision_key: null, ...(body ? { payload_hash: sha(body) } : {}) });
          } catch (cause) { throw new Error(`source_invalid_record: ${name}:${line}`, { cause }); }
          ordinal++;
          const native = base.source_revision, origin = sha(JSON.stringify(base.episode.origin));
          const signature = sha(JSON.stringify(base)), head = heads.get(origin), duplicate = head?.native === native;
          if (duplicate && head.signature !== signature) throw new Error(`source_revision_conflict: ${name}:${line}`);
          const seenKey = sha(JSON.stringify([origin, native]));
          const revision = duplicate ? head.revision : seen.has(seenKey) ? `${native}:occurrence:${ordinal}` : native;
          let params: RpcRememberParams;
          try { params = RpcRememberParams.parse({ ...base, source_revision: revision, expected_previous_revision_key: duplicate ? head.previous : head?.key ?? null }); }
          catch (cause) { throw new Error(`source_invalid_record: ${name}:${line}`, { cause }); }
          if (!duplicate) {
            if (seen.size >= MAX_REVISIONS || heads.size >= MAX_REVISIONS) throw new Error("source_revision_limit");
            seen.add(seenKey);
            heads.set(origin, { native, revision, key: sourceRevisionKey(params), previous: params.expected_previous_revision_key, signature });
          }
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
  console.log(JSON.stringify({ event: "source_scope", source: "slack", snapshot: "complete", tail: "incomplete", rotation: "incomplete" }));
}
