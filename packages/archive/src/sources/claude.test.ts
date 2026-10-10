import { createClaudeRawParser } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { claudeFixture } from "./claude.fixture.ts";
import { claudeAdapter } from "./claude.ts";
import { revisionOf } from "../record.ts";

const path = "/home/x/.claude/projects/-work/resumed.jsonl";
const encoder = new TextEncoder();

test("claude preserves backfill identity time and revision while keeping what backfill drops", async () => {
  const parsed = await claudeAdapter.parse(path, encoder.encode(claudeFixture));
  const records = parsed.flatMap((session) => session.records);
  const parser = createClaudeRawParser({ path });
  const episodes = claudeFixture.trimEnd().split("\n").flatMap((line) => parser(encoder.encode(line)));
  expect(episodes.length).toBe(3);
  for (const episode of episodes) {
    const match = records.find((record) => record.origin.record === episode.input.origin.record);
    expect(match?.origin).toEqual(episode.input.origin);
    expect(match?.occurred_at).toBe(episode.input.time?.value);
    expect(match?.revision).toBe(episode.input.source_revision);
  }
  expect(parsed.map((session) => session.sessionId)).toEqual(["resumed"]);
  expect(records.map((record) => [record.origin.record, record.role, record.kind, record.canonical_kind, record.line])).toEqual([
    ["resumed:0", "system", "session", "other", 1],
    ["user-1:content:1", "user", "user", "message", 2],
    ["thinking:content:1", "assistant", "assistant", "message", 4],
    ["summary", "system", "summary", "compaction", 5],
    ["system", "system", "system", "system-event", 6],
    ["snapshot", "system", "file-history-snapshot", "other", 7],
    ["role-mismatch", "assistant", "user", "message", 8],
  ]);
  expect(records.some((record) => record.origin.record.startsWith("tool-only"))).toBe(false);
  expect(records[1]?.text).toBe("kept beside result");
  expect(records[2]?.reasoning).toBe("reason");
  expect(records[6]?.text).toBe("strict rejected");
  for (const record of records) expect(record.revision).toBe(revisionOf(record.occurred_at, record.text));
});

test("claude keeps thinking-field messages without prose", async () => {
  const line = JSON.stringify({ type: "assistant", sessionId: "thought-session", uuid: "thought", timestamp: 1000, message: {
    role: "assistant", content: [{ type: "thinking", thinking: "private thought", signature: "signed" }],
  } });
  const parsed = await claudeAdapter.parse(path, encoder.encode(`${line}\n`));
  expect(parsed[0]?.records.map((record) => [record.text, record.reasoning])).toEqual([["", "private thought"]]);
});

test("claude discovers transcripts from ~/.claude, ~/.claude/projects or a renamed copy of it, skipping derived and ._ copies", async () => {
  const { mkdtemp, mkdir, writeFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const root = await mkdtemp(join(tmpdir(), "archive-claude-"));
  try {
    const projects = join(root, "projects-snapshot");
    await mkdir(join(projects, "-work"), { recursive: true });
    await mkdir(join(root, "runtime", "transcripts", "s"), { recursive: true });
    await writeFile(join(projects, "-work", "b.jsonl"), claudeFixture);
    await writeFile(join(projects, "-work", "a.jsonl"), claudeFixture);
    await writeFile(join(projects, "-work", "._a.jsonl"), claudeFixture);
    await writeFile(join(root, "runtime", "transcripts", "s", "transcript.jsonl"), claudeFixture);
    const expected = [join(projects, "-work", "a.jsonl"), join(projects, "-work", "b.jsonl")];
    for (const base of [root, projects]) {
      const files: string[] = [];
      for await (const file of claudeAdapter.files(base)) files.push(file);
      expect(files).toEqual(expected);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
