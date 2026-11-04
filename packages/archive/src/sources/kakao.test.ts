import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { revisionOf } from "../record.ts";
import { kakaoFixture } from "./kakao.fixture.ts";
import { kakaoAdapter } from "./kakao.ts";

test("kakao keeps every row verbatim as one session per chat with physical lines", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-kakao-"));
  try {
    await mkdir(join(root, "nested"));
    const path = join(root, "nested", "rooms.jsonl");
    await writeFile(path, kakaoFixture.map((row) => JSON.stringify(row)).join("\n") + "\n");
    await writeFile(join(root, "notes.txt"), "ignored");
    expect(await Array.fromAsync(kakaoAdapter.files(root))).toEqual([path]);
    const sessions = await kakaoAdapter.parse(path, await readFile(path));
    expect(sessions.map((session) => [session.sessionId, session.records.length])).toEqual([["100", 3], ["200", 1]]);
    const [first] = sessions;
    expect(first?.records.map((record) => [record.line, record.origin.actor, record.kind, record.canonical_kind])).toEqual([
      [1, "7", "text", "message"], [2, "9", "type_2", "other"], [3, "0", "type_0", "system-event"],
    ]);
    for (const row of kakaoFixture) {
      const record = sessions.flatMap((session) => session.records).find((item) => item.origin.record === row.message_id);
      expect(record?.origin).toEqual({ source: "kakao", session: row.chat_id, actor: row.sender_id, record: row.message_id });
      expect(record?.occurred_at).toBe(new Date(row.timestamp).toISOString());
      expect(record?.text).toBe(row.text);
      expect(record?.role).toBe("user");
      expect(record?.revision).toBe(revisionOf(record?.occurred_at ?? "", row.text));
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("kakao reads a sqlite3 -json array export and skips rows without identity", async () => {
  const rows = [...kakaoFixture, { chat_id: "300", text: "no message id" }, { message_id: "x", chat_id: "300", timestamp: "not a date" }];
  const sessions = await kakaoAdapter.parse("/rooms.json", new TextEncoder().encode(JSON.stringify(rows)));
  expect(sessions.map((session) => session.sessionId)).toEqual(["100", "200"]);
  expect(sessions.flatMap((session) => session.records).every((record) => record.line === 0)).toBe(true);
});
