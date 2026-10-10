import { createSessionParser, toEpisode } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { piFixture } from "./pi.fixture.ts";
import { piAdapter } from "./pi.ts";

test("pi keeps backfill native session identity time and revision as a superset", async () => {
  const records = (await piAdapter.parse("/pi.jsonl", new TextEncoder().encode(piFixture))).flatMap((session) => session.records);
  const episodes = piFixture.trimEnd().split("\n").flatMap(createSessionParser((event) => toEpisode(event, "pi", { kind: event.type })));
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

test("pi discovers only native sessions beneath its root", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-pi-"));
  try {
    const session = join(root, "agent", "sessions", "native.jsonl");
    await mkdir(join(root, "agent", "sessions", "native-artifacts"), { recursive: true });
    await writeFile(session, piFixture);
    await writeFile(join(root, "agent", "sessions", "native-artifacts", "tool.jsonl"), '{"type":"toolResult","text":"artifact"}');
    expect(await Array.fromAsync(piAdapter.files(root))).toEqual([session]);
  } finally { await rm(root, { recursive: true, force: true }); }
});
