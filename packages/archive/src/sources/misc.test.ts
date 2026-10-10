import { createMiscRawParser } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, utimes, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { antigravityFixture, asideFixture, miscTime, opencodeFixture } from "./misc.fixture.ts";
import { miscAdapter } from "./misc.ts";

test("misc discovers three store layouts and preserves each backfill identity time revision", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-misc-"));
  try {
    const aside = join(root, "aside", "home", ".aside", "u", "0", "sessions", "2026-01-01_native", "messages.jsonl");
    const brain = join(root, "gemini-antigravity", "home", ".gemini", "antigravity-cli", "brain", "brain-id", ".system_generated", "logs", "transcript.jsonl");
    const opencode = join(root, "opencode", "home", ".local", "state", "opencode", "prompt-history.jsonl");
    for (const path of [aside, brain, opencode]) await mkdir(join(path, ".."), { recursive: true });
    await writeFile(aside, asideFixture);
    await writeFile(brain, antigravityFixture);
    await writeFile(opencode, opencodeFixture);
    await utimes(opencode, miscTime / 1000, miscTime / 1000);
    expect(await Array.fromAsync(miscAdapter.files(root))).toEqual([aside, brain, opencode].sort((a, b) => a.localeCompare(b)));
    for (const [path, fixture, context] of [
      [aside, asideFixture, { source: "aside", session: "native" }],
      [brain, antigravityFixture, { source: "gemini-antigravity", session: "brain-id" }],
      [opencode, opencodeFixture, { source: "opencode", occurredAt: miscTime }],
    ] as const) {
      const sessions = await miscAdapter.parse(path, await readFile(path));
      const records = sessions.flatMap((session) => session.records);
      const episodes = fixture.trimEnd().split("\n").flatMap(createMiscRawParser(context));
      for (const episode of episodes) {
        const record = records.find((item) => item.origin.record === episode.input.origin.record);
        expect(record?.origin).toEqual(episode.input.origin);
        expect(record?.occurred_at).toBe(episode.input.time?.value);
        expect(record?.revision).toBe(episode.input.source_revision);
      }
    }
    const asideRecords = (await miscAdapter.parse(aside, await readFile(aside)))[0]?.records;
    expect(asideRecords?.map((record) => [record.origin.actor, record.role, record.canonical_kind])).toEqual([
      ["user", "user", "message"], ["toolResult", "tool", "message"],
      ["system", "system", "message"], ["assistant", "assistant", "message"],
      ["system", "system", "compaction"],
    ]);
    expect(asideRecords?.[3]?.reasoning).toBe("reason");
    expect(asideRecords?.[1]?.text).toBe("tool output");
    const brainRecords = (await miscAdapter.parse(brain, await readFile(brain)))[0]?.records;
    expect(brainRecords?.map((record) => record.kind)).toEqual(["USER_INPUT", "PLANNER_RESPONSE", "RUN_COMMAND", "SYSTEM_MESSAGE", "compaction"]);
    expect(brainRecords?.[3]?.canonical_kind).toBe("system-event");
    expect(brainRecords?.[4]?.canonical_kind).toBe("compaction");
  } finally { await rm(root, { recursive: true, force: true }); }
});
