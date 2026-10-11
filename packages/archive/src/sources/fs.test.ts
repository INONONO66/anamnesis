import { expect, test } from "bun:test";
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isFileOrAbsent, readdirOrEmpty } from "./fs.ts";
import { miscAdapter } from "./misc.ts";
import { omoAdapter } from "./omo.ts";

test("discovery ignores absent paths but propagates access failures", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-discovery-"));
  const locked = join(root, "locked");
  try {
    expect(await readdirOrEmpty(join(root, "missing"))).toEqual([]);
    expect(await isFileOrAbsent(join(root, "missing"))).toBe(false);
    if (process.getuid?.() === 0) return;
    await mkdir(locked);
    await writeFile(join(locked, "present"), "content");
    await chmod(locked, 0);
    await expect(readdirOrEmpty(locked)).rejects.toMatchObject({ code: "EACCES" });
    await expect(isFileOrAbsent(join(locked, "present"))).rejects.toMatchObject({ code: "EACCES" });
    await expect(Array.fromAsync(omoAdapter.files(locked))).rejects.toMatchObject({ code: "EACCES" });
    await expect(Array.fromAsync(miscAdapter.files(locked))).rejects.toMatchObject({ code: "EACCES" });
  } finally {
    await chmod(locked, 0o700).catch((error: unknown) => {
      if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) throw error;
    });
    await rm(root, { recursive: true, force: true });
  }
});
