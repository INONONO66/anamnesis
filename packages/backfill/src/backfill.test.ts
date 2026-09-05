import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import {
  mkdtemp,
  mkdir,
  readdir,
  rm,
  stat,
  writeFile,
  utimes,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import neo4j from "neo4j-driver";
import { Engine } from "@anamnesis/core";
import type { RememberInput } from "@anamnesis/core";
import { collectAgentLog } from "./agentlog.ts";
import { collectClaudeRaw } from "./clauderaw.ts";
import type { ClaudeRawEpisode } from "./clauderaw.ts";
import { collectGjcRaw } from "./gjcraw.ts";
import { collectCodexRaw } from "./codexraw.ts";
import { collectMiscRaw } from "./miscraw.ts";
import { collectNotion } from "./notion.ts";
import { collectOmoRaw } from "./omoraw.ts";
import { collectSlack } from "./slack.ts";
import {
  concurrency,
  groupBySession,
  ingestGroups,
  main,
  type Collected,
  type Receipt,
} from "./run.ts";
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

/**
 * Two records copied byte for byte out of the normalized export this adapter
 * backfills alongside, each with the raw transcript line it was derived from.
 * They are the alignment contract in its original form: if the adapter ever
 * derives a different origin tuple for these bytes, the overlap with the ten
 * thousand already-ingested sessions stops being a no-op and the assertion
 * below fails. Both are short, prose-only turns with nothing to redact.
 */
const CANONICAL_SAMPLES = [
  {
    raw: '{"type":"user","timestamp":"2026-05-30T05:03:48.912Z","content":"\\"Reply exactly: OK\\""}',
    session: "ses_188bba578ffeZcbjTC7AHsYCX9",
    record:
      "fallback:66407b928098d9c71c1605d287c4dec25c0b28c309938eaad1d65a210171ea83:0",
    role: "user",
    occurredAt: 1780117428912,
    text: '"Reply exactly: OK"',
  },
  {
    raw: '{"type":"user","timestamp":"2026-06-17T09:17:00.521Z","content":"\\"Reply with exactly one word: pong\\""}',
    session: "ses_12b216332ffe8RU9AomQyKnaRV",
    record:
      "fallback:3f418bfb1dc411fd4ae955d15477cd3e97ee522a511586468749c99795c170e9:0",
    role: "user",
    occurredAt: 1781687820521,
    text: '"Reply with exactly one word: pong"',
  },
] as const;

const LONG_TURN = "z".repeat(4200);

function claudeLine(record: Record<string, unknown>): string {
  return JSON.stringify(record);
}

function assistantText(
  uuid: string,
  timestamp: string,
  blocks: readonly unknown[],
  extra: Record<string, unknown> = {},
): string {
  return claudeLine({
    type: "assistant",
    uuid,
    timestamp,
    message: { role: "assistant", content: blocks },
    ...extra,
  });
}

/**
 * Two projects with two sessions each, every file written out of event-time
 * order and the projects named so that file order contradicts time order.
 * Alongside the transcripts sit the record kinds the export rejected — a
 * sidechain turn written into a main transcript, tool plumbing, an API error
 * and an empty message — plus an AppleDouble sidecar that parses as neither.
 *
 * `session-a1` also owns the two delegated transcripts it spawned: one
 * subagent and one workflow step, both naming `session-a1` as their
 * `sessionId` exactly as the runtime writes them, beside the workflow journal
 * that is orchestration bookkeeping rather than a conversation.
 *
 * The tree is laid out the way the production snapshot lays it out — the
 * transcript roots under the `home/.claude` the raw tree was copied from,
 * not at the top of the dataset root — because a fixture that hoists them to
 * the top exercises a path the real snapshot never takes.
 */
async function claudeRawRoot(): Promise<string> {
  const root = await fixtureRoot();
  const home = join(root, "home", ".claude");
  const alpha = join(home, "projects", "-Users-ino-alpha");
  const beta = join(home, "projects", "-Users-ino-beta");
  const subagents = join(alpha, "session-a1", "subagents");
  const workflow = join(subagents, "workflows", "wf_11111111-222");
  await mkdir(workflow, { recursive: true });
  await mkdir(beta, { recursive: true });
  await mkdir(join(home, "transcripts"), { recursive: true });

  await writeFile(
    join(alpha, "session-a1.jsonl"),
    [
      assistantText("a1-late", "2026-05-01T00:00:09.000Z", [
        { type: "text", text: "the later answer" },
      ]),
      claudeLine({
        type: "user",
        uuid: "a1-early",
        timestamp: "2026-05-01T00:00:03.000Z",
        sessionId: "session-a1",
        cwd: "/Users/ino/alpha",
        gitBranch: "main",
        message: { role: "user", content: "the earlier question" },
      }),
      claudeLine({
        type: "user",
        uuid: "a1-side",
        timestamp: "2026-05-01T00:00:04.000Z",
        isSidechain: true,
        message: { role: "user", content: "delegated away" },
      }),
      claudeLine({
        type: "assistant",
        uuid: "a1-error",
        timestamp: "2026-05-01T00:00:05.000Z",
        isApiErrorMessage: true,
        message: { role: "assistant", content: [{ type: "text", text: "overloaded" }] },
      }),
      claudeLine({
        type: "assistant",
        uuid: "a1-tool",
        timestamp: "2026-05-01T00:00:06.000Z",
        message: {
          role: "assistant",
          content: [{ type: "tool_use", id: "t1", name: "Bash", input: {} }],
        },
      }),
      claudeLine({
        type: "user",
        uuid: "a1-result",
        timestamp: "2026-05-01T00:00:07.000Z",
        message: {
          role: "user",
          content: [{ type: "tool_result", tool_use_id: "t1", content: "ok" }],
        },
      }),
      claudeLine({
        type: "user",
        uuid: "a1-empty",
        timestamp: "2026-05-01T00:00:08.000Z",
        message: { role: "user", content: "" },
      }),
      claudeLine({ type: "queue-operation", operation: "enqueue", timestamp: "2026-05-01T00:00:02.000Z" }),
    ].join("\n"),
  );
  /**
   * A delegate names the session that spawned it in `sessionId`, carries its
   * own `agentId`, and flags every record `isSidechain`. Its last turn is a
   * compaction, which a long delegation reaches the same way a session does.
   */
  await writeFile(
    join(subagents, "agent-deadbeef.jsonl"),
    [
      claudeLine({
        type: "user",
        uuid: "sub-1",
        timestamp: "2026-05-01T00:00:01.000Z",
        isSidechain: true,
        agentId: "deadbeef",
        sessionId: "session-a1",
        cwd: "/Users/ino/alpha",
        gitBranch: "main",
        message: { role: "user", content: "the delegate's own transcript" },
      }),
      claudeLine({
        type: "assistant",
        uuid: "sub-2",
        timestamp: "2026-05-01T00:00:10.000Z",
        isSidechain: true,
        agentId: "deadbeef",
        sessionId: "session-a1",
        message: {
          role: "assistant",
          content: [
            { type: "thinking", thinking: "delegate scratch space" },
            { type: "text", text: "what the delegate reported back" },
          ],
        },
      }),
      claudeLine({
        type: "system",
        subtype: "compact_boundary",
        uuid: "sub-compact",
        timestamp: "2026-05-01T00:00:11.000Z",
        isSidechain: true,
        agentId: "deadbeef",
        content: "the delegation so far, compacted",
      }),
    ].join("\n"),
  );
  await writeFile(
    join(workflow, "agent-cafebabe.jsonl"),
    claudeLine({
      type: "user",
      uuid: "wf-1",
      timestamp: "2026-05-01T00:00:12.000Z",
      isSidechain: true,
      agentId: "cafebabe",
      sessionId: "session-a1",
      cwd: "/Users/ino/alpha",
      message: { role: "user", content: "the workflow step's own prompt" },
    }),
  );
  await writeFile(
    join(workflow, "journal.jsonl"),
    claudeLine({ type: "started", key: "v2:abc", agentId: "cafebabe" }),
  );
  /**
   * A background job's timeline sits beside the transcript roots under a
   * directory of its own and holds runtime bookkeeping, not conversation.
   */
  await mkdir(join(home, "jobs", "6710737a"), { recursive: true });
  await writeFile(
    join(home, "jobs", "6710737a", "timeline.jsonl"),
    claudeLine({
      type: "user",
      uuid: "job-1",
      timestamp: "2026-05-01T00:00:01.000Z",
      message: { role: "user", content: "a job timeline entry" },
    }),
  );
  await writeFile(
    join(subagents, "._agent-deadbeef.jsonl"),
    claudeLine({
      type: "user",
      uuid: "sub-sidecar",
      timestamp: "2026-05-01T00:00:01.000Z",
      isSidechain: true,
      agentId: "deadbeef",
      message: { role: "user", content: "AppleDouble beside a delegate" },
    }),
  );
  await writeFile(
    join(alpha, "._session-a1.jsonl"),
    claudeLine({
      type: "user",
      uuid: "sidecar",
      timestamp: "2026-05-01T00:00:00.000Z",
      message: { role: "user", content: "AppleDouble" },
    }),
  );

  /**
   * A message mixing prose with plumbing, so the export sliced it and suffixed
   * each surviving block with its position, beside one that is prose only and
   * kept the message's own id.
   */
  await writeFile(
    join(alpha, "session-a2.jsonl"),
    [
      assistantText("a2-mixed", "2026-05-02T00:00:02.000Z", [
        { type: "thinking", thinking: "reasoning that never shipped" },
        { type: "text", text: "first shipped block" },
        { type: "tool_use", id: "t2", name: "Read", input: {} },
        { type: "text", text: "second shipped block" },
      ]),
      assistantText("a2-whole", "2026-05-02T00:00:01.000Z", [
        { type: "text", text: "one half" },
        { type: "text", text: "other half" },
      ]),
      assistantText("a2-blockless", "2026-05-02T00:00:03.000Z", [
        "a bare string where a block belongs",
        null,
      ]),
    ].join("\n"),
  );

  await writeFile(
    join(beta, "session-b1.jsonl"),
    [
      claudeLine({
        type: "summary",
        uuid: "b1-summary",
        timestamp: "2026-05-03T00:00:02.000Z",
        summary: "the session so far, compacted",
      }),
      claudeLine({
        type: "user",
        uuid: "b1-secret",
        timestamp: "2026-05-03T00:00:01.000Z",
        message: {
          role: "user",
          content: "deploy with ghp_0123456789012345678901234567890123456789",
        },
      }),
      claudeLine({
        type: "assistant",
        uuid: "b1-long",
        timestamp: "2026-05-03T00:00:03.000Z",
        message: { role: "assistant", content: [{ type: "text", text: LONG_TURN }] },
      }),
      claudeLine({ type: "system", subtype: "boot", timestamp: "2026-05-03T00:00:00.000Z", content: "session started" }),
    ].join("\n"),
  );
  await writeFile(
    join(beta, "session-b2.jsonl"),
    [
      claudeLine({
        type: "user",
        uuid: "b2-tie",
        timestamp: "2026-05-04T00:00:00.000Z",
        message: { role: "user", content: "tie two" },
      }),
      claudeLine({
        type: "user",
        uuid: "b2-aaa",
        timestamp: "2026-05-04T00:00:00.000Z",
        message: { role: "user", content: "tie one" },
      }),
    ].join("\n"),
  );

  for (const sample of CANONICAL_SAMPLES) {
    await writeFile(
      join(home, "transcripts", `${sample.session}.jsonl`),
      `${sample.raw}\n`,
    );
    /** Every file on the copied volume carries an AppleDouble sidecar. */
    await writeFile(
      join(home, "transcripts", `._${sample.session}.jsonl`),
      `${sample.raw}\n`,
    );
  }
  return root;
}

describe("collectClaudeRaw", () => {
  test("derives the origin the normalized export already ingested", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);

    for (const sample of CANONICAL_SAMPLES) {
      const episode = episodes.find(
        (candidate) => candidate.input.origin.session === sample.session,
      );
      expect(episode?.input.origin).toEqual({
        source: "claude-code",
        session: sample.session,
        actor: sample.role,
        record: sample.record,
      });
      const occurredAt = new Date(sample.occurredAt).toISOString();
      expect(episode?.input.time).toEqual({
        value: occurredAt,
        precision: "second",
      });
      expect(episode?.input.content).toBe(sample.text);
      expect(episode?.input.source_revision).toBe(
        createHash("sha256")
          .update(`${occurredAt}\n${sample.text}`, "utf8")
          .digest("hex"),
      );
    }
  });

  /**
   * The production snapshot keeps the home directory it was copied from, so
   * the transcript roots sit under `home/.claude/` rather than at the top of
   * the dataset root. Reading the `~/.claude` layout as well is what lets one
   * root serve both, and the ids have to come out identical either way: they
   * hash the line's on-disk bytes, so a snapshot re-run has to be recognized
   * as duplicates of the records the normalized export already ingested
   * rather than as a second copy of them.
   */
  test("reads the transcript roots at whatever depth the snapshot nests them", async () => {
    const nested = await fixtureRoot();
    const flat = await fixtureRoot();
    for (const [root, prefix] of [
      [nested, join("home", ".claude")],
      [flat, "."],
    ] as const) {
      await mkdir(join(root, prefix, "transcripts"), { recursive: true });
      await mkdir(join(root, prefix, "projects", "-Users-ino-alpha"), {
        recursive: true,
      });
      for (const sample of CANONICAL_SAMPLES) {
        await writeFile(
          join(root, prefix, "transcripts", `${sample.session}.jsonl`),
          `${sample.raw}\n`,
        );
      }
      await writeFile(
        join(root, prefix, "projects", "-Users-ino-alpha", "session-a1.jsonl"),
        claudeLine({
          type: "user",
          uuid: "a1-early",
          timestamp: "2026-05-01T00:00:03.000Z",
          sessionId: "session-a1",
          message: { role: "user", content: "the earlier question" },
        }),
      );
    }

    const fromNested = await collectClaudeRaw(nested);
    const fromFlat = await collectClaudeRaw(flat);

    const origins = (
      episodes: readonly ClaudeRawEpisode[],
    ): RememberInput["origin"][] =>
      episodes.map((episode) => episode.input.origin);
    expect(origins(fromNested)).toEqual([
      {
        source: "claude-code",
        session: "session-a1",
        actor: "user",
        record: "a1-early",
      },
      {
        source: "claude-code",
        session: CANONICAL_SAMPLES[0].session,
        actor: CANONICAL_SAMPLES[0].role,
        record: CANONICAL_SAMPLES[0].record,
      },
      {
        source: "claude-code",
        session: CANONICAL_SAMPLES[1].session,
        actor: CANONICAL_SAMPLES[1].role,
        record: CANONICAL_SAMPLES[1].record,
      },
    ]);
    expect(origins(fromNested)).toEqual(origins(fromFlat));
  });

  /** A directory the raw tree keeps beside the transcripts is not one. */
  test("reads no conversation out of a directory that holds none", async () => {
    const root = await fixtureRoot();
    await mkdir(join(root, "home", ".claude", "jobs", "6710737a"), {
      recursive: true,
    });
    await writeFile(
      join(root, "home", ".claude", "jobs", "6710737a", "timeline.jsonl"),
      claudeLine({
        type: "user",
        uuid: "job-1",
        timestamp: "2026-05-01T00:00:01.000Z",
        message: { role: "user", content: "a job timeline entry" },
      }),
    );

    expect(await collectClaudeRaw(root)).toEqual([]);
  });

  test("orders every transcript by event time and tie-breaks on the record", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "sub-1",
      "a1-early",
      "a1-late",
      "sub-2:content:1",
      "sub-compact",
      "wf-1",
      "a2-whole",
      "a2-mixed:content:1",
      "a2-mixed:content:3",
      "b1-secret",
      "b1-summary",
      "b1-long",
      "b2-aaa",
      "b2-tie",
      CANONICAL_SAMPLES[0].record,
      CANONICAL_SAMPLES[1].record,
    ]);
  });

  test("carries the session context onto every turn that inherits it", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);
    const early = episodes.find((e) => e.input.origin.record === "a1-early");
    const late = episodes.find((e) => e.input.origin.record === "a1-late");

    expect(early?.input.origin.session).toBe("session-a1");
    expect(early?.input.properties).toEqual({
      kind: "agent_message",
      cwd: "/Users/ino/alpha",
      git_branch: "main",
    });
    expect(late?.input.properties).toEqual({ kind: "agent_message" });
    expect(late?.input.origin.actor).toBe("assistant");
  });

  test("keeps prose and drops the plumbing the export rejected", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);
    const records = episodes.map((e) => e.input.origin.record);

    expect(records).not.toContain("a1-side");
    expect(records).not.toContain("a1-error");
    expect(records).not.toContain("a1-tool");
    expect(records).not.toContain("a1-result");
    expect(records).not.toContain("a1-empty");
    expect(records).not.toContain("sidecar");
    expect(records).not.toContain("sub-sidecar");
    expect(records).not.toContain("job-1");
    expect(records).not.toContain("a2-blockless");
    expect(
      episodes.find((e) => e.input.origin.record === "a2-whole")?.input.content,
    ).toBe("one half\nother half");
    expect(
      episodes.find((e) => e.input.origin.record === "a2-mixed:content:1")
        ?.input.content,
    ).toBe("first shipped block");
  });

  test("keeps a compaction summary as the record it stands in for", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);
    const summary = episodes.find(
      (e) => e.input.origin.record === "b1-summary",
    );

    expect(summary?.input.content).toBe("the session so far, compacted");
    expect(summary?.input.properties).toEqual({ kind: "compaction" });
    expect(summary?.input.origin.actor).toBe("unknown");
  });

  test("masks secrets and stores an oversized turn as a payload", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);
    const secret = episodes.find((e) => e.input.origin.record === "b1-secret");
    const long = episodes.find((e) => e.input.origin.record === "b1-long");

    expect(secret?.redactions).toBe(1);
    expect(secret?.input.content).toBe(`deploy with ${REDACTION}`);
    expect(secret?.input.payload).toBeUndefined();
    expect(long?.input.content).toBe("z".repeat(4000));
    expect(long?.input.payload).toEqual(new TextEncoder().encode(LONG_TURN));
    expect(long?.input.payload_media_type).toBe("text/plain");
    expect(long?.redactions).toBe(0);
  });

  test("skips a record no reader can parse or place in the order", async () => {
    const root = await fixtureRoot();
    await writeFile(
      join(root, "session-torn.jsonl"),
      [
        '{"type":"user","uuid":"torn","timestamp":"2026-05-05T00:00:00.000Z",',
        claudeLine({ type: "user", uuid: "no-time", message: { role: "user", content: "no event time" } }),
        claudeLine({ type: "user", uuid: "role-clash", timestamp: "2026-05-05T00:00:01.000Z", message: { role: "assistant", content: "role disagrees with type" } }),
        claudeLine({ type: "summary", uuid: "blank", timestamp: "2026-05-05T00:00:02.000Z", summary: "" }),
        "[1,2,3]",
        claudeLine({ type: "user", uuid: "kept", timestamp: "2026-05-05T00:00:03.000Z", message: { role: "user", content: "the only survivor" } }),
      ].join("\n"),
    );

    const episodes = await collectClaudeRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["kept"]);
    expect(episodes[0]?.input.origin.session).toBe("session-torn");
  });

  test("hashes the exact bytes so an id survives a re-encoded transcript", async () => {
    const root = await fixtureRoot();
    await mkdir(join(root, "transcripts"), { recursive: true });
    const line = claudeLine({
      type: "user",
      timestamp: "2026-05-06T00:00:00.000Z",
      content: "no native id anywhere",
    });
    await writeFile(
      join(root, "transcripts", "ses_fallback.jsonl"),
      `${claudeLine({ type: "tool_use", timestamp: "2026-05-06T00:00:00.000Z", tool_name: "Bash" })}\n${line}\n`,
    );

    const [episode] = await collectClaudeRaw(root);

    expect(episode?.input.origin.record).toBe(
      `fallback:${bytesToHex(blake3(new TextEncoder().encode(line)))}:1`,
    );
  });

  test("follows a resumed session onto the id it renames itself to", async () => {
    const root = await fixtureRoot();
    await writeFile(
      join(root, "old-name.jsonl"),
      claudeLine({
        type: "user",
        uuid: "resumed",
        timestamp: "2026-05-07T00:00:00.000Z",
        sessionId: "new-name",
        message: { role: "user", content: "resumed under a new id" },
      }),
    );

    const [episode] = await collectClaudeRaw(root);

    expect(episode?.input.origin.session).toBe("new-name");
  });

  test("gives every delegated transcript a session of its own", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);
    const sub = episodes.find((e) => e.input.origin.record === "sub-1");
    const reply = episodes.find(
      (e) => e.input.origin.record === "sub-2:content:1",
    );
    const step = episodes.find((e) => e.input.origin.record === "wf-1");

    /**
     * A delegate names its parent in `sessionId`, so keying on that would
     * thread every delegation onto the parent's single spine; the agent id is
     * the transcript's own identity and the parent is kept as a property.
     */
    expect(sub?.input.origin).toEqual({
      source: "claude-code",
      session: "deadbeef",
      actor: "user",
      record: "sub-1",
    });
    expect(step?.input.origin.session).toBe("cafebabe");
    /** A delegate's turns run through the same slicing rule as a main one. */
    expect(reply?.input.origin.actor).toBe("assistant");
    expect(reply?.input.origin.session).toBe("deadbeef");
    expect(sub?.input.content).toBe("the delegate's own transcript");
    expect(reply?.input.content).toBe("what the delegate reported back");
    expect(step?.input.content).toBe("the workflow step's own prompt");
    /** The workflow journal is orchestration bookkeeping, not a transcript. */
    expect(
      episodes.filter((e) => e.input.origin.session === "journal"),
    ).toEqual([]);
  });

  test("marks a delegated turn with the delegation it belongs to", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);
    const sub = episodes.find((e) => e.input.origin.record === "sub-1");
    const compact = episodes.find(
      (e) => e.input.origin.record === "sub-compact",
    );
    const step = episodes.find((e) => e.input.origin.record === "wf-1");

    expect(sub?.input.properties).toEqual({
      kind: "agent_message",
      cwd: "/Users/ino/alpha",
      git_branch: "main",
      is_sidechain: "true",
      agent_id: "deadbeef",
      parent_session_id: "session-a1",
      transcript_kind: "subagent",
    });
    expect(step?.input.properties).toEqual({
      kind: "agent_message",
      cwd: "/Users/ino/alpha",
      is_sidechain: "true",
      agent_id: "cafebabe",
      parent_session_id: "session-a1",
      transcript_kind: "workflow",
    });
    /**
     * A compaction boundary names no session of its own, so the parent it
     * hangs off is the one the transcript has already established.
     */
    expect(compact?.input.properties).toEqual({
      kind: "compaction",
      cwd: "/Users/ino/alpha",
      git_branch: "main",
      is_sidechain: "true",
      agent_id: "deadbeef",
      parent_session_id: "session-a1",
      transcript_kind: "subagent",
    });
    expect(compact?.input.content).toBe("the delegation so far, compacted");
  });

  test("leaves every main-transcript episode exactly as it was", async () => {
    const root = await claudeRawRoot();

    const episodes = await collectClaudeRaw(root);
    const main = episodes.filter(
      (episode) => episode.input.properties?.["is_sidechain"] === undefined,
    );

    /**
     * The sidechain records the raw tree holds live only under `subagents/`,
     * so a flagged record inside a main transcript stays rejected: admitting
     * it would rewrite output the normalized export already ingested. This is
     * every main-transcript episode as the adapter produced them before the
     * delegated transcripts were admitted.
     */
    expect(
      main.map((episode) => ({
        ...episode.input.origin,
        time: episode.input.time?.value,
        content: episode.input.content,
        properties: episode.input.properties,
      })),
    ).toEqual([
      { source: "claude-code", session: "session-a1", actor: "user", record: "a1-early", time: "2026-05-01T00:00:03.000Z", content: "the earlier question", properties: { kind: "agent_message", cwd: "/Users/ino/alpha", git_branch: "main" } },
      { source: "claude-code", session: "session-a1", actor: "assistant", record: "a1-late", time: "2026-05-01T00:00:09.000Z", content: "the later answer", properties: { kind: "agent_message" } },
      { source: "claude-code", session: "session-a2", actor: "assistant", record: "a2-whole", time: "2026-05-02T00:00:01.000Z", content: "one half\nother half", properties: { kind: "agent_message" } },
      { source: "claude-code", session: "session-a2", actor: "assistant", record: "a2-mixed:content:1", time: "2026-05-02T00:00:02.000Z", content: "first shipped block", properties: { kind: "agent_message" } },
      { source: "claude-code", session: "session-a2", actor: "assistant", record: "a2-mixed:content:3", time: "2026-05-02T00:00:02.000Z", content: "second shipped block", properties: { kind: "agent_message" } },
      { source: "claude-code", session: "session-b1", actor: "user", record: "b1-secret", time: "2026-05-03T00:00:01.000Z", content: `deploy with ${REDACTION}`, properties: { kind: "agent_message" } },
      { source: "claude-code", session: "session-b1", actor: "unknown", record: "b1-summary", time: "2026-05-03T00:00:02.000Z", content: "the session so far, compacted", properties: { kind: "compaction" } },
      { source: "claude-code", session: "session-b1", actor: "assistant", record: "b1-long", time: "2026-05-03T00:00:03.000Z", content: "z".repeat(4000), properties: { kind: "agent_message" } },
      { source: "claude-code", session: "session-b2", actor: "user", record: "b2-aaa", time: "2026-05-04T00:00:00.000Z", content: "tie one", properties: { kind: "agent_message" } },
      { source: "claude-code", session: "session-b2", actor: "user", record: "b2-tie", time: "2026-05-04T00:00:00.000Z", content: "tie two", properties: { kind: "agent_message" } },
      { source: "claude-code", session: CANONICAL_SAMPLES[0].session, actor: "user", record: CANONICAL_SAMPLES[0].record, time: "2026-05-30T05:03:48.912Z", content: CANONICAL_SAMPLES[0].text, properties: { kind: "agent_message" } },
      { source: "claude-code", session: CANONICAL_SAMPLES[1].session, actor: "user", record: CANONICAL_SAMPLES[1].record, time: "2026-06-17T09:17:00.521Z", content: CANONICAL_SAMPLES[1].text, properties: { kind: "agent_message" } },
    ]);
  });

  test("ingests one unbroken session chain per raw transcript", async () => {
    const root = await claudeRawRoot();
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const episodes = await collectClaudeRaw(root);
      const ids: string[] = [];
      for (const episode of episodes) {
        ids.push((await engine.remember(episode.input)).id);
      }

      const heads: string[] = [];
      for (const [index, id] of ids.entries()) {
        const inbound = await engine.store.linksOf(id, "NEXT_EPISODE");
        if (inbound.every((link) => link.to !== id)) {
          heads.push(episodes[index]?.input.origin.session ?? "");
        }
      }
      const sessions = episodes.map((e) => e.input.origin.session);

      expect(heads.sort()).toEqual([...new Set(sessions)].sort());
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
  });
});

interface CodexRaw {
  timestamp: string;
  type: string;
  payload: Record<string, unknown>;
}

function codexLines(records: CodexRaw[]): string {
  return `${records.map((record) => JSON.stringify(record)).join("\n")}\n`;
}

function userMessage(text: string): Record<string, unknown> {
  return {
    type: "message",
    role: "user",
    content: [{ type: "input_text", text }],
  };
}

function assistantMessage(text: string): Record<string, unknown> {
  return {
    type: "message",
    role: "assistant",
    content: [{ type: "output_text", text }],
  };
}

/**
 * One real turn copied out of the snapshot's rollout tree together with the
 * canonical line the normalized export produced for it, so the alignment
 * assertion is anchored on observed bytes rather than on a restatement of the
 * adapter's own rules. Session `019cd33d-…` line 9, secret-free.
 */
const REAL_TEXT =
  "I\u2019m pulling the issue text and the current docs/code so I can verify each requested correction against the repository before touching the markdown.";
const REAL_SESSION = "019cd33d-e40f-7da3-a19f-15360a9c3c36";
const REAL_TIMESTAMP = "2026-03-09T15:36:22.283Z";
const REAL_OCCURRED_AT = 1773070582283;

const CODEX_LONG = "z".repeat(4200);

/**
 * Two sessions written out of event-time order, spanning every record kind the
 * export retained and a representative sample of the kinds it rejected, plus
 * an AppleDouble sidecar and a truncated tail line.
 */
async function codexRawRoot(scope = "fixed"): Promise<string> {
  const root = await fixtureRoot();
  const day = join(root, "2026", "03", "10");
  await mkdir(day, { recursive: true });
  const sessionB = `019cd33d-0000-7000-8000-${scope.slice(0, 12).padEnd(12, "0")}`;

  await writeFile(
    join(day, `._rollout-2026-03-10T00-36-14-${REAL_SESSION}.jsonl`),
    "Mac OS X            \u0000\u0000\u0000\u0000",
  );
  await writeFile(
    join(day, `rollout-2026-03-10T00-36-14-${REAL_SESSION}.jsonl`),
    codexLines([
      {
        timestamp: "2026-03-09T15:36:16.548Z",
        type: "session_meta",
        payload: {
          id: REAL_SESSION,
          cwd: "/Users/ino/Company/timetree-planner-agent",
        },
      },
      {
        timestamp: "2026-03-09T15:36:16.754Z",
        type: "response_item",
        payload: {
          type: "message",
          role: "developer",
          content: [{ type: "input_text", text: "harness prompt" }],
        },
      },
      {
        timestamp: "2026-03-09T15:36:16.754Z",
        type: "turn_context",
        payload: { model: "gpt-5.4", cwd: "/ignored" },
      },
      {
        timestamp: "2026-03-09T15:36:30.000Z",
        type: "response_item",
        payload: assistantMessage(CODEX_LONG),
      },
      {
        timestamp: "2026-03-09T15:36:16.900Z",
        type: "event_msg",
        payload: { type: "user_message", message: "start the backfill" },
      },
      {
        timestamp: REAL_TIMESTAMP,
        type: "event_msg",
        payload: { type: "agent_message", message: REAL_TEXT },
      },
      {
        timestamp: "2026-03-09T15:36:24.000Z",
        type: "response_item",
        payload: { type: "reasoning", summary: [{ text: "thinking" }] },
      },
      {
        timestamp: "2026-03-09T15:36:25.000Z",
        type: "response_item",
        payload: { type: "function_call", name: "shell", arguments: "{}" },
      },
      {
        timestamp: "2026-03-09T15:36:26.000Z",
        type: "event_msg",
        payload: { type: "token_count", total: 12 },
      },
      {
        timestamp: "2026-03-09T15:36:27.000Z",
        type: "response_item",
        payload: assistantMessage("   "),
      },
      {
        timestamp: "2026-03-09T15:36:40.000Z",
        type: "event_msg",
        payload: { type: "context_compacted" },
      },
    ]),
  );

  await writeFile(
    join(day, `rollout-2026-03-10T00-40-00-${sessionB}.jsonl`),
    `${codexLines([
      {
        timestamp: "2026-03-09T15:40:00.000Z",
        type: "session_meta",
        payload: { id: sessionB },
      },
      {
        timestamp: "2026-03-09T15:40:01.000Z",
        type: "response_item",
        payload: {
          ...userMessage("use ghp_0123456789012345678901234567890123456789"),
          id: "msg_native",
        },
      },
      {
        timestamp: "not-a-date",
        type: "response_item",
        payload: assistantMessage("undateable"),
      },
    ])}{"timestamp":"2026-03-09T15:40:02.000Z","type":"resp\n`,
  );
  return root;
}

describe("collectCodexRaw", () => {
  test("mirrors the normalized export's origin and revision for a real turn", async () => {
    const root = await codexRawRoot();

    const episodes = await collectCodexRaw(root);
    const real = episodes.find((e) => e.input.content === REAL_TEXT);

    expect(real?.input.origin).toEqual({
      source: "codex",
      session: REAL_SESSION,
      actor: "assistant",
      record: `${REAL_SESSION}:5`,
    });
    expect(real?.input.time).toEqual({
      value: new Date(REAL_OCCURRED_AT).toISOString(),
      precision: "second",
    });
    /** The agent-log adapter's key, recomputed from the canonical fields. */
    expect(real?.input.source_revision).toBe(
      createHash("sha256")
        .update(
          `${new Date(REAL_OCCURRED_AT).toISOString()}\n${REAL_TEXT}`,
          "utf8",
        )
        .digest("hex"),
    );
    expect(real?.input.properties).toEqual({
      kind: "message",
      canonical_kind: "agent_message",
      cwd: "/Users/ino/Company/timetree-planner-agent",
      model: "gpt-5.4",
    });
    expect(real?.input.schema).toBe("anamnesis.original-message/1");
  });

  test("orders every session by event time and keeps the compaction marker", async () => {
    const root = await codexRawRoot();

    const episodes = await collectCodexRaw(root);

    expect(episodes.map((e) => e.input.content)).toEqual([
      "start the backfill",
      REAL_TEXT,
      "z".repeat(4000),
      "context_compacted",
      `use ${REDACTION}`,
    ]);
    expect(episodes[3]?.input.properties).toEqual({
      kind: "compaction",
      canonical_kind: "compaction",
      cwd: "/Users/ino/Company/timetree-planner-agent",
      model: "gpt-5.4",
    });
    expect(episodes[3]?.input.origin.actor).toBe("unknown");
  });

  test("reproduces the export's native event id when the record carries one", async () => {
    const root = await codexRawRoot();

    const episodes = await collectCodexRaw(root);
    const native = episodes.find((e) => e.input.content === `use ${REDACTION}`);

    expect(native?.input.origin.record).toBe("msg_native:content:0");
    expect(native?.redactions).toBe(1);
    expect(native?.input.origin.actor).toBe("user");
  });

  test("stores an oversized turn as a payload with an excerpt on the node", async () => {
    const root = await codexRawRoot();

    const episodes = await collectCodexRaw(root);
    const long = episodes[2];

    expect(long?.input.content).toBe("z".repeat(4000));
    expect(long?.input.payload).toEqual(new TextEncoder().encode(CODEX_LONG));
    expect(long?.input.payload_media_type).toBe("text/plain");
    expect(episodes[0]?.input.payload).toBeUndefined();
  });

  test("keys the revision on raw text so masking changes open none", async () => {
    const root = await codexRawRoot();
    const secret = "ghp_0123456789012345678901234567890123456789";

    const episodes = await collectCodexRaw(root);
    const masked = episodes[4];

    expect(masked?.input.content).toBe(`use ${REDACTION}`);
    expect(masked?.input.source_revision).toBe(
      createHash("sha256")
        .update(`2026-03-09T15:40:01.000Z\nuse ${secret}`, "utf8")
        .digest("hex"),
    );
  });

  test("tie-breaks equal event times on the record so the order is total", async () => {
    const root = await fixtureRoot();
    const session = "019cd33d-1111-7000-8000-000000000000";
    await writeFile(
      join(root, `rollout-2026-03-10T00-36-14-${session}.jsonl`),
      codexLines([
        {
          timestamp: "2026-03-09T15:36:16.000Z",
          type: "response_item",
          payload: { ...userMessage("tie b"), id: "msg_b" },
        },
        {
          timestamp: "2026-03-09T15:36:16.000Z",
          type: "response_item",
          payload: { ...userMessage("tie a"), id: "msg_a" },
        },
      ]),
    );

    const episodes = await collectCodexRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "msg_a:content:0",
      "msg_b:content:0",
    ]);
  });

  test("falls back to the file name when a session carries no meta record", async () => {
    const root = await fixtureRoot();
    const session = "019cd33d-2222-7000-8000-000000000000";
    await writeFile(
      join(root, `rollout-2026-03-10T00-36-14-${session}.jsonl`),
      codexLines([
        {
          timestamp: "2026-03-09T15:36:16.000Z",
          type: "session_meta",
          payload: { note: "no id" },
        },
        {
          timestamp: "2026-03-09T15:36:17.000Z",
          type: "event_msg",
          payload: { type: "agent_message", message: "orphan turn" },
        },
      ]),
    );

    const [episode] = await collectCodexRaw(root);

    expect(episode?.input.origin.session).toBe(session);
    expect(episode?.input.origin.record).toBe(`${session}:1`);
    expect(episode?.input.properties).toEqual({
      kind: "message",
      canonical_kind: "agent_message",
    });
  });

  test("skips rollout lines that are not a well-formed record", async () => {
    const root = await fixtureRoot();
    const session = "019cd33d-3333-7000-8000-000000000000";
    await writeFile(
      join(root, `rollout-2026-03-10T00-36-14-${session}.jsonl`),
      [
        "[1,2,3]",
        '"a bare string"',
        "null",
        JSON.stringify({ timestamp: "2026-03-09T15:36:16.000Z", type: "event_msg" }),
        JSON.stringify({
          timestamp: "2026-03-09T15:36:16.000Z",
          type: "event_msg",
          payload: ["not an object"],
        }),
        JSON.stringify({
          type: "event_msg",
          payload: { type: "agent_message", message: "no timestamp" },
        }),
        JSON.stringify({
          timestamp: "2026-03-09T15:36:17.000Z",
          type: "response_item",
          payload: {
            type: "message",
            role: "assistant",
            content: ["not a part", { type: "output_text", text: "survivor" }],
          },
        }),
        "",
      ].join("\n"),
    );

    const episodes = await collectCodexRaw(root);

    expect(episodes.map((e) => e.input.content)).toEqual(["survivor"]);
  });

  test("ingests one unbroken session chain per rollout file", async () => {
    const root = await codexRawRoot(`chain${Date.now()}`);
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const episodes = await collectCodexRaw(root);
      const ids: string[] = [];
      for (const episode of episodes) {
        ids.push((await engine.remember(episode.input)).id);
      }

      const heads: string[] = [];
      for (const [index, id] of ids.entries()) {
        const inbound = await engine.store.linksOf(id, "NEXT_EPISODE");
        if (inbound.every((link) => link.to !== id)) {
          heads.push(episodes[index]?.input.origin.session ?? "");
        }
      }
      const sessions = episodes.map((e) => e.input.origin.session);

      expect(heads.sort()).toEqual([...new Set(sessions)].sort());
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
  });
});

interface AsideMessage {
  role: string;
  content?: unknown;
  timestamp?: number;
  attachments?: unknown[];
  toolCallId?: string;
  kind?: string;
}

function jsonLines(records: unknown[]): string {
  return `${records.map((record) => JSON.stringify(record)).join("\n")}\n`;
}

const MISC_LONG = "y".repeat(4300);

/**
 * The snapshot's own store names, one directory per agent product, carrying
 * one small fixture per store the adapter ingests beside the diagnostics-only
 * stores it has to leave alone. The samples in the skipped stores are copied
 * from the snapshot rather than invented, so the skip assertions fail if the
 * adapter ever starts reading a log file that only holds telemetry.
 */
async function miscRawRoot(): Promise<string> {
  const root = await fixtureRoot();

  const asideUser = join(root, "aside", "home", ".aside", "u", "0");
  const asideSession = join(
    asideUser,
    "sessions",
    "2026-06-30_6K1bOZC03YTVbDpP",
  );
  await mkdir(asideSession, { recursive: true });
  await writeFile(
    join(asideSession, "messages.jsonl"),
    jsonLines([
      {
        role: "user-message-metadata",
        attachments: [{ id: "tab:41C4", type: "tab" }],
        timestamp: 1_782_794_590_500,
      },
      {
        role: "user",
        content: "help me build the recipe deck",
        timestamp: 1_782_794_590_501,
      },
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "provider scratch space" },
          { type: "text", text: "sure, let me look at the page first" },
          { type: "toolCall", id: "toolu_01", name: "repl", arguments: {} },
        ],
        timestamp: 1_782_794_591_000,
      },
      {
        role: "toolResult",
        content: [{ type: "text", text: "tab listing output" }],
        timestamp: 1_782_794_591_500,
        toolCallId: "toolu_01",
      },
      {
        role: "system-message",
        content: "Relevant skill docs are available.",
        kind: "site_skill",
        timestamp: 1_782_794_592_000,
      },
      { role: "assistant", content: [], timestamp: 1_782_794_592_500 },
      { role: "user", content: "   ", timestamp: 1_782_794_593_000 },
      { role: "user", content: "no timestamp here" },
      ["a bare array, not a record"],
      {
        role: "user",
        content: `use ghp_0123456789012345678901234567890123456789`,
        timestamp: 1_782_794_594_000,
      },
      { role: "assistant", content: MISC_LONG, timestamp: 1_782_794_595_000 },
    ] satisfies (AsideMessage | string[])[]),
  );
  await writeFile(join(asideSession, "._messages.jsonl"), "Mac OS X\u0000");
  const state = new Database(join(asideUser, "state.db"));
  state.run(
    "create table sessions (id text primary key, title text not null, cwd text not null)",
  );
  state.run("insert into sessions values (?, ?, ?)", [
    "6K1bOZC03YTVbDpP",
    "recipe deck",
    "/Users/ino/.aside/u/0/sessions/2026-06-30_6K1bOZC03YTVbDpP",
  ]);
  state.close();
  await writeFile(join(asideUser, "._state.db"), "Mac OS X\u0000");

  const brain = join(
    root,
    "gemini-antigravity",
    "home",
    ".gemini",
    "antigravity-cli",
    "brain",
    "3c119fe5-8550-4230-ba74-63f52e82568e",
    ".system_generated",
    "logs",
  );
  await mkdir(brain, { recursive: true });
  await writeFile(
    join(brain, "transcript.jsonl"),
    [
      jsonLines([
        {
          step_index: 0,
          source: "USER_EXPLICIT",
          type: "USER_INPUT",
          status: "DONE",
          created_at: "2026-06-14T07:35:41Z",
          content:
            "<USER_REQUEST>\nCREATE-FLOW.md is this the doc?\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-06-14T16:35:41+09:00.\n</ADDITIONAL_METADATA>",
        },
        {
          step_index: 1,
          source: "SYSTEM",
          type: "CONVERSATION_HISTORY",
          status: "DONE",
          created_at: "2026-06-14T07:35:41Z",
        },
        {
          step_index: 2,
          source: "MODEL",
          type: "PLANNER_RESPONSE",
          status: "DONE",
          created_at: "2026-06-14T07:35:42Z",
          content: "I will start by exploring the workspace.",
          tool_calls: [{ name: "list_dir", args: {} }],
        },
        {
          step_index: 3,
          source: "MODEL",
          type: "RUN_COMMAND",
          status: "DONE",
          created_at: "2026-06-14T07:35:43Z",
          content: "Created At: 2026-06-14T07:35:43Z\nTask Description: find …",
        },
        {
          step_index: 4,
          source: "SYSTEM",
          type: "EPHEMERAL_MESSAGE",
          status: "DONE",
          created_at: "2026-06-14T07:35:44Z",
          content:
            "The following is an <EPHEMERAL_MESSAGE> not actually sent by the user.",
        },
        {
          step_index: 5,
          source: "USER_EXPLICIT",
          type: "USER_INPUT",
          status: "DONE",
          created_at: "2026-06-14T07:35:45Z",
          content: "<USER_REQUEST>\n\n</USER_REQUEST>",
        },
        {
          step_index: 6,
          source: "MODEL",
          type: "PLANNER_RESPONSE",
          status: "DONE",
          created_at: "not a timestamp",
          content: "unplaceable in the session order",
        },
      ]),
      "{ truncated mid-write\n",
    ].join(""),
  );
  await writeFile(
    join(brain, "transcript_full.jsonl"),
    jsonLines([
      {
        step_index: 0,
        source: "USER_EXPLICIT",
        type: "USER_INPUT",
        status: "DONE",
        created_at: "2026-06-14T07:35:41Z",
        content: "<USER_REQUEST>\nCREATE-FLOW.md is this the doc?\n</USER_REQUEST>",
      },
    ]),
  );

  const opencode = join(
    root,
    "opencode",
    "home",
    ".local",
    "state",
    "opencode",
  );
  await mkdir(opencode, { recursive: true });
  await writeFile(
    join(opencode, "prompt-history.jsonl"),
    jsonLines([
      { input: "compare the two videos by sha256", parts: [], mode: "normal" },
      {
        input: "[Pasted ~64 lines] is this right?",
        parts: [
          { type: "text", text: "the pasted review comment" },
          { type: "file", filename: "note.md" },
        ],
        mode: "normal",
      },
      { input: "   ", parts: [], mode: "normal" },
      { parts: [], mode: "normal" },
    ]),
  );
  await writeFile(
    join(opencode, "frecency.jsonl"),
    jsonLines([{ path: "/Users/ino/Develop/pss-mgba", frequency: 1 }]),
  );
  await utimes(
    join(opencode, "prompt-history.jsonl"),
    new Date(1_784_000_000_000),
    new Date(1_784_000_000_000),
  );

  const cursorLogs = join(
    root,
    "cursor",
    "home",
    "Library",
    "Application Support",
    "Cursor",
    "logs",
    "20260521T152729",
  );
  await mkdir(cursorLogs, { recursive: true });
  await writeFile(
    join(cursorLogs, "Claude VSCode.log"),
    "2026-05-21 15:27:33.909 [info] MCP Server running on port 12920 (localhost only)\n",
  );

  const codexLogs = join(
    root,
    "codex-desktop",
    "home",
    "Library",
    "Logs",
    "com.openai.codex",
  );
  await mkdir(codexLogs, { recursive: true });
  await writeFile(
    join(codexLogs, "codex-desktop-1-t0.log"),
    "2026-07-16T03:30:48.438Z error [electron-message-handler] Conversation state not found conversationId=019f68ec\n",
  );

  const openomni = join(root, "openomni", "home", ".openomni");
  await mkdir(openomni, { recursive: true });
  const storage = new Database(join(openomni, "storage.db"));
  storage.run(
    "create table message (id text primary key, session_id text, data text, role text, time_created integer)",
  );
  storage.close();

  const claudeProject = join(root, "claude-project", "home", "data");
  await mkdir(claudeProject, { recursive: true });
  const calendars = new Database(join(claudeProject, "raw.db"));
  calendars.run(
    "create table calendars (calendar_id integer primary key, name text, purpose text)",
  );
  calendars.run("insert into calendars values (?, ?, ?)", [
    1_915_182,
    "private",
    "other",
  ]);
  calendars.close();

  const zedLogs = join(root, "zed", "home", "Library", "Logs", "Zed");
  await mkdir(zedLogs, { recursive: true });
  await writeFile(
    join(zedLogs, "Zed.log"),
    "2026-09-03T18:20:27+09:00 INFO  [zed::reliability] memory usage: resident 396 MiB (+5 MiB)\n",
  );

  const ouroLogs = join(root, "ouro", "home", ".ouroboros", "logs");
  await mkdir(ouroLogs, { recursive: true });
  await writeFile(
    join(ouroLogs, "ouroboros.log"),
    "2026-06-09T06:33:54.035022Z [info] mcp.tool.qa iteration=1 pass_threshold=0.8\n",
  );

  const anamnesisLogs = join(root, "anamnesis", "home", "Library", "Logs");
  await mkdir(anamnesisLogs, { recursive: true });
  await writeFile(
    join(anamnesisLogs, "omni-anamnesis-sync.log"),
    "tar: Ignoring unknown extended header keyword 'LIBARCHIVE.xattr.com.apple.provenance'\n",
  );

  await writeFile(join(root, "._aside"), "Mac OS X\u0000");
  await mkdir(join(root, "kodu"), { recursive: true });
  await writeFile(join(root, "kodu", "tasks.json"), "{}");

  return root;
}

describe("collectMiscRaw", () => {
  test("ingests only the stores that carry conversation, in event-time order", async () => {
    const root = await miscRawRoot();

    const episodes = await collectMiscRaw(root);

    expect(episodes.map((e) => e.input.origin.source)).toEqual([
      "gemini-antigravity",
      "gemini-antigravity",
      "aside",
      "aside",
      "aside",
      "aside",
      "opencode",
      "opencode",
    ]);
    expect(episodes.map((e) => e.input.time?.value)).toEqual([
      "2026-06-14T07:35:41.000Z",
      "2026-06-14T07:35:42.000Z",
      "2026-06-30T04:43:10.501Z",
      "2026-06-30T04:43:11.000Z",
      "2026-06-30T04:43:14.000Z",
      "2026-06-30T04:43:15.000Z",
      "2026-07-14T03:33:20.000Z",
      "2026-07-14T03:33:20.000Z",
    ]);
  });

  test("maps the aside origin onto the session id the product's own index joins on", async () => {
    const root = await miscRawRoot();

    const episodes = await collectMiscRaw(root);
    const aside = episodes.filter((e) => e.input.origin.source === "aside");

    expect(aside[0]?.input.origin).toEqual({
      source: "aside",
      session: "6K1bOZC03YTVbDpP",
      actor: "user",
      record: "6K1bOZC03YTVbDpP:1",
    });
    expect(aside[0]?.input.content).toBe("help me build the recipe deck");
    expect(aside[0]?.input.properties).toEqual({
      kind: "message",
      session_title: "recipe deck",
      cwd: "/Users/ino/.aside/u/0/sessions/2026-06-30_6K1bOZC03YTVbDpP",
    });
    expect(aside[0]?.input.schema).toBe("anamnesis.original-message/1");
  });

  test("keeps aside prose and drops thinking, tool calls and harness messages", async () => {
    const root = await miscRawRoot();

    const episodes = await collectMiscRaw(root);
    const aside = episodes.filter((e) => e.input.origin.source === "aside");

    expect(aside.map((e) => e.input.content)).toEqual([
      "help me build the recipe deck",
      "sure, let me look at the page first",
      `use ${REDACTION}`,
      "y".repeat(4000),
    ]);
  });

  test("unwraps the antigravity user request from the metadata wrapped around it", async () => {
    const root = await miscRawRoot();

    const episodes = await collectMiscRaw(root);
    const antigravity = episodes.filter(
      (e) => e.input.origin.source === "gemini-antigravity",
    );

    expect(antigravity.map((e) => e.input.content)).toEqual([
      "CREATE-FLOW.md is this the doc?",
      "I will start by exploring the workspace.",
    ]);
    expect(antigravity[0]?.input.origin).toEqual({
      source: "gemini-antigravity",
      session: "3c119fe5-8550-4230-ba74-63f52e82568e",
      actor: "user",
      record: "3c119fe5-8550-4230-ba74-63f52e82568e:0",
    });
    expect(antigravity[0]?.input.properties).toEqual({ kind: "USER_INPUT" });
    expect(antigravity[1]?.input.origin.actor).toBe("assistant");
    expect(antigravity[1]?.input.properties).toEqual({
      kind: "PLANNER_RESPONSE",
    });
  });

  test("appends the elided paste to the opencode prompt that carried it", async () => {
    const root = await miscRawRoot();

    const episodes = await collectMiscRaw(root);
    const opencode = episodes.filter(
      (e) => e.input.origin.source === "opencode",
    );

    expect(opencode.map((e) => e.input.content)).toEqual([
      "compare the two videos by sha256",
      "[Pasted ~64 lines] is this right?\nthe pasted review comment",
    ]);
    expect(opencode[1]?.input.origin).toEqual({
      source: "opencode",
      session: "prompt-history",
      actor: "user",
      record: "prompt-history:1",
    });
    expect(opencode[1]?.input.properties).toEqual({
      kind: "prompt",
      mode: "normal",
    });
    /** The history carries no per-entry time; the file's own is all there is. */
    expect(opencode[0]?.input.time).toEqual({
      value: new Date(1_784_000_000_000).toISOString(),
      precision: "second",
    });
  });

  test("keys the revision on raw text so masking changes open none", async () => {
    const root = await miscRawRoot();
    const secret = "ghp_0123456789012345678901234567890123456789";

    const episodes = await collectMiscRaw(root);
    const masked = episodes.find((e) => e.input.content === `use ${REDACTION}`);

    expect(masked?.redactions).toBe(1);
    expect(masked?.input.source_revision).toBe(
      createHash("sha256")
        .update(
          `${new Date(1_782_794_594_000).toISOString()}\nuse ${secret}`,
          "utf8",
        )
        .digest("hex"),
    );
  });

  test("stores an oversized turn as a payload with an excerpt on the node", async () => {
    const root = await miscRawRoot();

    const episodes = await collectMiscRaw(root);
    const long = episodes.find((e) => e.input.payload !== undefined);

    expect(long?.input.content).toBe("y".repeat(4000));
    expect(long?.input.payload).toEqual(new TextEncoder().encode(MISC_LONG));
    expect(long?.input.payload_media_type).toBe("text/plain");
    expect(episodes[0]?.input.payload).toBeUndefined();
  });

  test("reads none of the stores whose files hold only diagnostics or caches", async () => {
    const root = await miscRawRoot();

    const episodes = await collectMiscRaw(root);

    expect(
      episodes.filter((e) =>
        [
          "cursor",
          "codex-desktop",
          "zed",
          "ouro",
          "anamnesis",
          "openomni",
          "claude-project",
          "kodu",
        ].includes(e.input.origin.source),
      ),
    ).toEqual([]);
  });

  test("leaves the dataset directory byte-identical after a backfill", async () => {
    const root = await miscRawRoot();
    const asideUser = join(root, "aside", "home", ".aside", "u", "0");
    const before = await stat(join(asideUser, "state.db"));

    await collectMiscRaw(root);

    const after = await stat(join(asideUser, "state.db"));
    expect(after.mtimeMs).toBe(before.mtimeMs);
    expect(after.size).toBe(before.size);
    expect(
      (await readdir(asideUser))
        .filter((name) => name.includes("state.db"))
        .sort(),
    ).toEqual(["._state.db", "state.db"]);
  });

  test("tie-breaks equal event times on the record so the order is total", async () => {
    const root = await fixtureRoot();
    const session = join(
      root,
      "aside",
      "home",
      ".aside",
      "u",
      "0",
      "sessions",
      "2026-06-30_tie",
    );
    await mkdir(session, { recursive: true });
    await writeFile(
      join(session, "messages.jsonl"),
      jsonLines([
        { role: "user", content: "tie b", timestamp: 1_782_794_590_000 },
        { role: "user", content: "tie a", timestamp: 1_782_794_590_000 },
      ] satisfies AsideMessage[]),
    );

    const episodes = await collectMiscRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "tie:0",
      "tie:1",
    ]);
    expect(episodes.map((e) => e.input.content)).toEqual(["tie b", "tie a"]);
    expect(episodes[0]?.input.properties).toEqual({ kind: "message" });
  });

  /**
   * The snapshot's own `state.db` carries an unflushed write-ahead log, so the
   * rows this adapter reads only exist once the log is copied beside the main
   * file — reading the main file alone would silently lose the session title
   * and cwd of every session written since the last checkpoint.
   */
  test("reads rows still held in a sqlite write-ahead log", async () => {
    const root = await fixtureRoot();
    const user = join(root, "aside", "home", ".aside", "u", "0");
    const session = join(user, "sessions", "2026-06-30_walSession");
    await mkdir(session, { recursive: true });
    await writeFile(
      join(session, "messages.jsonl"),
      jsonLines([
        { role: "user", content: "in the log", timestamp: 1_782_794_590_000 },
      ] satisfies AsideMessage[]),
    );
    const state = new Database(join(user, "state.db"));
    state.run("pragma journal_mode = wal");
    state.run(
      "create table sessions (id text primary key, title text not null, cwd text not null)",
    );
    state.run("insert into sessions values (?, ?, ?)", [
      "walSession",
      "unflushed title",
      "/Users/ino/Develop",
    ]);
    state.close(false);

    const [episode] = await collectMiscRaw(root);

    expect(episode?.input.properties).toEqual({
      kind: "message",
      session_title: "unflushed title",
      cwd: "/Users/ino/Develop",
    });
  });

  test("skips a store whose transcript or index is missing or unreadable", async () => {
    const root = await fixtureRoot();
    const user = join(root, "aside", "home", ".aside", "u", "0");
    await mkdir(join(user, "sessions", "2026-06-30_empty"), {
      recursive: true,
    });
    await writeFile(join(user, "sessions", "loose.txt"), "not a session");
    /**
     * A file where the walk expects a user directory: it is listed beside the
     * real users, and a stray lock file must not be opened as one.
     */
    await writeFile(join(root, "aside", "home", ".aside", "u", "lock"), "");
    /** A second user the product created but never opened a session in. */
    await mkdir(join(root, "aside", "home", ".aside", "u", "1"), {
      recursive: true,
    });
    /** A conversation whose brain directory holds tasks but no transcript. */
    await mkdir(
      join(
        root,
        "gemini-antigravity",
        "home",
        ".gemini",
        "antigravity-cli",
        "brain",
        "e5d007bf-9b98-4094-8db3-ca70db2bed29",
        ".system_generated",
        "tasks",
      ),
      { recursive: true },
    );
    await mkdir(join(root, "opencode", "home"), { recursive: true });

    expect(await collectMiscRaw(root)).toEqual([]);
  });

  test("ingests one unbroken session chain per raw store", async () => {
    const root = await miscRawRoot();
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const episodes = await collectMiscRaw(root);
      const ids: string[] = [];
      for (const episode of episodes) {
        ids.push(
          (
            await engine.remember({
              ...episode.input,
              origin: {
                ...episode.input.origin,
                session: `${episode.input.origin.session}:${objectsRoot}`,
              },
            })
          ).id,
        );
      }

      const heads: string[] = [];
      for (const [index, id] of ids.entries()) {
        const inbound = await engine.store.linksOf(id, "NEXT_EPISODE");
        if (inbound.every((link) => link.to !== id)) {
          heads.push(episodes[index]?.input.origin.session ?? "");
        }
      }
      const sessions = episodes.map((e) => e.input.origin.session);

      expect(heads.sort()).toEqual([...new Set(sessions)].sort());
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
  });
});

const OMO_LONG = "w".repeat(4200);

function omoLine(record: Record<string, unknown>): string {
  return JSON.stringify(record);
}

function omoMessage(
  id: string,
  timestamp: string,
  role: string,
  content: readonly unknown[],
): string {
  return omoLine({
    type: "message",
    id,
    parentId: null,
    timestamp,
    message: { role, content, timestamp: 1 },
  });
}

/**
 * The snapshot's two homes under one parent: the agent's own `~/.omo` store
 * and a project checkout that keeps its delegated subagent transcripts under
 * `.omo/senpi-task/children`. Every transcript is written out of event-time
 * order, and the homes are named so that walk order contradicts time order.
 */
async function omoRawRoot(scope = "fixed"): Promise<string> {
  const root = await fixtureRoot();
  const workspace = join(
    root,
    "omo-senpi",
    "home",
    ".omo",
    "agent",
    "sessions",
    "--Users-ino-Develop--",
  );
  const memory = join(
    root,
    "omo-senpi",
    "home",
    ".omo",
    "memory",
    "agents",
    "develop-db021987",
    "runtime",
    "reflection",
    "runs",
    "reflection-run-1",
  );
  const child = join(
    root,
    "omo-project",
    "home",
    "Develop",
    ".omo",
    "senpi-task",
    "children",
    "st_01a041b3",
    "sessions",
    "st_01a041b3",
  );
  await mkdir(workspace, { recursive: true });
  await mkdir(memory, { recursive: true });
  await mkdir(child, { recursive: true });

  await writeFile(
    join(workspace, "2026-09-02T04-47-11-454Z_01a06071.jsonl"),
    [
      omoLine({
        type: "session",
        version: 3,
        id: `${scope}-session-main`,
        timestamp: "2026-09-02T04:47:12.718Z",
        cwd: "/Users/ino/Develop",
      }),
      omoMessage("ev-late", "2026-09-02T04:50:00.000Z", "assistant", [
        { type: "thinking", thinking: "provider scratch space", thinkingSignature: "{}" },
        { type: "text", text: OMO_LONG },
      ]),
      omoMessage("ev-early", "2026-09-02T04:48:00.000Z", "user", [
        { type: "text", text: "use ghp_0123456789012345678901234567890123456789" },
      ]),
      omoLine({
        type: "compaction",
        id: "ev-compaction",
        parentId: "ev-early",
        timestamp: "2026-09-02T04:49:00.000Z",
        summary: "## 1. User Requests (Verbatim)\nships the raw adapter",
      }),
      omoMessage("ev-two-parts", "2026-09-02T04:51:00.000Z", "assistant", [
        { type: "text", text: "first part" },
        { type: "text", text: "second part" },
      ]),
    ].join("\n"),
  );

  await writeFile(
    join(memory, "2026-08-26T06-54-38-959Z_01a03cd9.jsonl"),
    [
      omoLine({
        type: "session",
        version: 3,
        id: `${scope}-session-memory`,
        timestamp: "2026-08-26T06:54:38.959Z",
        cwd: "/Users/ino/.omo/memory/agents/develop-db021987/runtime/worktrees/1787727277154-reflection-run-1",
      }),
      omoMessage("ev-memory", "2026-09-02T04:47:30.000Z", "user", [
        { type: "text", text: "the memory runtime's own agent turn" },
      ]),
    ].join("\n"),
  );

  await writeFile(
    join(child, "2026-08-27T05-23-13-372Z_01a041ac.jsonl"),
    [
      omoLine({
        type: "session",
        version: 3,
        id: `${scope}-session-child`,
        timestamp: "2026-08-27T05:23:13.372Z",
      }),
      omoMessage("ev-child", "2026-09-02T04:47:20.000Z", "user", [
        { type: "text", text: "the delegated subagent turn" },
      ]),
    ].join("\n"),
  );
  return root;
}

/**
 * Runtime bookkeeping the agent interleaves with the conversation. The
 * snapshot carries 748 `custom` records (rule scans, todo and hook state,
 * memory bindings), 155 `custom_message` harness injections and 3,167
 * `toolResult` messages — every one of them addressed to the model rather
 * than spoken by either party. An event without an id has no record to key
 * on, and one without an event time has no place in the session order.
 */
async function omoNoiseRoot(): Promise<string> {
  const root = await fixtureRoot();
  const workspace = join(root, "omo-senpi", "home", ".omo", "agent", "sessions", "--Users-ino--");
  await mkdir(workspace, { recursive: true });
  await writeFile(
    join(workspace, "noise.jsonl"),
    [
      omoLine({ type: "session", version: 3, id: "session-noise", timestamp: "2026-09-02T04:00:00.000Z" }),
      omoLine({ type: "model_change", id: "n-model", parentId: null, timestamp: "2026-09-02T04:00:01.000Z", provider: "token-hub-codex", modelId: "gpt-5.6-sol" }),
      omoLine({ type: "thinking_level_change", id: "n-think", timestamp: "2026-09-02T04:00:02.000Z", thinkingLevel: "medium" }),
      omoLine({ type: "session_info", id: "n-name", timestamp: "2026-09-02T04:00:03.000Z", name: "클로드 비즈니스 계정 쿼터 전환" }),
      omoLine({ type: "custom", id: "n-rules", timestamp: "2026-09-02T04:00:04.000Z", customType: "pi-rules.scan", data: { cwd: "/Users/ino", reason: "startup" } }),
      omoLine({ type: "custom", id: "n-todo", timestamp: "2026-09-02T04:00:05.000Z", customType: "senpi.todo-state", data: { items: [] } }),
      omoLine({ type: "custom", id: "n-turns", timestamp: "2026-09-02T04:00:06.000Z", customType: "omo-memory:accepted-turns", data: {} }),
      omoLine({ type: "custom_message", id: "n-injection", timestamp: "2026-09-02T04:00:07.000Z", customType: "ttsr-injection", content: "<system-interrupt reason=\"rule_violation\">stop restating</system-interrupt>" }),
      omoMessage("n-toolcall", "2026-09-02T04:00:08.000Z", "assistant", [
        { type: "toolCall", id: "call_1", name: "read", arguments: { path: "README.md" } },
      ]),
      omoLine({
        type: "message",
        id: "n-toolresult",
        timestamp: "2026-09-02T04:00:09.000Z",
        message: {
          role: "toolResult",
          toolCallId: "call_1|fc_08f8",
          toolName: "read",
          content: [{ type: "text", text: "file body" }],
          timestamp: 2,
        },
      }),
      omoMessage("n-blank", "2026-09-02T04:00:10.000Z", "user", [{ type: "text", text: "   " }]),
      omoMessage("n-thinking-only", "2026-09-02T04:00:11.000Z", "assistant", [
        { type: "thinking", thinking: "scratch" },
      ]),
      omoLine({
        type: "message",
        timestamp: "2026-09-02T04:00:12.000Z",
        message: { role: "user", content: [{ type: "text", text: "no id, no record to key on" }], timestamp: 3 },
      }),
      omoLine({
        type: "message",
        id: "n-untimed",
        message: { role: "user", content: [{ type: "text", text: "no event time, no place in the order" }], timestamp: 4 },
      }),
      omoLine({
        type: "message",
        id: "n-textless",
        timestamp: "2026-09-02T04:00:13.000Z",
        message: { role: "user", content: "not a part list", timestamp: 5 },
      }),
      omoLine({
        type: "message",
        id: "n-messageless",
        timestamp: "2026-09-02T04:00:14.000Z",
        message: "not an object",
      }),
      omoLine({ type: "compaction", id: "n-empty-compaction", timestamp: "2026-09-02T04:00:15.000Z", summary: "   " }),
      omoMessage("n-kept", "2026-09-02T04:00:16.000Z", "user", [{ type: "text", text: "the only turn here" }]),
    ].join("\n"),
  );
  return root;
}

describe("collectOmoRaw", () => {
  test("orders both homes by event time and maps the origin", async () => {
    const root = await omoRawRoot();

    const episodes = await collectOmoRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "ev-child",
      "ev-memory",
      "ev-early",
      "ev-compaction",
      "ev-late",
      "ev-two-parts",
    ]);
    expect(episodes[0]?.input.origin).toEqual({
      source: "omo",
      session: "fixed-session-child",
      actor: "user",
      record: "ev-child",
    });
    expect(episodes[0]?.input.time).toEqual({
      value: "2026-09-02T04:47:20.000Z",
      precision: "second",
    });
    expect(episodes[0]?.input.properties).toEqual({
      kind: "message",
      role: "user",
    });
    expect(episodes[0]?.input.schema).toBe("anamnesis.original-message/1");
    expect(episodes[0]?.input.payload).toBeUndefined();
  });

  test("masks secrets and keys the revision on the raw text", async () => {
    const root = await omoRawRoot();

    const episodes = await collectOmoRaw(root);
    const masked = episodes[2];
    const secret = "use ghp_0123456789012345678901234567890123456789";

    expect(masked?.redactions).toBe(1);
    expect(masked?.input.content).toBe(`use ${REDACTION}`);
    expect(masked?.input.source_revision).toBe(
      createHash("sha256")
        .update(`2026-09-02T04:48:00.000Z\n${secret}`, "utf8")
        .digest("hex"),
    );
  });

  test("stores an oversized turn as a payload with an excerpt on the node", async () => {
    const root = await omoRawRoot();

    const episodes = await collectOmoRaw(root);
    const long = episodes[4];

    expect(long?.input.content).toBe("w".repeat(4000));
    expect(long?.input.payload).toEqual(new TextEncoder().encode(OMO_LONG));
    expect(long?.input.payload_media_type).toBe("text/plain");
  });

  test("keeps a limit-length turn inline", async () => {
    const root = await fixtureRoot();
    const workspace = join(root, "omo-senpi", "home", ".omo", "agent", "sessions", "--Users-ino--");
    await mkdir(workspace, { recursive: true });
    await writeFile(
      join(workspace, "limit.jsonl"),
      [
        omoLine({ type: "session", id: "session-limit", timestamp: "2026-09-02T04:00:00.000Z" }),
        omoMessage("ev-limit", "2026-09-02T04:00:01.000Z", "assistant", [
          { type: "text", text: "v".repeat(4000) },
        ]),
      ].join("\n"),
    );

    const [episode] = await collectOmoRaw(root);

    expect(episode?.input.content).toBe("v".repeat(4000));
    expect(episode?.input.payload).toBeUndefined();
    expect(episode?.input.payload_media_type).toBeUndefined();
  });

  /**
   * A compaction replaces the turns it summarizes, so its summary is the only
   * surviving record of them and it has no author of its own.
   */
  test("carries a compaction summary under the kind that names it", async () => {
    const root = await omoRawRoot();

    const episodes = await collectOmoRaw(root);
    const compaction = episodes[3];

    expect(compaction?.input.content).toBe(
      "## 1. User Requests (Verbatim)\nships the raw adapter",
    );
    expect(compaction?.input.origin.actor).toBe("unknown");
    expect(compaction?.input.properties).toEqual({ kind: "compaction" });
  });

  test("joins the text parts of one turn and drops provider scratch space", async () => {
    const root = await omoRawRoot();

    const episodes = await collectOmoRaw(root);

    expect(episodes[5]?.input.content).toBe("first part\nsecond part");
    expect(episodes[4]?.input.content).not.toContain("provider scratch space");
  });

  test("skips runtime bookkeeping, tool plumbing and incomplete events", async () => {
    const root = await omoNoiseRoot();

    const episodes = await collectOmoRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["n-kept"]);
    expect(episodes[0]?.input.content).toBe("the only turn here");
  });

  /**
   * The snapshot was written on macOS, so an AppleDouble sidecar mirrors every
   * one of its files. Reading one would parse its binary header as a
   * transcript. Alongside them sit the `.log` captures of child process
   * output, the `.txt` notes and the `posthog-activity.json` telemetry cache,
   * none of which carries conversation.
   */
  test("ignores AppleDouble sidecars and non-transcript files", async () => {
    const root = await omoNoiseRoot();
    const workspace = join(root, "omo-senpi", "home", ".omo", "agent", "sessions", "--Users-ino--");
    await writeFile(join(workspace, "._noise.jsonl"), "\u0000\u0005\u0016\u0007not json");
    await writeFile(join(workspace, "child-stdout.log"), "bun install\n");
    await writeFile(
      join(workspace, "session.log"),
      `${JSON.stringify({ ts: "2026-08-26T06:26:09.768Z", level: "debug", event: "queue_enqueue" })}\n`,
    );
    await writeFile(
      join(workspace, "posthog-activity.json"),
      JSON.stringify({ lastActivity: 1787727277154 }),
    );
    await writeFile(join(workspace, "notes.txt"), "scratch");

    const episodes = await collectOmoRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["n-kept"]);
  });

  /**
   * The memory extension re-encodes sessions it has observed into a flat
   * `kind`/`text`/`captured_at` stream keyed by `source_message_id`. The
   * native transcript is authoritative — it alone carries the session header,
   * the parent chain and the part structure — so the derived copy is skipped
   * and one turn cannot enter the graph under two records.
   */
  test("prefers the native transcript over the memory runtime's re-encoding", async () => {
    const root = await omoRawRoot();
    const derived = join(
      root,
      "omo-senpi",
      "home",
      ".omo",
      "memory",
      "agents",
      "develop-db021987",
      "runtime",
      "transcripts",
      "fixed-session-main",
    );
    await mkdir(derived, { recursive: true });
    await writeFile(
      join(derived, "transcript.jsonl"),
      [
        JSON.stringify({
          kind: "user",
          text: "use ghp_0123456789012345678901234567890123456789",
          captured_at: "2026-09-02T04:48:00.000Z",
          source_line_id: "ev-early:user",
          source_message_id: "ev-early",
        }),
        JSON.stringify({
          kind: "tool_call",
          name: "read",
          argsText: "{}",
          resultOk: true,
          resultText: "file body",
          captured_at: "2026-09-02T04:48:30.000Z",
          source_line_id: "ev-tool:tool",
          source_message_id: "ev-tool",
        }),
      ].join("\n"),
    );

    const episodes = await collectOmoRaw(root);
    const records = episodes.map((e) => e.input.origin.record);

    expect(records.filter((record) => record === "ev-early")).toEqual([
      "ev-early",
    ]);
    expect(records).not.toContain("ev-tool");
  });

  /**
   * The snapshot's 27 databases are `codegraph.db` code indexes totalling
   * 20GB, whose tables are `nodes`, `edges`, `files` and their FTS shadows.
   * The probe reads the table list and returns without touching a symbol row.
   */
  test("skips a database that holds no conversation table", async () => {
    const root = await fixtureRoot();
    const projects = join(root, "omo-senpi", "home", ".omo", "codegraph", "projects", "repo-dff5884f");
    await mkdir(projects, { recursive: true });
    const codegraph = new Database(join(projects, "codegraph.db"), { create: true });
    codegraph.run(
      "CREATE TABLE nodes (id TEXT PRIMARY KEY, kind TEXT, name TEXT, file_path TEXT)",
    );
    codegraph.run("CREATE TABLE edges (id INTEGER PRIMARY KEY, source TEXT, target TEXT)");
    codegraph.run("CREATE TABLE files (path TEXT PRIMARY KEY)");
    codegraph.run(
      "INSERT INTO nodes VALUES ('n1', 'constant', 'KO_TITLE', 'apps/admin/e2e/i18n.spec.ts')",
    );
    codegraph.close();

    expect(await collectOmoRaw(root)).toEqual([]);
  });

  /**
   * A store that does keep sessions in sqlite is read through the same
   * originals contract as a transcript, with the part list decoded out of the
   * content column and the runtime roles left behind.
   */
  test("reads a session store that keeps its messages in sqlite", async () => {
    const root = await fixtureRoot();
    const store = join(root, "omo-senpi", "home", ".omo", "agent");
    await mkdir(store, { recursive: true });
    const db = new Database(join(store, "sessions.db"), { create: true });
    db.run("PRAGMA journal_mode = WAL");
    db.run(
      "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT)",
    );
    const insert = db.prepare(
      "INSERT INTO messages VALUES (?, ?, ?, ?, ?)",
    );
    insert.run("db-late", "db-session", "assistant", "a plain text column", "2026-09-02T05:01:00.000Z");
    insert.run(
      "db-early",
      "db-session",
      "user",
      JSON.stringify([
        { type: "thinking", thinking: "scratch" },
        { type: "text", text: "encoded as a part list" },
      ]),
      "2026-09-02T05:00:00.000Z",
    );
    insert.run("db-object", "db-session", "user", JSON.stringify({ text: "encoded as one part" }), "2026-09-02T05:02:00.000Z");
    /**
     * A row killed mid-write opens like a part list and never closes. It is
     * still the only record of that turn, so it is kept as the text it is
     * rather than discarded for failing to parse.
     */
    insert.run("db-truncated", "db-session", "assistant", '[{"type":"text","text":"cut off mid', "2026-09-02T05:02:30.000Z");
    insert.run("db-tool", "db-session", "toolResult", "command output", "2026-09-02T05:03:00.000Z");
    insert.run("db-blank", "db-session", "user", "   ", "2026-09-02T05:04:00.000Z");
    insert.run("db-untimed", "db-session", "user", "no event time", "not a date");
    insert.run("db-partial", "db-session", "user", null, "2026-09-02T05:05:00.000Z");
    db.close();

    const episodes = await collectOmoRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual([
      "db-early",
      "db-late",
      "db-object",
      "db-truncated",
    ]);
    expect(episodes[3]?.input.content).toBe('[{"type":"text","text":"cut off mid');
    expect(episodes[0]?.input.content).toBe("encoded as a part list");
    expect(episodes[0]?.input.origin).toEqual({
      source: "omo",
      session: "db-session",
      actor: "user",
      record: "db-early",
    });
    expect(episodes[0]?.input.source_revision).toBe(
      createHash("sha256")
        .update("2026-09-02T05:00:00.000Z\nencoded as a part list", "utf8")
        .digest("hex"),
    );
    expect(episodes[2]?.input.content).toBe("encoded as one part");
  });

  /** A transcript whose header never arrives cannot be attributed to a session. */
  test("leaves out events that arrive before their session header", async () => {
    const root = await fixtureRoot();
    const workspace = join(root, "omo-senpi", "home", ".omo", "agent", "sessions", "--Users-ino--");
    await mkdir(workspace, { recursive: true });
    await writeFile(
      join(workspace, "headerless.jsonl"),
      [
        omoMessage("ev-orphan", "2026-09-02T04:00:00.000Z", "user", [
          { type: "text", text: "orphan" },
        ]),
        "",
        JSON.stringify(["not an object"]),
        '{"type":"message","id":"torn",',
      ].join("\n"),
    );

    expect(await collectOmoRaw(root)).toEqual([]);
  });

  test("tie-breaks equal event times on the record so the order is total", async () => {
    const root = await fixtureRoot();
    const workspace = join(root, "omo-senpi", "home", ".omo", "agent", "sessions", "--Users-ino--");
    await mkdir(workspace, { recursive: true });
    const same = (id: string): string =>
      omoMessage(id, "2026-09-02T04:00:00.000Z", "user", [
        { type: "text", text: `tie ${id}` },
      ]);
    await writeFile(
      join(workspace, "tie.jsonl"),
      [
        omoLine({ type: "session", id: "session-tie", timestamp: "2026-09-02T03:00:00.000Z" }),
        same("b"),
        same("c"),
        same("a"),
      ].join("\n"),
    );

    const episodes = await collectOmoRaw(root);

    expect(episodes.map((e) => e.input.origin.record)).toEqual(["a", "b", "c"]);
  });

  /** A raw store that was never populated is an empty backfill, not an error. */
  test("returns nothing when the snapshot root is absent", async () => {
    expect(await collectOmoRaw(join(await fixtureRoot(), "absent"))).toEqual([]);
  });

  test("ingests one unbroken session chain per raw transcript", async () => {
    const root = await omoRawRoot(`chain${Date.now()}`);
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const episodes = await collectOmoRaw(root);
      const ids: string[] = [];
      for (const episode of episodes) {
        ids.push((await engine.remember(episode.input)).id);
      }

      const heads: string[] = [];
      for (const [index, id] of ids.entries()) {
        const inbound = await engine.store.linksOf(id, "NEXT_EPISODE");
        if (inbound.every((link) => link.to !== id)) {
          heads.push(episodes[index]?.input.origin.session ?? "");
        }
      }
      const sessions = episodes.map((e) => e.input.origin.session);

      expect(heads.sort()).toEqual([...new Set(sessions)].sort());
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
  });
});

function ingested(session: string, record: string, value: string): Collected {
  return {
    input: {
      content: `${session} ${record}`,
      time: { value, precision: "second" },
      origin: { source: "runner-test", session, actor: "ino", record },
    },
    redactions: 0,
  };
}

function emptyReceipt(): Receipt {
  return {
    source: "runner-test",
    episodes: 0,
    created: 0,
    duplicates: 0,
    invalidated: 0,
    diverged: 0,
    redactions: 0,
  };
}

describe("groupBySession", () => {
  test("keeps within-session order and first-seen group order", () => {
    const collected = [
      ingested("a", "a1", "2026-01-01T00:00:01Z"),
      ingested("b", "b1", "2026-01-01T00:00:02Z"),
      ingested("a", "a2", "2026-01-01T00:00:03Z"),
      ingested("c", "c1", "2026-01-01T00:00:04Z"),
      ingested("b", "b2", "2026-01-01T00:00:05Z"),
      ingested("a", "a3", "2026-01-01T00:00:06Z"),
    ];

    const groups = groupBySession(collected);

    expect(
      groups.map((group) => group.map((item) => item.input.origin.record)),
    ).toEqual([["a1", "a2", "a3"], ["b1", "b2"], ["c1"]]);
  });

  test("separates equal session names from different sources", () => {
    const shared = ingested("dup", "x1", "2026-01-01T00:00:01Z");
    const other: Collected = {
      ...shared,
      input: {
        ...shared.input,
        origin: { ...shared.input.origin, source: "other-source" },
      },
    };

    expect(groupBySession([shared, other])).toEqual([[shared], [other]]);
  });
});

describe("concurrency", () => {
  test("defaults to 8 when the variable is unset or empty", () => {
    expect(concurrency(undefined)).toBe(8);
    expect(concurrency("")).toBe(8);
  });

  test("reads a positive integer from the environment", () => {
    expect(concurrency("3")).toBe(3);
    expect(concurrency("1")).toBe(1);
  });

  test("rejects values that are not positive integers", () => {
    expect(() => concurrency("0")).toThrow(
      "ANAMNESIS_BACKFILL_CONCURRENCY must be a positive integer, got 0",
    );
    expect(() => concurrency("-2")).toThrow("positive integer");
    expect(() => concurrency("2.5")).toThrow("positive integer");
    expect(() => concurrency("eight")).toThrow("positive integer");
  });
});

describe("ingestGroups", () => {
  test("ingests independent sessions in parallel, in order within each", async () => {
    const stamp = `${Date.now()}-${process.pid}`;
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const sessions = [`par-a-${stamp}`, `par-b-${stamp}`, `par-c-${stamp}`];
      const collected = [0, 1, 2, 3, 4].flatMap((step) =>
        sessions.map((session) =>
          ingested(
            session,
            `r${step}`,
            new Date(Date.UTC(2026, 0, 1, 0, 0, step)).toISOString(),
          ),
        ),
      );
      const receipt = emptyReceipt();
      receipt.episodes = collected.length;

      const groups = groupBySession(collected);
      await ingestGroups(groups, engine, undefined, receipt, 3);

      expect(groups.length).toBe(3);
      expect(receipt.created).toBe(collected.length);
      expect(receipt.duplicates).toBe(0);
      for (const session of sessions) {
        const order = await sessionChain(engine, session);
        expect(order).toEqual(["r0", "r1", "r2", "r3", "r4"]);
      }
      expect(await chainHeads(engine, sessions)).toEqual(sessions.length);
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
  });

  test("aggregates duplicate and redaction counters across workers", async () => {
    const stamp = `${Date.now()}-${process.pid}`;
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    const engine = new Engine({ ...TEST_DB, objectsRoot });
    await engine.init();
    try {
      const collected = [
        { ...ingested(`dup-a-${stamp}`, "r0", "2026-01-01T00:00:00Z"), redactions: 2 },
        ingested(`dup-b-${stamp}`, "r0", "2026-01-01T00:00:00Z"),
      ];
      const receipt = emptyReceipt();

      const groups = groupBySession([...collected, ...collected]);
      await ingestGroups(groups, engine, undefined, receipt, 8);

      expect(receipt.created).toBe(2);
      expect(receipt.duplicates).toBe(2);
      expect(receipt.redactions).toBe(4);
    } finally {
      await engine.close();
      await rm(objectsRoot, { recursive: true });
    }
  });
});

interface Ingested {
  id: string;
  record: string;
}

async function episodesOf(session: string): Promise<Ingested[]> {
  const driver = neo4j.driver(
    TEST_DB.uri,
    neo4j.auth.basic(TEST_DB.user, TEST_DB.password),
  );
  try {
    const { records } = await driver.executeQuery(
      `MATCH (e:Element:Episode { origin_session: $session })
       RETURN e.id AS id, e.origin_record AS record
       ORDER BY e.time_utc, e.id`,
      { session },
    );
    return records.map((row) => ({
      id: String(row.get("id")),
      record: String(row.get("record")),
    }));
  } finally {
    await driver.close();
  }
}

/** Walks NEXT_EPISODE from the session head so link order, not time, is read. */
async function sessionChain(
  engine: Engine,
  session: string,
): Promise<string[]> {
  const episodes = await episodesOf(session);
  const byId = new Map(episodes.map((e) => [e.id, e.record]));
  const successors = new Map<string, string>();
  const inbound = new Set<string>();
  for (const episode of episodes) {
    for (const link of await engine.store.linksOf(episode.id, "NEXT_EPISODE")) {
      if (link.from === episode.id) successors.set(link.from, link.to);
      if (link.to === episode.id) inbound.add(link.to);
    }
  }
  const heads = episodes.filter((e) => !inbound.has(e.id));
  expect(heads.length).toBe(1);
  const chain: string[] = [];
  for (let id: string | undefined = heads[0]?.id; id !== undefined; ) {
    chain.push(byId.get(id) ?? id);
    id = successors.get(id);
  }
  return chain;
}

async function chainHeads(
  engine: Engine,
  sessions: readonly string[],
): Promise<number> {
  let heads = 0;
  for (const session of sessions) {
    for (const episode of await episodesOf(session)) {
      const links = await engine.store.linksOf(episode.id, "NEXT_EPISODE");
      if (links.every((link) => link.to !== episode.id)) heads += 1;
    }
  }
  return heads;
}

describe("main", () => {
  test("rejects an unknown source or a missing dataset root", async () => {
    const argv = process.argv;
    try {
      process.argv = ["bun", "run.ts", "nosuch", "/tmp"];
      await expect(main()).rejects.toThrow("usage: bun backfill/run.ts");
      process.argv = ["bun", "run.ts", "slack"];
      await expect(main()).rejects.toThrow("usage: bun backfill/run.ts");
      process.argv = ["bun", "run.ts"];
      await expect(main()).rejects.toThrow("usage: bun backfill/run.ts");
    } finally {
      process.argv = argv;
    }
  });

  test("writes one receipt for a journaled parallel run", async () => {
    const stamp = `${Date.now()}-${process.pid}`;
    const root = await fixtureRoot();
    const journalDirectory = await fixtureRoot();
    const objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
    await mkdir(join(root, "channels"), { recursive: true });
    await writeFile(join(root, "index.jsonl"), "");
    for (const channel of [`M1-${stamp}`, `M2-${stamp}`]) {
      await writeFile(
        join(root, "channels", `${channel}.jsonl`),
        [
          JSON.stringify({ ts: "1.0", text: "first ghp_0123456789012345678901234567890123456789", user: "U1" }),
          JSON.stringify({ ts: "2.0", text: "second", user: "U1" }),
        ].join("\n"),
      );
    }
    const argv = process.argv;
    const env = { ...process.env };
    const written: string[] = [];
    const write = process.stdout.write.bind(process.stdout);
    process.stdout.write = ((chunk: string) => {
      written.push(String(chunk));
      return true;
    }) as typeof process.stdout.write;
    try {
      process.argv = ["bun", "run.ts", "slack", root, journalDirectory];
      process.env["ANAMNESIS_NEO4J_URI"] = TEST_DB.uri;
      process.env["ANAMNESIS_NEO4J_USER"] = TEST_DB.user;
      process.env["ANAMNESIS_NEO4J_PASSWORD"] = TEST_DB.password;
      process.env["ANAMNESIS_OBJECTS_ROOT"] = objectsRoot;
      process.env["ANAMNESIS_BACKFILL_CONCURRENCY"] = "2";

      await main();

      expect(written.length).toBe(1);
      expect(JSON.parse(written[0] ?? "{}")).toEqual({
        source: "slack",
        episodes: 4,
        created: 4,
        duplicates: 0,
        invalidated: 0,
        diverged: 0,
        redactions: 2,
      });
      expect((await readdir(journalDirectory)).length).toBe(1);
    } finally {
      process.stdout.write = write;
      process.argv = argv;
      process.env = env;
      await rm(objectsRoot, { recursive: true });
      await rm(root, { recursive: true });
      await rm(journalDirectory, { recursive: true });
    }
  });
});
