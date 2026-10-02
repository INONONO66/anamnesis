import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { collectSlack } from "./slack.ts";
import { REDACTION } from "./secrets.ts";

let tmpRoot: string;
let next = 0;
const directory = async () => { const dir = join(tmpRoot, `slack${next++}`); await mkdir(dir); return dir; };
beforeAll(async () => { tmpRoot = await mkdtemp(join(tmpdir(), "slack-fs-")); });
afterAll(async () => { await rm(tmpRoot, { recursive: true, force: true }); });

describe("collectSlack", () => {
  test("merges threads, drops slop and orders by event time", async () => {
    const root = await directory();
    await mkdir(join(root, "channels"), { recursive: true });
    await mkdir(join(root, "threads"), { recursive: true });
    await writeFile(
      join(root, "index.jsonl"),
      `${JSON.stringify({ id: "C1", name: "pj-10x-data", type: "channel", msgs: 3 })}\n`,
    );
    await writeFile(
      join(root, "channels", "C1.jsonl"),
      [
        JSON.stringify({ ts: "200.5", text: "second", user: "U2" }),
        JSON.stringify({ ts: "100.0", text: "first", user: "U1" }),
        JSON.stringify({ ts: "150.0", text: "<@U9>さんがチャンネルに参加しました", user: "U9" }),
        JSON.stringify({ ts: "160.0", subtype: "channel_join", text: "joined", user: "U9" }),
        JSON.stringify({ ts: "170.0", text: "   ", user: "U9" }),
      ].join("\n"),
    );
    await writeFile(
      join(root, "channels", "C0.jsonl"),
      `${JSON.stringify({ ts: "90.0", text: "earlier channel", user: "U1" })}\n`,
    );
    await writeFile(
      join(root, "threads", "C1-200.5.jsonl"),
      `${JSON.stringify({ ts: "250.0", text: "reply", user: "U3", thread_ts: "200.5", edited: { ts: "260.0" } })}\n`,
    );

    const episodes = await collectSlack(root);

    expect(episodes.map((e) => e.input.content)).toEqual([
      "earlier channel",
      "first",
      "second",
      "reply",
    ]);
    expect(episodes[1]?.input.origin).toEqual({
      source: "slack",
      session: "C1",
      actor: "U1",
      record: "100.0",
    });
    expect(episodes[1]?.input.time).toEqual({
      value: new Date(100_000).toISOString(),
      precision: "second",
    });
    expect(episodes[1]?.input.properties).toEqual({
      channel_name: "pj-10x-data",
      slack_ts: "100.0",
    });
    const reply = episodes[3];
    expect(reply?.input.source_revision).toBe("260.0");
    expect(reply?.input.properties).toEqual({
      channel_name: "pj-10x-data",
      slack_ts: "250.0",
      thread_parent_ts: "200.5",
    });
  });

  test("drops every housekeeping subtype and localized join notice", async () => {
    const root = await directory();
    await mkdir(join(root, "channels"), { recursive: true });
    await writeFile(join(root, "index.jsonl"), JSON.stringify({ id: "C-SLOP", name: "general" }) + "\n");
    const subtypes = ["channel_join", "channel_leave", "group_join", "group_leave", "channel_topic", "channel_purpose", "channel_name", "mpdm_move", "huddle_thread", "bot_message"];
    const lines = subtypes.map((subtype, i) => JSON.stringify({ ts: `${i + 1}.0`, user: "U1", subtype, text: "housekeeping" }));
    lines.push(JSON.stringify({ ts: "20.0", user: "U1", text: "<@U1> has joined the channel" }));
    lines.push(JSON.stringify({ ts: "21.0", user: "U1", text: "real message" }));
    await writeFile(join(root, "channels", "C-SLOP.jsonl"), lines.join("\n") + "\n");
    const episodes = await collectSlack(root);
    expect(episodes).toHaveLength(1);
    expect(episodes[0]?.input.content).toBe("real message");
  });

  test("masks secrets found in message text", async () => {
    const root = await directory();
    await mkdir(join(root, "channels"), { recursive: true });
    await writeFile(join(root, "index.jsonl"), "");
    await writeFile(
      join(root, "channels", "C2.jsonl"),
      `${JSON.stringify({ ts: "1.0", text: "use ghp_0123456789012345678901234567890123456789", user: "U1" })}\n`,
    );

    const [episode] = await collectSlack(root);

    expect(episode?.redactions).toBe(1);
    expect(episode?.input.content).toBe(`use ${REDACTION}`);
    expect(episode?.input.properties).toEqual({
      channel_name: "C2",
      slack_ts: "1.0",
    });
  });
});
