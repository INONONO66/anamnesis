import { createOmoRawParser } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sessionFixture } from "./omo.fixture.ts";
import { omoAdapter } from "./omo.ts";
import { collect } from "../collect.ts";
import { Manifest } from "../manifest.ts";

test("omo preserves backfill identity time and revision while admitting filtered roles and events", async () => {
  const parsed = await omoAdapter.parse("/native.jsonl", new TextEncoder().encode(sessionFixture));
  const records = parsed.flatMap((session) => session.records);
  const episodes = sessionFixture.trimEnd().split("\n").flatMap(createOmoRawParser());
  for (const episode of episodes) {
    const match = records.find((record) => record.origin.record === episode.input.origin.record);
    expect(match?.origin).toEqual(episode.input.origin);
    expect(match?.occurred_at).toBe(episode.input.time?.value);
    expect(match?.revision).toBe(episode.input.source_revision);
  }
  expect(parsed.map((session) => session.sessionId)).toEqual(["native-session"]);
  expect(records.map((record) => [record.origin.record, record.origin.actor, record.role, record.canonical_kind])).toEqual([
    ["m-user", "user", "user", "message"],
    ["m-tool-text", "toolResult", "tool", "message"],
    ["m-system", "system", "system", "message"],
    ["m-developer", "developer", "developer", "message"],
    ["c-1", "unknown", "operator", "compaction"],
    ["x-1", "unknown", "operator", "system-event"],
  ]);
  expect(records[0]?.text).toBe("first\nsecond");
  expect(records[0]?.reasoning).toBe("reasoning");
  expect(records[1]?.text).toBe("tool output");
  expect(records[5]?.text).toBe(JSON.stringify(JSON.parse(sessionFixture.trimEnd().split("\n")[7] ?? "")));
  expect(records.map((record) => record.line)).toEqual([2, 4, 5, 6, 7, 8]);
});

test("omo keeps reasoning-only and whitespace-only message contents", async () => {
  const lines = [
    { type: "session", id: "reasoning-session" },
    { type: "message", id: "thinking", timestamp: "2026-10-10T12:00:00Z", message: { role: "assistant", content: [
      { type: "thinking", text: "thought" }, { type: "reasoning", text: "more" }, { type: "toolCall" },
    ] } },
    { type: "message", id: "spaces", timestamp: "2026-10-10T12:00:00Z", message: { role: "user", content: "  \n " } },
    { type: "message", id: "parts", timestamp: "2026-10-10T12:00:00Z", message: { role: "user", content: [
      { type: "text", text: " " }, { type: "toolResult" }, { type: "text", text: "\t" },
    ] } },
    { type: "message", id: "tool-only", timestamp: "2026-10-10T12:00:00Z", message: { role: "assistant", content: [{ type: "tool_use" }] } },
  ];
  const parsed = await omoAdapter.parse("/native.jsonl", new TextEncoder().encode(lines.map((line) => JSON.stringify(line)).join("\n")));
  expect(parsed[0]?.records.map((record) => [record.origin.record, record.text, record.reasoning])).toEqual([
    ["thinking", "", "thought\nmore"], ["spaces", "  \n ", undefined], ["parts", " \n\t", undefined],
  ]);
});

test("omo discovers header-bearing sessions and message sqlite without admitting artifact and derived copies", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-omo-"));
  try {
    const native = join(root, "agent", "sessions", "cwd", "date_id.jsonl");
    const artifact = join(root, "agent", "sessions", "cwd", "date_id-artifacts", "tool.jsonl");
    const derived = join(root, "runtime", "transcripts", "native-session", "transcript.jsonl");
    await mkdir(join(root, "agent", "sessions", "cwd", "date_id-artifacts"), { recursive: true });
    await mkdir(join(root, "runtime", "transcripts", "native-session"), { recursive: true });
    await writeFile(native, sessionFixture);
    await writeFile(artifact, '{"type":"toolResult","text":"artifact"}\n');
    await writeFile(derived, sessionFixture);
    const dbPath = join(root, "messages.db");
    const db = new Database(dbPath);
    db.run("CREATE TABLE messages (id TEXT, session_id TEXT, role TEXT, content TEXT, created_at TEXT)");
    db.run("INSERT INTO messages VALUES ('row', 'db-session', 'assistant', 'db text', '2026-04-05T06:07:08Z')");
    db.query("INSERT INTO messages VALUES (?, ?, ?, ?, ?)").run(
      "parts", "db-session", "system",
      JSON.stringify([{ type: "thinking", text: "db reasoning" }, { type: "text", text: "db content" }, { type: "toolCall" }]),
      "2026-04-05T06:07:09Z",
    );
    db.close();
    expect(await Array.fromAsync(omoAdapter.files(root))).toEqual([native, dbPath].sort((a, b) => a.localeCompare(b)));
    const session = await omoAdapter.parse(dbPath, await readFile(dbPath));
    expect(session[0]?.records[0]?.origin).toEqual({ source: "omo", session: "db-session", actor: "assistant", record: "row" });
    expect(session[0]?.records[1]?.reasoning).toBe("db reasoning");
    expect(session[0]?.records[1]?.role).toBe("system");
    expect(session[0]?.records.map((record) => record.line)).toEqual([0, 0]);
    const archiveDir = join(root, "archive-output");
    expect(await collect({ adapter: omoAdapter, root, archiveDir })).toEqual({ files: 2, sessions: 2, records: 8, skipped: 0 });
    const manifest = Manifest.open(archiveDir);
    try {
      expect(manifest.countsBySource()).toEqual([{ source: "omo", sessions: 2, records: 8 }]);
    } finally { manifest.close(); }
  } finally { await rm(root, { recursive: true, force: true }); }
});
