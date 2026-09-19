import { createHash, type Hash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, opendir } from "node:fs/promises";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { createGjcRawParser } from "../../packages/backfill/src/gjcraw.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { RpcClient } from "./client.ts";
import { ingestSnapshot, sourceRevisionKey, type SourceRecord } from "./source.ts";

const MAX_FILES = 16_384;
const MAX_ENTRIES = 65_536;
const MAX_DEPTH = 64;
const MAX_LINE_BYTES = 8 * 1024 * 1024;
const MAX_FILE_BYTES = 256 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 4 * 1024 * 1024 * 1024;
const MAX_REVISIONS = 100_000;
const sha = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
function isGjcRollout(path: string): boolean {
  return path.endsWith(".jsonl") && path.includes("home/.gjc/agent/sessions/");
}
const fingerprint = (info: Awaited<ReturnType<typeof fileInfo>>) => [info.dev, info.ino, info.size, info.mtimeNs, info.ctimeNs, info.mode].join(":");
async function fileInfo(path: string) {
  const info = await lstat(path, { bigint: true });
  if (info.isSymbolicLink()) throw new Error(`source_symlink: ${path}`);
  if ((info.mode & 0o444n) === 0n || (info.isDirectory() && (info.mode & 0o111n) === 0n)) throw new Error(`source_permission: ${path}`);
  return info;
}
interface Tree { files: string[]; fingerprints: Map<string, string>; }
async function tree(root: string): Promise<Tree> {
  const files: string[] = [], fingerprints = new Map<string, string>();
  let entries = 0, bytes = 0;
  async function walk(directory: string, depth: number): Promise<void> {
    if (depth > MAX_DEPTH) throw new Error("source_depth_limit");
    const info = await fileInfo(directory);
    if (!info.isDirectory()) throw new Error(`source_not_directory: ${directory}`);
    fingerprints.set(relative(root, directory), fingerprint(info));
    const names: string[] = [];
    for await (const entry of await opendir(directory)) {
      if (++entries > MAX_ENTRIES) throw new Error("source_entry_limit");
      if (!entry.name.startsWith("._")) names.push(entry.name);
    }
    // Bounded depth-first code-unit file order; no export-wide episode sort.
    for (const name of names.sort()) {
      const path = join(directory, name), info = await fileInfo(path), local = relative(root, path);
      if (info.isDirectory()) await walk(path, depth + 1);
      else if (isGjcRollout(local)) {
        if (!info.isFile()) throw new Error(`source_not_regular_file: ${path}`);
        if (info.size > BigInt(MAX_FILE_BYTES)) throw new Error(`source_file_too_large: ${path}`);
        bytes += Number(info.size);
        if (bytes > MAX_SNAPSHOT_BYTES) throw new Error("source_snapshot_too_large");
        if (files.length >= MAX_FILES) throw new Error("source_file_limit");
        files.push(local); fingerprints.set(local, fingerprint(info));
      }
    }
  }
  await walk(root, 0);
  if (!files.length) throw new Error("source_no_export_files");
  return { files, fingerprints };
}

/** Fixed buffers bound bytes BEFORE decoding or JSON parsing. An open descriptor
 * pins the file; both descriptor and tree identities detect replacement/growth. */
async function* lines(root: string, name: string, expected: string, hash: Hash): AsyncGenerator<{ bytes: Uint8Array; line: number }> {
  const file = await open(join(root, name), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await file.stat({ bigint: true });
    if (!info.isFile() || fingerprint(info) !== expected) throw new Error("source_changed");
    const buffer = Buffer.allocUnsafe(MAX_LINE_BYTES), chunk = Buffer.allocUnsafe(64 * 1024);
    const decoder = new TextDecoder("utf-8", { fatal: true });
    let length = 0, line = 1, total = 0;
    while (true) {
      const { bytesRead } = await file.read(chunk, 0, chunk.length, null);
      if (!bytesRead) break;
      total += bytesRead;
      if (total > Number(info.size)) throw new Error("source_changed");
      const bytes = chunk.subarray(0, bytesRead); hash.update(bytes);
      let start = 0;
      while (start < bytes.length) {
        const newline = bytes.indexOf(10, start), end = newline < 0 ? bytes.length : newline;
        if (length + end - start > MAX_LINE_BYTES) throw new Error(`source_record_too_large: ${name}:${line}`);
        bytes.copy(buffer, length, start, end); length += end - start;
        if (newline < 0) break;
        const record = buffer.subarray(0, length);
        try { decoder.decode(record); } catch (cause) { throw new Error(`source_invalid_utf8: ${name}:${line}`, { cause }); }
        yield { bytes: record, line };
        line++; length = 0; start = newline + 1;
      }
    }
    if (length) throw new Error(`source_partial_final_line: ${name}:${line}`);
    if (total !== Number(info.size) || fingerprint(await file.stat({ bigint: true })) !== expected) throw new Error("source_changed");
  } finally { await file.close(); }
}

/** Caller-owned, offline, producer-sealed raw export only. LF termination is
 * required admission, NOT proof of completion. No live tail/rotation or inferred
 * missing history. Physical file/line/block order defines observed occurrences;
 * full validated replay reconstructs session headers and revision predecessors. */
export async function ingestGjcRaw(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  root = resolve(root);
  const cp = relative(root, resolve(checkpoint));
  if (!isAbsolute(cp) && cp !== ".." && !cp.startsWith(".." + sep)) throw new Error("source_checkpoint_path_conflict");
  await ingestSnapshot(checkpoint, client, async () => {
    const initial = await tree(root);
    const assertUnchanged = async () => {
      const now = await tree(root);
      if (JSON.stringify(now.files) !== JSON.stringify(initial.files) || JSON.stringify([...now.fingerprints]) !== JSON.stringify([...initial.fingerprints])) throw new Error("source_changed");
    };
    const manifest: { file: string; sha256: string }[] = [];
    async function* records(validating = false): AsyncGenerator<SourceRecord> {
      const heads = new Map<string, { native: string; revision: string; key: string; previous: string | null; signature: string }>();
      const seen = new Set<string>();
      let ordinal = 0;
      for (const name of initial.files) {
        const parse = createGjcRawParser();
        const hash = createHash("sha256");
        for await (const { bytes, line } of lines(root, name, initial.fingerprints.get(name)!, hash)) {
          let episodes;
          try { episodes = parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)); }
          catch (cause) { throw new Error(`source_invalid_record: ${name}:${line}`, { cause }); }
          for (const { input } of episodes) {
            ordinal++;
            const body = input.payload === undefined ? undefined : Buffer.from(input.payload);
            let base: RpcRememberParams;
            try {
              base = RpcRememberParams.parse({ episode: { schema: input.schema, time: input.time, content: input.content, origin: input.origin, properties: input.properties }, source_revision: input.source_revision, expected_previous_revision_key: null, ...(body ? { payload_hash: sha(body) } : {}) });
            } catch (cause) { throw new Error(`source_invalid_record: ${name}:${line}`, { cause }); }
            const o = base.episode.origin, origin = sha(JSON.stringify([o.source, o.session, o.actor, o.record]));
            const native = base.source_revision, signature = sha(JSON.stringify(base)), head = heads.get(origin), duplicate = head?.native === native;
            if (duplicate && head.signature !== signature) throw new Error(`source_revision_conflict: ${name}:${line}`);
            const seenKey = sha(JSON.stringify([origin, native]));
            const revision = duplicate ? head.revision : seen.has(seenKey) ? `${native}:occurrence:${ordinal}` : native;
            const params = RpcRememberParams.parse({ ...base, source_revision: revision, expected_previous_revision_key: duplicate ? head.previous : head?.key ?? null });
            if (!duplicate) {
              if (seen.size >= MAX_REVISIONS || heads.size >= MAX_REVISIONS) throw new Error("source_revision_limit");
              seen.add(seenKey);
              heads.set(origin, { native, revision, key: sourceRevisionKey(params), previous: params.expected_previous_revision_key, signature });
            }
            yield { params, context: { file: name, line, native_source_revision: native }, ...(body ? { payload: { bytes_b64: body.toString("base64"), media_type: input.payload_media_type! } } : {}) };
          }
        }
        const digest = hash.digest("hex");
        if (validating) manifest.push({ file: name, sha256: digest });
        else if (digest !== manifest.find(entry => entry.file === name)!.sha256) throw new Error("source_changed");
      }
    }
    // Complete validation (including occurrence/body mapping) before any RPC.
    // Retain only finite metadata; discard each parsed body immediately.
    for await (const _record of records(true)) { /* bounded preflight */ }
    await assertUnchanged();
    const sourceHash = sha(JSON.stringify({ format: "gjc-raw-snapshot/1", manifest, fingerprints: [...initial.fingerprints] }));
    return { sourceHash, records: records(), assertUnchanged };
  });
  console.log(JSON.stringify({ event: "source_scope", source: "gjc-raw", snapshot: "complete", live_tail: "unsupported", rotation: "unsupported", producer_sealed: "required" }));
}
