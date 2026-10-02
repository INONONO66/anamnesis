import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

interface CodexRaw {
  timestamp: string;
  type: string;
  payload: Record<string, unknown>;
}

export function codexLines(records: CodexRaw[]): string {
  return `${records.map((record) => JSON.stringify(record)).join("\n")}\n`;
}

export function userMessage(text: string): Record<string, unknown> {
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
export const REAL_TEXT =
  "I\u2019m pulling the issue text and the current docs/code so I can verify each requested correction against the repository before touching the markdown.";
export const REAL_SESSION = "019cd33d-e40f-7da3-a19f-15360a9c3c36";
const REAL_TIMESTAMP = "2026-03-09T15:36:22.283Z";
export const REAL_OCCURRED_AT = 1773070582283;

export const CODEX_LONG = "z".repeat(4200);

/**
 * Two sessions written out of event-time order, spanning every record kind the
 * export retained and a representative sample of the kinds it rejected, plus
 * an AppleDouble sidecar and a truncated tail line.
 */
export async function codexRawRoot(root: string, scope = "fixed"): Promise<string> {
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
