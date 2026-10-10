import { createHash, randomUUID } from "node:crypto";
import { readFile, mkdir, rename, stat, writeFile, unlink } from "node:fs/promises";
import { join } from "node:path";

export async function copyRaw(path: string, archiveDir: string, bytes?: Uint8Array): Promise<string> {
  const content = bytes ?? await readFile(path);
  const sha256 = createHash("sha256").update(content).digest("hex");
  const directory = join(archiveDir, "raw", sha256.slice(0, 2));
  const destination = join(directory, sha256);
  await mkdir(directory, { recursive: true });
  try {
    await stat(destination);
    return sha256;
  } catch (error) {
    if (!(error instanceof Error) || !("code" in error) || error.code !== "ENOENT") throw error;
  }
  const temporary = join(directory, `.${sha256}.${randomUUID()}`);
  await writeFile(temporary, content, { flag: "wx" });
  try {
    await rename(temporary, destination);
  } finally {
    await unlink(temporary).catch((error: unknown) => {
      if (!(error instanceof Error) || !("code" in error) || error.code !== "ENOENT") throw error;
    });
  }
  return sha256;
}
