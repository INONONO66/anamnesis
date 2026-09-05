import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, rm, writeFile, utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "@anamnesis/core";
import { collectAgentLog } from "./agentlog.ts";
import { collectGjcRaw } from "./gjcraw.ts";
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
    expect(episodes[0]?.input.source_revision).toBe(
      createHash("sha256")
        .update(`${new Date(1000).toISOString()}\nstart the backfill`, "utf8")
        .digest("hex"),
    );
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

  test("opens a revision per re-emission of one evolving event id", async () => {
    const root = await fixtureRoot();
    await writeFile(
      join(root, "codex.jsonl"),
      agentLines([
        {
          provider: "codex",
          partition_id: "session-stream",
          upstream_event_id: "cx-stream",
          occurred_at: 1000,
          role: "assistant",
          text: "partial answ",
        },
        {
          provider: "codex",
          partition_id: "session-stream",
          upstream_event_id: "cx-stream",
          occurred_at: 2000,
          role: "assistant",
          text: "partial answer, completed",
        },
      ]),
    );

    const episodes = await collectAgentLog(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "cx-stream",
      "cx-stream",
    ]);
    expect(episodes[0]?.input.source_revision).not.toBe(
      episodes[1]?.input.source_revision,
    );
  });

  test("keys the revision on raw text so masking changes open none", async () => {
    const root = await fixtureRoot();
    const secret = "ghp_0123456789012345678901234567890123456789";
    await writeFile(
      join(root, "codex.jsonl"),
      agentLines([
        {
          provider: "codex",
          partition_id: "session-secret",
          upstream_event_id: "cx-secret",
          occurred_at: 1000,
          role: "user",
          text: `use ${secret}`,
        },
      ]),
    );

    const [episode] = await collectAgentLog(root);

    expect(episode?.input.content).toBe(`use ${REDACTION}`);
    expect(episode?.input.source_revision).toBe(
      createHash("sha256")
        .update(`${new Date(1000).toISOString()}\nuse ${secret}`, "utf8")
        .digest("hex"),
    );
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

  test("ingests a re-emitted event id as a superseding revision", async () => {
    const root = await fixtureRoot();
    const scope = `stream-${Date.now()}`;
    await writeFile(
      join(root, "codex.jsonl"),
      agentLines([
        {
          provider: "codex",
          partition_id: `${scope}-session`,
          upstream_event_id: `${scope}-event`,
          occurred_at: 1000,
          role: "assistant",
          text: "partial answ",
        },
        {
          provider: "codex",
          partition_id: `${scope}-session`,
          upstream_event_id: `${scope}-event`,
          occurred_at: 2000,
          role: "assistant",
          text: "partial answer, completed",
        },
      ]),
    );
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const [first, second] = await collectAgentLog(root);

      const early = await engine.remember(first!.input);
      const late = await engine.remember(second!.input);

      expect(early).toMatchObject({ created: true });
      expect(early.invalidated).toBeUndefined();
      expect(late).toMatchObject({ created: true, invalidated: early.id });
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
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

/**
 * The raw store nests one session file per transcript under a per-workspace
 * directory, and subagent transcripts sit one level deeper beside the
 * `<n>.bash.log` captures of the commands their tool calls ran.
 */
async function gjcRawRoot(): Promise<string> {
  const root = await fixtureRoot();
  const sessions = join(root, "home", ".gjc", "agent", "sessions");
  const workspace = join(sessions, "-Develop-token_hub");
  const nested = join(workspace, "2026-07-17T15-03-18-381Z_019f709a");
  await mkdir(nested, { recursive: true });

  await writeFile(
    join(workspace, "main.jsonl"),
    [
      JSON.stringify({
        type: "session",
        version: 4,
        id: "session-main",
        timestamp: "2026-07-20T11:31:06.117Z",
        cwd: "/Users/ino/Develop/token_hub",
      }),
      JSON.stringify({
        type: "message",
        id: "ev-late",
        timestamp: "2026-07-20T11:35:00.000Z",
        message: {
          role: "assistant",
          content: [
            { type: "thinking", thinking: "provider scratch space" },
            { type: "text", text: LONG_TEXT },
          ],
          timestamp: 1,
        },
      }),
      JSON.stringify({
        type: "message",
        id: "ev-early",
        timestamp: "2026-07-20T11:32:00.000Z",
        message: {
          role: "user",
          content: [{ type: "text", text: "use ghp_0123456789012345678901234567890123456789" }],
          attribution: "user",
          timestamp: 2,
        },
      }),
      JSON.stringify({
        type: "compaction",
        id: "ev-compaction",
        timestamp: "2026-07-20T11:34:00.000Z",
        summary: "## Goal\nships the raw adapter",
      }),
      JSON.stringify({
        type: "message",
        id: "ev-two-parts",
        timestamp: "2026-07-20T11:36:00.000Z",
        message: {
          role: "assistant",
          content: [
            { type: "text", text: "first part" },
            { type: "text", text: "second part" },
          ],
          timestamp: 3,
        },
      }),
    ].join("\n"),
  );

  await writeFile(
    join(nested, "92-CFM4FinalReview.jsonl"),
    [
      JSON.stringify({
        type: "session",
        version: 4,
        id: "session-subagent",
        timestamp: "2026-07-20T11:31:06.117Z",
      }),
      JSON.stringify({
        type: "message",
        id: "ev-subagent",
        timestamp: "2026-07-20T11:33:00.000Z",
        message: {
          role: "user",
          content: [{ type: "text", text: "subagent turn" }],
          timestamp: 4,
        },
      }),
    ].join("\n"),
  );
  return root;
}

/**
 * Runtime bookkeeping the agent interleaves with the conversation: model and
 * mode switches, workspace reminders, tool plumbing and cancelled turns all
 * reach the transcript, and none of them is a turn the export published.
 * An event without an id has no record to key on, and one without an event
 * time has no place in the session order, so neither can be ingested.
 */
async function gjcNoiseRoot(): Promise<string> {
  const root = await fixtureRoot();
  const workspace = join(root, "home", ".gjc", "agent", "sessions", "-Develop");
  await mkdir(workspace, { recursive: true });
  await writeFile(
    join(workspace, "noise.jsonl"),
    [
      JSON.stringify({ type: "session", id: "session-noise", timestamp: "2026-07-20T11:00:00.000Z" }),
      JSON.stringify({ type: "model_change", id: "n-model", timestamp: "2026-07-20T11:00:01.000Z", model: "inonono/claude-opus-4-8" }),
      JSON.stringify({ type: "thinking_level_change", id: "n-think", timestamp: "2026-07-20T11:00:02.000Z", thinkingLevel: "medium" }),
      JSON.stringify({ type: "mode_change", id: "n-mode", timestamp: "2026-07-20T11:00:03.000Z" }),
      JSON.stringify({ type: "configured_model_chain", id: "n-chain", timestamp: "2026-07-20T11:00:04.000Z", entries: [] }),
      JSON.stringify({ type: "session_init", id: "n-init", timestamp: "2026-07-20T11:00:05.000Z" }),
      JSON.stringify({ type: "custom", id: "n-custom", timestamp: "2026-07-20T11:00:06.000Z", customType: "workflow-intent-diff", data: { route: "direct" } }),
      JSON.stringify({ type: "custom_message", id: "n-reminder", timestamp: "2026-07-20T11:00:07.000Z", customType: "volatile-project-context", content: "<system-reminder>workspace tree</system-reminder>" }),
      JSON.stringify({
        type: "message",
        id: "n-toolcall",
        timestamp: "2026-07-20T11:00:08.000Z",
        message: { role: "assistant", content: [{ type: "toolCall", id: "toolu_1", name: "read", arguments: { path: "README.md" } }], timestamp: 1 },
      }),
      JSON.stringify({
        type: "message",
        id: "n-toolresult",
        timestamp: "2026-07-20T11:00:09.000Z",
        message: { role: "toolResult", toolCallId: "toolu_1", toolName: "read", content: [{ type: "text", text: "file body" }], timestamp: 2 },
      }),
      JSON.stringify({
        type: "message",
        id: "n-filemention",
        timestamp: "2026-07-20T11:00:10.000Z",
        message: { role: "fileMention", content: null, timestamp: 3 },
      }),
      JSON.stringify({
        type: "message",
        id: "n-blank",
        timestamp: "2026-07-20T11:00:11.000Z",
        message: { role: "user", content: [{ type: "text", text: "   " }], timestamp: 4 },
      }),
      JSON.stringify({
        type: "message",
        id: "n-thinking-only",
        timestamp: "2026-07-20T11:00:12.000Z",
        message: { role: "assistant", content: [{ type: "thinking", thinking: "scratch" }], timestamp: 5 },
      }),
      JSON.stringify({
        type: "message",
        timestamp: "2026-07-20T11:00:13.000Z",
        message: { role: "user", content: [{ type: "text", text: "no id, no record to key on" }], timestamp: 6 },
      }),
      JSON.stringify({
        type: "message",
        id: "n-untimed",
        message: { role: "user", content: [{ type: "text", text: "no event time, no place in the order" }], timestamp: 7 },
      }),
      JSON.stringify({
        type: "message",
        id: "n-textless",
        timestamp: "2026-07-20T11:00:14.000Z",
        message: { role: "user", content: "not a part list", timestamp: 8 },
      }),
      JSON.stringify({
        type: "compaction",
        id: "n-empty-compaction",
        timestamp: "2026-07-20T11:00:15.000Z",
        summary: "   ",
      }),
      JSON.stringify({
        type: "message",
        id: "n-kept",
        timestamp: "2026-07-20T11:00:16.000Z",
        message: { role: "user", content: [{ type: "text", text: "the only turn here" }], timestamp: 9 },
      }),
    ].join("\n"),
  );
  return root;
}

describe("collectGjcRaw", () => {
  test("orders every session file by event time and maps the origin", async () => {
    const root = await gjcRawRoot();

    const episodes = await collectGjcRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "ev-early",
      "ev-subagent",
      "ev-compaction",
      "ev-late",
      "ev-two-parts",
    ]);
    expect(episodes[1]?.input.origin).toEqual({
      source: "gjc",
      session: "session-subagent",
      actor: "user",
      record: "ev-subagent",
    });
    expect(episodes[1]?.input.time).toEqual({
      value: "2026-07-20T11:33:00.000Z",
      precision: "second",
    });
    expect(episodes[1]?.input.properties).toEqual({
      kind: "message",
      role: "user",
    });
    expect(episodes[1]?.input.schema).toBe("anamnesis.original-message/1");
    expect(episodes[1]?.input.payload).toBeUndefined();
  });

  /**
   * The contract this adapter exists to honour. The expected values are copied
   * by hand from one line of the normalized export the 1,128 already-ingested
   * sessions came from, beside the raw event that produced it: reaching the
   * same turn from the raw store has to rebuild that export's origin tuple and
   * revision exactly, or re-reading the raw store would duplicate every
   * session already in the graph rather than resolving to a no-op.
   *
   * The raw event carries two timestamps 39_944ms apart. The export keyed on
   * the outer one, so this fixture keeps both and pins the outer.
   */
  test("rebuilds the origin tuple and revision the normalized export wrote", async () => {
    const root = await fixtureRoot();
    const workspace = join(root, "home", ".gjc", "agent", "sessions", "-Develop-token_hub");
    await mkdir(workspace, { recursive: true });
    const text = "Return the M4 verdict now with only blocker/high findings and status.";
    await writeFile(
      join(workspace, "92-CFM4FinalReview.jsonl"),
      [
        JSON.stringify({
          type: "session",
          version: 4,
          id: "019f7f4b-4105-7000-8518-eb0fbb71a583",
          timestamp: "2026-07-20T11:31:06.117Z",
          cwd: "/Users/ino/Develop/token_hub",
        }),
        JSON.stringify({
          type: "message",
          id: "bdf10532",
          parentId: "ada9ed6c",
          timestamp: "2026-07-20T11:33:34.269Z",
          message: {
            role: "user",
            content: [{ type: "text", text }],
            attribution: "user",
            timestamp: 1784547174325,
          },
        }),
      ].join("\n"),
    );

    const [episode] = await collectGjcRaw(root);

    // Copied from the export: partition_id, upstream_event_id, role, occurred_at.
    const occurredAt = 1784547214269;
    expect(episode?.input.origin).toEqual({
      source: "gjc",
      session: "019f7f4b-4105-7000-8518-eb0fbb71a583",
      actor: "user",
      record: "bdf10532",
    });
    expect(episode?.input.time?.value).toBe(new Date(occurredAt).toISOString());
    expect(episode?.input.source_revision).toBe(
      createHash("sha256")
        .update(`${new Date(occurredAt).toISOString()}\n${text}`, "utf8")
        .digest("hex"),
    );
    expect(episode?.input.content).toBe(text);
  });

  test("masks secrets and keys the revision on the raw text", async () => {
    const root = await gjcRawRoot();

    const episodes = await collectGjcRaw(root);
    const masked = episodes[0];
    const secret = "use ghp_0123456789012345678901234567890123456789";

    expect(masked?.redactions).toBe(1);
    expect(masked?.input.content).toBe(`use ${REDACTION}`);
    expect(masked?.input.source_revision).toBe(
      createHash("sha256")
        .update(`2026-07-20T11:32:00.000Z\n${secret}`, "utf8")
        .digest("hex"),
    );
  });

  test("stores an oversized turn as a payload with an excerpt on the node", async () => {
    const root = await gjcRawRoot();

    const episodes = await collectGjcRaw(root);
    const long = episodes[3];

    expect(long?.input.content).toBe("x".repeat(4000));
    expect(long?.input.payload).toEqual(new TextEncoder().encode(LONG_TEXT));
    expect(long?.input.payload_media_type).toBe("text/plain");
  });

  test("keeps a limit-length turn inline", async () => {
    const root = await fixtureRoot();
    const workspace = join(root, "home", ".gjc", "agent", "sessions", "-Develop");
    await mkdir(workspace, { recursive: true });
    await writeFile(
      join(workspace, "limit.jsonl"),
      [
        JSON.stringify({ type: "session", id: "session-limit", timestamp: "2026-07-20T11:00:00.000Z" }),
        JSON.stringify({
          type: "message",
          id: "ev-limit",
          timestamp: "2026-07-20T11:00:01.000Z",
          message: { role: "assistant", content: [{ type: "text", text: "y".repeat(4000) }], timestamp: 1 },
        }),
      ].join("\n"),
    );

    const [episode] = await collectGjcRaw(root);

    expect(episode?.input.content).toBe("y".repeat(4000));
    expect(episode?.input.payload).toBeUndefined();
    expect(episode?.input.payload_media_type).toBeUndefined();
  });

  /**
   * A compaction replaces the turns it summarizes, so its summary is the only
   * surviving record of them and it has no author of its own.
   */
  test("carries a compaction summary under the kind that names it", async () => {
    const root = await gjcRawRoot();

    const episodes = await collectGjcRaw(root);
    const compaction = episodes[2];

    expect(compaction?.input.content).toBe("## Goal\nships the raw adapter");
    expect(compaction?.input.origin.actor).toBe("unknown");
    expect(compaction?.input.properties).toEqual({ kind: "compaction" });
  });

  test("joins the text parts of one turn and drops provider scratch space", async () => {
    const root = await gjcRawRoot();

    const episodes = await collectGjcRaw(root);

    expect(episodes[4]?.input.content).toBe("first part\nsecond part");
    expect(episodes[3]?.input.content).not.toContain("provider scratch space");
  });

  test("skips runtime bookkeeping, tool plumbing and incomplete events", async () => {
    const root = await gjcNoiseRoot();

    const episodes = await collectGjcRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["n-kept"]);
    expect(episodes[0]?.input.content).toBe("the only turn here");
  });

  /**
   * The export was written on macOS, so an AppleDouble sidecar mirrors every
   * transcript. Reading one would parse its binary header as a transcript.
   */
  test("ignores AppleDouble sidecars and non-transcript files", async () => {
    const root = await gjcNoiseRoot();
    const workspace = join(root, "home", ".gjc", "agent", "sessions", "-Develop");
    await writeFile(join(workspace, "._noise.jsonl"), "\u0000\u0005\u0016\u0007not json");
    await writeFile(join(workspace, "12.bash.log"), "## Stack\n\n- Base: `codex/calendar-sync`\n");
    await writeFile(join(workspace, "state.json"), JSON.stringify({ skill: "ralplan" }));

    const episodes = await collectGjcRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["n-kept"]);
  });

  /** A transcript whose header never arrives cannot be attributed to a session. */
  test("leaves out events that arrive before their session header", async () => {
    const root = await fixtureRoot();
    const workspace = join(root, "home", ".gjc", "agent", "sessions", "-Develop");
    await mkdir(workspace, { recursive: true });
    await writeFile(
      join(workspace, "headerless.jsonl"),
      [
        JSON.stringify({
          type: "message",
          id: "ev-orphan",
          timestamp: "2026-07-20T11:00:00.000Z",
          message: { role: "user", content: [{ type: "text", text: "orphan" }], timestamp: 1 },
        }),
        "",
        JSON.stringify(["not an object"]),
      ].join("\n"),
    );

    expect(await collectGjcRaw(root)).toEqual([]);
  });

  test("tie-breaks equal event times on the record so the order is total", async () => {
    const root = await fixtureRoot();
    const workspace = join(root, "home", ".gjc", "agent", "sessions", "-Develop");
    await mkdir(workspace, { recursive: true });
    const same = (id: string): string =>
      JSON.stringify({
        type: "message",
        id,
        timestamp: "2026-07-20T11:00:00.000Z",
        message: { role: "user", content: [{ type: "text", text: `tie ${id}` }], timestamp: 1 },
      });
    await writeFile(
      join(workspace, "tie.jsonl"),
      [
        JSON.stringify({ type: "session", id: "session-tie", timestamp: "2026-07-20T10:00:00.000Z" }),
        same("b"),
        same("c"),
        same("a"),
      ].join("\n"),
    );

    const episodes = await collectGjcRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["a", "b", "c"]);
  });

  /** A raw store that was never populated is an empty backfill, not an error. */
  test("returns nothing when the sessions root is absent", async () => {
    expect(await collectGjcRaw(await fixtureRoot())).toEqual([]);
  });
});
