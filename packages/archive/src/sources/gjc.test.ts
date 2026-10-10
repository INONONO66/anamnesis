import { createGjcRawParser } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gjcFixture } from "./gjc.fixture.ts";
import { gjcAdapter } from "./gjc.ts";

test("gjc preserves backfill event identity and time and includes filtered events", async () => {
  const records = (await gjcAdapter.parse("/gjc.jsonl", new TextEncoder().encode(gjcFixture))).flatMap((session) => session.records);
  const episodes = gjcFixture.trimEnd().split("\n").flatMap(createGjcRawParser());
  for (const episode of episodes) {
    const record = records.find((item) => item.origin.record === episode.input.origin.record);
    expect(record?.origin).toEqual(episode.input.origin);
    expect(record?.occurred_at).toBe(episode.input.time?.value);
    expect(record?.revision).toBe(episode.input.source_revision);
  }
  expect(records.map((record) => record.origin.record)).toEqual(["m-user", "m-tool-text", "m-system", "m-developer", "c-1", "x-1"]);
  expect(records[1]?.role).toBe("tool");
  expect(records[2]?.role).toBe("system");
  expect(records[4]?.canonical_kind).toBe("compaction");
});

test("gjc walks workspace and nested subagent sessions without reading tool logs", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-gjc-"));
  try {
    const path = join(root, "home", ".gjc", "agent", "sessions", "workspace", "sub", "session.jsonl");
    await mkdir(join(root, "home", ".gjc", "agent", "sessions", "workspace", "sub"), { recursive: true });
    await writeFile(path, gjcFixture);
    await writeFile(join(root, "home", ".gjc", "agent", "sessions", "workspace", "command.bash.log"), "tool output");
    expect(await Array.fromAsync(gjcAdapter.files(root))).toEqual([path]);
  } finally { await rm(root, { recursive: true, force: true }); }
});
