import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, rm, writeFile, utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "@anamnesis/core";
import { collectAgentLog } from "./agentlog.ts";
import { collectNotion } from "./notion.ts";
import { collectSlack } from "./slack.ts";
import { maskSecrets, REDACTION } from "./secrets.ts";

async function fixtureRoot(): Promise<string> {
  return await mkdtemp(join(tmpdir(), "backfill-"));
}

const TEST_DB = {
  uri: "bolt://127.0.0.1:7688",
  user: "neo4j",
  password: "anamnesis-test",
};

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
    const aside = join(root, "10x-docs-hub", "Aside.md");
    await writeFile(aside, "# Aside\n");
    await utimes(
      aside,
      new Date("2026-06-01T00:00:00Z"),
      new Date("2026-06-01T00:00:00Z"),
    );
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
    expect(episodes.map((e) => e.input.time?.value)).toEqual([
      "2026-03-01T00:00:00.000Z",
      "2026-06-01T00:00:00.000Z",
    ]);
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

interface AgentLine {
  provider: string;
  partition_id: string;
  upstream_event_id: string;
  occurred_at?: number;
  role?: string;
  canonical_kind?: string;
  kind?: string;
  text?: string;
}

function agentLines(lines: AgentLine[]): string {
  return `${lines.map((line) => JSON.stringify(line)).join("\n")}\n`;
}

const LONG_TEXT = "x".repeat(4200);

/**
 * Two providers with interleaved sessions, written in an order that contradicts
 * both file name order and event-time order inside every file: `codex.jsonl`
 * opens on the latest turn of a session it does not start, and the sole
 * `claude-code` session is split around it. Alongside the transcripts sit the
 * export manifest, an AppleDouble sidecar and a provider that captured
 * nothing. The scope keeps records unique per run so leftover episodes from an
 * earlier run cannot join these session chains.
 */
async function agentLogRoot(scope = "fixed"): Promise<string> {
  const root = await fixtureRoot();
  await writeFile(
    join(root, "manifest.json"),
    JSON.stringify({ exported_at: 1773070576754, files: 2 }),
  );
  await writeFile(join(root, "._codex.jsonl"), agentLines([]));
  await writeFile(join(root, "pi.jsonl"), "");
  await writeFile(
    join(root, "codex.jsonl"),
    agentLines([
      {
        provider: "codex",
        partition_id: `${scope}-session-a`,
        upstream_event_id: `${scope}-cx-5`,
        occurred_at: 5000,
        role: "assistant",
        canonical_kind: "agent_message",
        kind: "message",
        text: LONG_TEXT,
      },
      {
        provider: "codex",
        partition_id: `${scope}-session-a`,
        upstream_event_id: `${scope}-cx-1`,
        occurred_at: 1000,
        role: "user",
        canonical_kind: "agent_message",
        kind: "message",
        text: "start the backfill",
      },
      {
        provider: "codex",
        partition_id: `${scope}-session-a`,
        upstream_event_id: `${scope}-cx-3`,
        occurred_at: 3000,
        canonical_kind: "tool_result",
        kind: "tool",
        text: "   ",
      },
      {
        provider: "codex",
        partition_id: `${scope}-session-b`,
        upstream_event_id: `${scope}-cx-6`,
        role: "assistant",
        text: "no event time, no place in the order",
      },
      {
        provider: "codex",
        partition_id: `${scope}-session-b`,
        upstream_event_id: `${scope}-cx-4`,
        occurred_at: 4000,
        role: "user",
        canonical_kind: "agent_message",
        kind: "message",
        text: "second codex session",
      },
    ]),
  );
  await writeFile(
    join(root, "claude-code.jsonl"),
    agentLines([
      {
        provider: "claude-code",
        partition_id: `${scope}-session-c`,
        upstream_event_id: `${scope}-cc-2`,
        occurred_at: 2000,
        role: "user",
        canonical_kind: "agent_message",
        kind: "message",
        text: "use ghp_0123456789012345678901234567890123456789",
      },
      {
        provider: "claude-code",
        partition_id: `${scope}-session-c`,
        upstream_event_id: `${scope}-cc-7`,
        occurred_at: 7000,
        role: "assistant",
        text: "later turn, no kinds",
      },
    ]),
  );
  return root;
}

describe("collectAgentLog", () => {
  test("orders every provider file by event time and maps the origin", async () => {
    const root = await agentLogRoot();

    const episodes = await collectAgentLog(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "fixed-cx-1",
      "fixed-cc-2",
      "fixed-cx-4",
      "fixed-cx-5",
      "fixed-cc-7",
    ]);
    expect(episodes[0]?.input.origin).toEqual({
      source: "codex",
      session: "fixed-session-a",
      actor: "user",
      record: "fixed-cx-1",
    });
    expect(episodes[0]?.input.source_revision).toBe("fixed-cx-1");
    expect(episodes[0]?.input.time).toEqual({
      value: new Date(1000).toISOString(),
      precision: "second",
    });
    expect(episodes[0]?.input.properties).toEqual({
      canonical_kind: "agent_message",
      kind: "message",
    });
    expect(episodes[0]?.input.payload).toBeUndefined();
    expect(episodes[0]?.input.schema).toBe("anamnesis.original-message/1");
  });

  test("masks secrets and omits properties the export did not carry", async () => {
    const root = await agentLogRoot();

    const episodes = await collectAgentLog(root);
    const masked = episodes[1];
    const kindless = episodes[4];

    expect(masked?.redactions).toBe(1);
    expect(masked?.input.content).toBe(`use ${REDACTION}`);
    expect(kindless?.input.properties).toEqual({});
    expect(kindless?.redactions).toBe(0);
  });

  test("stores an oversized turn as a payload with an excerpt on the node", async () => {
    const root = await agentLogRoot();

    const episodes = await collectAgentLog(root);
    const long = episodes[3];

    expect(long?.input.content).toBe("x".repeat(4000));
    expect(long?.input.payload).toEqual(new TextEncoder().encode(LONG_TEXT));
    expect(long?.input.payload_media_type).toBe("text/plain");
  });

  test("keeps a limit-length turn inline and defaults a roleless actor", async () => {
    const root = await fixtureRoot();
    await writeFile(
      join(root, "gjc.jsonl"),
      agentLines([
        {
          provider: "gjc",
          partition_id: "session-e",
          upstream_event_id: "gj-1",
          occurred_at: 1000,
          canonical_kind: "agent_message",
          text: "y".repeat(4000),
        },
      ]),
    );

    const [episode] = await collectAgentLog(root);

    expect(episode?.input.content).toBe("y".repeat(4000));
    expect(episode?.input.payload).toBeUndefined();
    expect(episode?.input.payload_media_type).toBeUndefined();
    expect(episode?.input.origin.actor).toBe("unknown");
    expect(episode?.input.properties).toEqual({
      canonical_kind: "agent_message",
    });
  });

  test("tie-breaks equal event times on the record so the order is total", async () => {
    const root = await fixtureRoot();
    const same = (id: string): AgentLine => ({
      provider: "codex",
      partition_id: "session-tie",
      upstream_event_id: id,
      occurred_at: 7000,
      role: "user",
      text: `tie ${id}`,
    });
    await writeFile(
      join(root, "codex.jsonl"),
      agentLines([same("b"), same("c"), same("a")]),
    );

    const episodes = await collectAgentLog(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["a", "b", "c"]);
  });

  test("rejects an event missing an identity field the contract requires", async () => {
    const root = await fixtureRoot();
    await writeFile(
      join(root, "codex.jsonl"),
      `${JSON.stringify({ provider: "codex", partition_id: "session-a", occurred_at: 1, text: "hi" })}\n`,
    );

    expect(collectAgentLog(root)).rejects.toThrow(
      "agent event without upstream_event_id",
    );
  });

  test("ingests one unbroken session chain per exported session", async () => {
    const root = await agentLogRoot(`chain-${Date.now()}`);
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const episodes = await collectAgentLog(root);
      const ids: string[] = [];
      for (const episode of episodes) {
        ids.push((await engine.remember(episode.input)).id);
      }

      const heads: string[] = [];
      for (const [index, id] of ids.entries()) {
        const inbound = await engine.store.linksOf(id, "NEXT_EPISODE");
        if (inbound.every((link) => link.to !== id)) {
          const origin = episodes[index]?.input.origin;
          heads.push(`${origin?.source}/${origin?.session}`);
        }
      }
      const sessions = episodes.map(
        (e) => `${e.input.origin.source}/${e.input.origin.session}`,
      );

      expect(heads.sort()).toEqual([...new Set(sessions)].sort());
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
  });
});
