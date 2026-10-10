import { isSlackSlop, parseSlackMessage, slackEpisode } from "@anamnesis/backfill";
import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { revisionOf } from "../record.ts";
import { slackFixture } from "./slack.fixture.ts";
import { slackAdapter } from "./slack.ts";

test("slack preserves backfill origin and time while keeping housekeeping and bot events", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-slack-"));
  try {
    const directory = join(root, "C1");
    await mkdir(directory);
    const path = join(directory, "2026-01-01.json");
    await writeFile(path, JSON.stringify(slackFixture));
    expect(await Array.fromAsync(slackAdapter.files(root))).toEqual([path]);
    const [session] = await slackAdapter.parse(path, await readFile(path));
    expect(session?.sessionId).toBe("C1");
    expect(session?.records.map((record) => [record.line, record.role, record.kind])).toEqual([
      [0, "user", "message"], [0, "operator", "bot_message"],
      [0, "user", "channel_join"], [0, "user", "message"],
    ]);
    for (const message of slackFixture) {
      const episode = slackEpisode(parseSlackMessage(JSON.stringify(message)), "C1", "C1");
      const record = session?.records.find((item) => item.origin.record === message.ts);
      expect(record?.origin).toEqual(episode.input.origin);
      expect(record?.occurred_at).toBe(episode.input.time?.value);
      expect(record?.text).toBe(message.text);
      expect(record?.revision).toBe(revisionOf(record?.occurred_at ?? "", message.text));
    }
    expect(isSlackSlop(parseSlackMessage(JSON.stringify(slackFixture[2])))).toBe(true);
    expect(session?.records.some((record) => record.origin.record === "102.0")).toBe(true);
    expect(session?.records[3]?.revision).not.toBe("104.0");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("slack reports physical JSONL lines and discovers channel and thread exports", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-slack-jsonl-"));
  try {
    await mkdir(join(root, "channels"));
    await mkdir(join(root, "threads"));
    const channel = join(root, "channels", "C1.jsonl");
    const thread = join(root, "threads", "C1-100.25.jsonl");
    await writeFile(channel, `\n${JSON.stringify(slackFixture[0])}\n`);
    await writeFile(thread, `${JSON.stringify(slackFixture[3])}\n`);
    await writeFile(join(root, "index.jsonl"), '{"id":"C1"}\n');
    expect(await Array.fromAsync(slackAdapter.files(root))).toEqual([channel, thread]);
    expect((await slackAdapter.parse(channel, await readFile(channel)))[0]?.records[0]?.line).toBe(2);
    expect((await slackAdapter.parse(thread, await readFile(thread)))[0]?.sessionId).toBe("C1");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
