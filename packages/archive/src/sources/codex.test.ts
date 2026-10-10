import { createCodexRawParser } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { codexFixture } from "./codex.fixture.ts";
import { codexAdapter } from "./codex.ts";
import { revisionOf } from "../record.ts";

const path = "/home/x/.codex/sessions/2026/03/09/rollout-2026-03-09T15-36-16-codex-session.jsonl";

test("codex preserves backfill identity and time while keeping what backfill drops", async () => {
  const parsed = await codexAdapter.parse(path, new TextEncoder().encode(codexFixture));
  const records = parsed.flatMap((session) => session.records);
  const parser = createCodexRawParser("codex-session");
  const episodes = codexFixture.trimEnd().split("\n").flatMap((line) => parser(line));
  expect(episodes.length).toBe(3);
  for (const episode of episodes) {
    const match = records.find((record) => record.origin.record === episode.input.origin.record);
    expect(match?.origin).toEqual(episode.input.origin);
    expect(match?.occurred_at).toBe(episode.input.time?.value);
  }
  expect(parsed.map((session) => session.sessionId)).toEqual(["codex-session"]);
  expect(records.map((record) => [record.origin.record, record.role, record.kind, record.canonical_kind, record.line])).toEqual([
    ["codex-session:content:0", "system", "session_meta", "system-event", 1],
    ["user-1:content:0", "user", "message", "message", 2],
    ["codex-session:3", "assistant", "message", "message", 4],
    ["codex-session:4", "system", "reasoning", "other", 5],
    ["codex-session:7", "system", "token_count", "system-event", 8],
    ["codex-session:8", "developer", "message", "message", 9],
    ["codex-session:9", "system", "context_compacted", "compaction", 10],
    ["codex-session:10", "system", "turn_context", "system-event", 11],
  ]);
  expect(records[1]?.text).toBe("kept beside result");
  expect(records[2]?.text).toBe("answer");
  expect(records[2]?.reasoning).toBe("think");
  expect(records[3]?.reasoning).toBe("internal reason");
  expect(records[6]?.text).toBe("context_compacted");
  for (const record of records) expect(record.revision).toBe(revisionOf(record.occurred_at, record.text));
});

test("codex discovers rollout files under the dated tree in sorted order", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-codex-"));
  try {
    await mkdir(join(root, "2026", "03", "09"), { recursive: true });
    await mkdir(join(root, "2026", "03", "08"), { recursive: true });
    await writeFile(join(root, "2026", "03", "09", "rollout-b.jsonl"), codexFixture);
    await writeFile(join(root, "2026", "03", "08", "rollout-a.jsonl"), codexFixture);
    await writeFile(join(root, "2026", "03", "09", "notes.jsonl"), codexFixture);
    const files: string[] = [];
    for await (const file of codexAdapter.files(root)) files.push(file);
    expect(files).toEqual([join(root, "2026", "03", "08", "rollout-a.jsonl"), join(root, "2026", "03", "09", "rollout-b.jsonl")]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
