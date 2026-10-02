import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { collectCodexRaw } from "./codexraw.ts";
import { REDACTION } from "./secrets.ts";
import { CODEX_LONG, REAL_OCCURRED_AT, REAL_SESSION, REAL_TEXT, codexLines, codexRawRoot, userMessage } from "./codexraw.fixture.ts";

let tmpRoot: string;
let next = 0;
const directory = async () => { const dir = join(tmpRoot, `codex${next++}`); await mkdir(dir); return dir; };
beforeAll(async () => { tmpRoot = await mkdtemp(join(tmpdir(), "codex-fs-")); });
afterAll(async () => { await rm(tmpRoot, { recursive: true, force: true }); });

describe("collectCodexRaw", () => {
  test("mirrors the normalized export's origin and revision for a real turn", async () => {
    const root = await codexRawRoot(await directory());

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
    const root = await codexRawRoot(await directory());

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
    const root = await codexRawRoot(await directory());

    const episodes = await collectCodexRaw(root);
    const native = episodes.find((e) => e.input.content === `use ${REDACTION}`);

    expect(native?.input.origin.record).toBe("msg_native:content:0");
    expect(native?.redactions).toBe(1);
    expect(native?.input.origin.actor).toBe("user");
  });

  test("stores an oversized turn as a payload with an excerpt on the node", async () => {
    const root = await codexRawRoot(await directory());

    const episodes = await collectCodexRaw(root);
    const long = episodes[2];

    expect(long?.input.content).toBe("z".repeat(4000));
    expect(long?.input.payload).toEqual(new TextEncoder().encode(CODEX_LONG));
    expect(long?.input.payload_media_type).toBe("text/plain");
    expect(episodes[0]?.input.payload).toBeUndefined();
  });

  test("keys the revision on raw text so masking changes open none", async () => {
    const root = await codexRawRoot(await directory());
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
    const root = await directory();
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
    const root = await directory();
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
    const root = await directory();
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
});
