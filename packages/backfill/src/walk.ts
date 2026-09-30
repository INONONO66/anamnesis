import type { Dirent } from "node:fs";
import { readdir } from "node:fs/promises";
import { join } from "node:path";

/** Depth-first walk in locale name order; unreadable directories are empty and AppleDouble sidecars are skipped. */
export async function walkSorted(directory: string, visit: (entry: Dirent, path: string) => void): Promise<void> {
  const entries = await readdir(directory, { withFileTypes: true }).catch(() => []);
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name.startsWith("._")) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) await walkSorted(path, visit);
    else visit(entry, path);
  }
}
