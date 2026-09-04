import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, writeFile, utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { collectNotion } from "./notion.ts";
import { collectSlack } from "./slack.ts";
import { maskSecrets, REDACTION } from "./secrets.ts";

async function fixtureRoot(): Promise<string> {
  return await mkdtemp(join(tmpdir(), "backfill-"));
}

describe("maskSecrets", () => {
  test("masks credential shapes before the write boundary", () => {
    const result = maskSecrets(
      "token xoxb-1234567890abcdef and key AKIA0123456789ABCDEF",
    );

    expect(result.redactions).toBe(2);
    expect(result.text).toBe(`token ${REDACTION} and key ${REDACTION}`);
  });

  test("leaves ordinary prose untouched", () => {
    const text = "we discussed the CalendarGNN paper in the data channel";

    expect(maskSecrets(text)).toEqual({ text, redactions: 0 });
  });
});

describe("collectSlack", () => {
  test("merges threads, drops slop and orders by event time", async () => {
    const root = await fixtureRoot();
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

  test("masks secrets found in message text", async () => {
    const root = await fixtureRoot();
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

describe("collectNotion", () => {
  test("keys the revision on masked content and carries the payload", async () => {
    const root = await fixtureRoot();
    await mkdir(join(root, "10x-docs-hub", "nested"), { recursive: true });
    await writeFile(join(root, "10x-docs-hub", "Aside.md"), "# Aside\n");
    const path = join(root, "10x-docs-hub", "nested", "Onboarding.md");
    await writeFile(path, "# Onboarding\n\nsecret: abcdefghijklmnopqrstuvwxyz\n");
    await utimes(path, new Date("2026-03-01T00:00:00Z"), new Date("2026-03-01T00:00:00Z"));

    const episodes = await collectNotion(root);
    const episode = episodes.find((e) =>
      e.input.origin.record.endsWith("Onboarding.md"),
    );
    const body = `# Onboarding\n\n${REDACTION}\n`;

    expect(episode?.redactions).toBe(1);
    expect(episode?.input.origin).toEqual({
      source: "notion",
      session: "10x-docs-hub",
      actor: "export",
      record: join("10x-docs-hub", "nested", "Onboarding.md"),
    });
    expect(episode?.input.source_revision).toBe(
      createHash("sha256").update(body, "utf8").digest("hex"),
    );
    expect(episode?.input.time).toEqual({
      value: "2026-03-01T00:00:00.000Z",
      precision: "day",
    });
    expect(episode?.input.payload).toEqual(new TextEncoder().encode(body));
    expect(episode?.input.payload_media_type).toBe("text/markdown");
    expect(episode?.input.content).toBe(`Onboarding\n\n# Onboarding ${REDACTION}`);
  });

  test("falls back to the title when a document has no body", async () => {
    const root = await fixtureRoot();
    await writeFile(join(root, "Empty.md"), "");

    const [episode] = await collectNotion(root);

    expect(episode?.input.content).toBe("Empty");
    expect(episode?.input.origin.session).toBe("Empty.md");
  });
});
