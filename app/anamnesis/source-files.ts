import { createHash, type Hash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, opendir } from "node:fs/promises";
import { join, relative } from "node:path";

// Shared by every offline export lane: identity of a sealed source file and
// the bounded line reader that pins it while it is read.
const MAX_LINE_BYTES = 8 * 1024 * 1024;
export const sha = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
export const fingerprint = (info: Awaited<ReturnType<typeof fileInfo>>) => [info.dev, info.ino, info.size, info.mtimeNs, info.ctimeNs, info.mode].join(":");
export async function fileInfo(path: string) {
  const info = await lstat(path, { bigint: true });
  if (info.isSymbolicLink()) throw new Error(`source_symlink: ${path}`);
  if ((info.mode & 0o444n) === 0n || (info.isDirectory() && (info.mode & 0o111n) === 0n)) throw new Error(`source_permission: ${path}`);
  return info;
}
function assertUtf8(decoder: TextDecoder, record: Uint8Array, at: string): void {
  try { decoder.decode(record); } catch (cause) { throw new Error(`source_invalid_utf8: ${at}`, { cause }); }
}
/** Fixed buffers bound bytes BEFORE decoding or JSON parsing. An open descriptor
 * pins the file; both descriptor and tree identities detect replacement/growth. */
export interface LineOptions { maxLineBytes?: number; ignoreBOM?: boolean }
export async function* lines(root: string, name: string, expected: string, hash?: Hash, options: LineOptions = {}): AsyncGenerator<{ bytes: Uint8Array; line: number }> {
  const max = options.maxLineBytes ?? MAX_LINE_BYTES;
  const file = await open(join(root, name), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await file.stat({ bigint: true });
    if (!info.isFile() || fingerprint(info) !== expected) throw new Error("source_changed");
    const buffer = Buffer.allocUnsafe(max), chunk = Buffer.allocUnsafe(64 * 1024);
    const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: options.ignoreBOM ?? false });
    let length = 0, line = 1, total = 0;
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
        bytes.copy(buffer, length, start, end); length += end - start;
        if (newline < 0) break;
        const record = buffer.subarray(0, length);
        assertUtf8(decoder, record, `${name}:${line}`);
        yield { bytes: record, line };
        line++; length = 0; start = newline + 1;
      }
    }
    if (length) throw new Error(`source_partial_final_line: ${name}:${line}`);
    if (total !== Number(info.size) || fingerprint(await file.stat({ bigint: true })) !== expected) throw new Error("source_changed");
  } finally { await file.close(); }
}
/** The same reader for lanes that parse decoded text per line. */
export async function* textLines(root: string, name: string, expected: string, hash?: Hash, options: LineOptions = {}): AsyncGenerator<{ text: string; line: number }> {
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: options.ignoreBOM ?? false });
  for await (const { bytes, line } of lines(root, name, expected, hash, options)) yield { text: decoder.decode(bytes), line };
}

export type FileInfo = Awaited<ReturnType<typeof fileInfo>>;
export interface WalkBounds { maxDepth: number; maxEntries: number }
/** Bounded depth-first walk in code-unit name order: fingerprints every directory, skips AppleDouble
 * entries into `fingerprints`, and hands every non-directory entry to `onFile`. Limits throw source_depth_limit /
 * source_entry_limit; a non-directory root or subtree entry throws source_not_directory. */
export async function walkTree(root: string, bounds: WalkBounds, fingerprints: Map<string, string>, onFile: (path: string, local: string, info: FileInfo, name: string) => Promise<void> | void): Promise<void> {
  let entries = 0;
  async function walk(directory: string, depth: number): Promise<void> {
    if (depth > bounds.maxDepth) throw new Error("source_depth_limit");
    const info = await fileInfo(directory);
    if (!info.isDirectory()) throw new Error(`source_not_directory: ${directory}`);
    fingerprints.set(relative(root, directory), fingerprint(info));
    const names: string[] = [];
    for await (const entry of await opendir(directory)) {
      if (++entries > bounds.maxEntries) throw new Error("source_entry_limit");
      if (!entry.name.startsWith("._")) names.push(entry.name);
    }
    for (const name of names.sort()) {
      const path = join(directory, name), info = await fileInfo(path);
      if (info.isDirectory()) await walk(path, depth + 1);
      else await onFile(path, relative(root, path), info, name);
    }
  }
  await walk(root, 0);
}
