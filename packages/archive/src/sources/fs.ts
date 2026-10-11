import { readdir, stat } from "node:fs/promises";

function absent(error: unknown): boolean {
  return error instanceof Error && "code" in error && (error.code === "ENOENT" || error.code === "ENOTDIR");
}

export async function readdirOrEmpty(path: string): Promise<import("node:fs").Dirent[]> {
  try {
    return await readdir(path, { withFileTypes: true });
  } catch (error) {
    if (absent(error)) return [];
    throw error;
  }
}

export async function isFileOrAbsent(path: string): Promise<boolean> {
  try {
    return (await stat(path)).isFile();
  } catch (error) {
    if (absent(error)) return false;
    throw error;
  }
}
