import { notionEpisode } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, utimes, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { revisionOf } from "../record.ts";
import { notionFixture } from "./notion.fixture.ts";
import { notionAdapter } from "./notion.ts";

test("notion preserves backfill origin and day timestamp with full unmasked documents", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-notion-"));
  try {
    const directory = join(root, "Workspace", "nested");
    await mkdir(directory, { recursive: true });
    const page = join(directory, "Page.md");
    const empty = join(root, "Workspace", "Empty.md");
    await writeFile(page, notionFixture.page);
    await writeFile(empty, notionFixture.empty);
    await utimes(page, notionFixture.mtime, notionFixture.mtime);
    await utimes(empty, notionFixture.mtime, notionFixture.mtime);
    expect(await Array.fromAsync(notionAdapter.files(root))).toEqual([empty, page]);
    for (const path of [page, empty]) {
      const raw = (await readFile(path)).toString();
      const episode = notionEpisode(root, path, raw, notionFixture.mtime).input;
      const [session] = await notionAdapter.parse(path, await readFile(path));
      const record = session?.records[0];
      expect(session?.sessionId).toBe("Workspace");
      expect(record?.origin).toEqual(episode.origin);
      expect(record?.occurred_at).toBe(episode.time?.value);
      expect(record?.time_precision).toBe("day");
      expect(record?.text).toBe(raw);
      expect(record?.revision).toBe(revisionOf(record?.occurred_at ?? "", raw));
      expect([record?.role, record?.kind, record?.canonical_kind, record?.line]).toEqual(["document", "document", "other", 0]);
    }
    const record = (await notionAdapter.parse(page, await readFile(page)))[0]?.records[0];
    expect(record?.text).toContain("secret: abcdefghijklmnopqrstuvwxyz");
    expect(record?.text.length).toBeGreaterThan(4000);
    expect(notionEpisode(root, empty, "", notionFixture.mtime).input.content).toBe("Empty");
    expect((await notionAdapter.parse(empty, await readFile(empty)))[0]?.records[0]?.text).toBe("");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
