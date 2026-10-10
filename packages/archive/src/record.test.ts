import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { rawEpisode } from "@anamnesis/backfill";
import { copyRaw } from "./raw.ts";
import { ArchiveRecord, revisionOf } from "./record.ts";

test("revisionOf matches backfill rawEpisode for unmasked text and normalized time", () => {
  const occurred = "2026-10-10T12:00:00Z";
  const text = "hello\nworld";
  const episode = rawEpisode(occurred, text, {
    source: "fixture", session: "s", actor: "user", record: "r",
  }, {});
  const record = ArchiveRecord.parse({
    origin: { source: "fixture", session: "s", actor: "user", record: "r" },
    seq: 0, role: "user", occurred_at: "2026-10-10T12:00:00.000Z",
    time_precision: "second", text, kind: "message", canonical_kind: "message",
    revision: revisionOf("2026-10-10T12:00:00.000Z", text),
    file: { sha256: createHash("sha256").update(text).digest("hex"), path: "/fixture", line: 1 },
  });
  expect(record.origin).toEqual(episode.input.origin);
  expect(episode.input.time?.value).toBe(record.occurred_at);
  expect(episode.input.source_revision).toBe(record.revision);
});

test("copyRaw stores equal bytes once and distinct bytes separately", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-raw-"));
  try {
    const source = join(root, "input");
    await writeFile(source, "first");
    const first = await copyRaw(source, root);
    const second = await copyRaw(source, root);
    expect(second).toBe(first);
    expect(await readdir(join(root, "raw", first.slice(0, 2)))).toEqual([first]);
    await writeFile(source, "other");
    const third = await copyRaw(source, root);
    expect(third).toBe(createHash("sha256").update("other").digest("hex"));
    expect(third).not.toBe(first);
    expect(await readFile(join(root, "raw", third.slice(0, 2), third), "utf8")).toBe("other");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
