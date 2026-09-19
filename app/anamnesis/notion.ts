import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, opendir } from "node:fs/promises";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { notionEpisode } from "../../packages/backfill/src/notion.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { RpcClient } from "./client.ts";
import { ingestSnapshot, type SourceRecord } from "./source.ts";

const MAX_FILES = 1024;
const MAX_ENTRIES = 4096;
const MAX_DEPTH = 32;
const MAX_PAGE_BYTES = 8 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 256 * 1024 * 1024;
const sha = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const fingerprint = (info: Awaited<ReturnType<typeof fileInfo>>) => [info.dev, info.ino, info.size, info.mtimeNs, info.ctimeNs, info.mode].join(":");
async function fileInfo(path: string) {
  const info = await lstat(path, { bigint: true });
  if (info.isSymbolicLink()) throw new Error(`source_symlink: ${path}`);
  if ((info.mode & 0o444n) === 0n) throw new Error(`source_permission: ${path}`);
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
    // Only one bounded directory is sorted, never the export's episodes/time.
    for (const name of names.sort()) {
      const path = join(directory, name), info = await fileInfo(path);
      if (info.isDirectory()) await walk(path, depth + 1);
      else if (name.endsWith(".md")) {
        if (!info.isFile()) throw new Error(`source_not_regular_file: ${path}`);
        if (info.size > BigInt(MAX_PAGE_BYTES)) throw new Error(`source_record_too_large: ${path}`);
        bytes += Number(info.size);
        if (bytes > MAX_SNAPSHOT_BYTES) throw new Error("source_snapshot_too_large");
        if (files.length >= MAX_FILES) throw new Error("source_file_limit");
        files.push(relative(root, path)); fingerprints.set(relative(root, path), fingerprint(info));
      }
    }
  }
  await walk(root, 0);
  if (!files.length) throw new Error("source_no_export_files");
  return { files, fingerprints };
}
async function page(root: string, name: string, expected: string): Promise<{ record: SourceRecord; rawHash: string }> {
  const path = join(root, name);
  // Do not follow a substituted symlink or block on a substituted FIFO.
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await file.stat({ bigint: true });
    if (!info.isFile() || fingerprint(info) !== expected) throw new Error("source_changed");
    const bytes = Buffer.allocUnsafe(Number(info.size) + 1);
    let length = 0;
    while (length < bytes.length) {
      const { bytesRead } = await file.read(bytes, length, bytes.length - length, null);
      if (!bytesRead) break;
      length += bytesRead;
    }
    if (length !== Number(info.size) || fingerprint(await file.stat({ bigint: true })) !== expected) throw new Error("source_changed");
    const body = bytes.subarray(0, length);
    if (length && body[length - 1] !== 10) throw new Error(`source_partial_final_line: ${name}`);
    let raw: string;
    try { raw = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(body); }
    catch (cause) { throw new Error(`source_invalid_utf8: ${name}`, { cause }); }
    if (raw.includes("\0")) throw new Error(`source_malformed_markdown: ${name}`);
    const { input } = notionEpisode(root, path, raw, new Date(Number(info.mtimeMs)));
    const payload = Buffer.from(input.payload!);
    // A sealed export contains one observed revision per unique page path.
    // There is no observed predecessor or A-B-A history to manufacture.
    const params = RpcRememberParams.parse({
      episode: { schema: input.schema, time: input.time, content: input.content, origin: input.origin, properties: input.properties },
      source_revision: input.source_revision, expected_previous_revision_key: null, payload_hash: sha(payload),
    });
    return { rawHash: sha(body), record: { params,
      payload: { bytes_b64: payload.toString("base64"), media_type: input.payload_media_type! },
      context: { file: name, line: 1, native_source_revision: input.source_revision! },
    } };
  } finally { await file.close(); }
}

/** Offline, producer-sealed Markdown export only. Newline termination is an
 * admission rule, not proof of producer completion. The caller must stop the
 * exporter before invoking this command; undetectable pre-snapshot loss cannot
 * be reconstructed. No live tail, watcher, rotation or inferred page history. */
export async function ingestNotion(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  root = resolve(root);
  const checkpointRelative = relative(root, resolve(checkpoint));
  if (!isAbsolute(checkpointRelative) && checkpointRelative !== ".." && !checkpointRelative.startsWith(".." + sep)) throw new Error("source_checkpoint_path_conflict");
  await ingestSnapshot(checkpoint, client, async () => {
    const initial = await tree(root);
    const assertUnchanged = async () => {
      const current = await tree(root);
      if (JSON.stringify(current.files) !== JSON.stringify(initial.files) || JSON.stringify([...current.fingerprints]) !== JSON.stringify([...initial.fingerprints])) throw new Error("source_changed");
    };
    const manifest: { file: string; sha256: string }[] = [];
    // Validate every page before any RPC; retain only finite metadata, not bodies.
    for (const name of initial.files) {
      const parsed = await page(root, name, initial.fingerprints.get(name)!);
      manifest.push({ file: name, sha256: parsed.rawHash });
    }
    await assertUnchanged();
    // Include filesystem identity and mtime: time is part of the delivered body,
    // and a same-byte replacement is still rotation, not this sealed snapshot.
    const sourceHash = sha(JSON.stringify({ format: "notion-markdown-snapshot/1", manifest, fingerprints: [...initial.fingerprints] }));
    async function* records(): AsyncGenerator<SourceRecord> {
      for (const entry of manifest) {
        const parsed = await page(root, entry.file, initial.fingerprints.get(entry.file)!);
        if (parsed.rawHash !== entry.sha256) throw new Error("source_changed");
        yield parsed.record;
      }
    }
    return { sourceHash, records: records(), assertUnchanged };
  });
  console.log(JSON.stringify({ event: "source_scope", source: "notion", snapshot: "complete", live_tail: "unsupported", rotation: "unsupported", producer_sealed: "required" }));
}
