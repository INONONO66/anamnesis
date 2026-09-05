import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** Beyond this the transcript turn lives in the object store, not the node. */
const CONTENT_LIMIT = 4000;

/**
 * The normalized export this adapter has to agree with kept exactly two record
 * kinds out of the rollout stream: the conversational turns and the compaction
 * marker that explains a gap between them. Tool calls, tool output, reasoning
 * and token accounting were all rejected there, so including any of them here
 * would ingest records the 602 already-ingested sessions do not contain and
 * turn a re-run over the overlap into new writes instead of a no-op.
 */
const COMPACTION_TEXT = "context_compacted";

interface CodexRecord {
  timestamp: string;
  type: string;
  payload: Record<string, unknown>;
}

/** A rollout record reduced to the fields the originals contract consumes. */
interface CodexTurn {
  index: number;
  occurredAt: number;
  text: string;
  canonicalKind: string;
  kind: string;
  eventId: string;
  role?: string;
}

export interface CodexRawEpisode {
  input: RememberInput;
  redactions: number;
}

function optionalText(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

/**
 * Rollout files are appended live, so a session killed mid-write leaves a
 * truncated final line. Dropping it keeps the other 12,847 files ingestible.
 */
function parseRecord(line: string): CodexRecord | undefined {
  let raw: unknown;
  try {
    raw = JSON.parse(line);
  } catch {
    return undefined;
  }
  const record = asRecord(raw);
  if (record === undefined) return undefined;
  const timestamp = optionalText(record["timestamp"]);
  const type = optionalText(record["type"]);
  const payload = asRecord(record["payload"]);
  if (timestamp === undefined || type === undefined || payload === undefined) {
    return undefined;
  }
  return { timestamp, type, payload };
}

/**
 * `response_item` messages carry their text in a content array, while the
 * `event_msg` mirror of the same turn carries it in a single `message` field.
 *
 * The export joined a multi-part content array into one turn on a newline,
 * verified byte-for-byte against a 2-part record (4669 + 196 parts against a
 * 4866-character canonical text), so the parts are joined the same way here.
 * The `:content:<n>` suffix on its native ids counts something else: it is
 * always `0` on the records this adapter can key natively.
 */
function textOf(payload: Record<string, unknown>): string {
  const message = payload["message"];
  if (typeof message === "string") return message;
  const content = payload["content"];
  if (!Array.isArray(content)) return "";
  const parts: string[] = [];
  for (const item of content) {
    const part = asRecord(item);
    if (part === undefined) continue;
    const text = part["text"];
    if (typeof text === "string") parts.push(text);
  }
  return parts.join("\n");
}

/**
 * The export identified a turn by its provider message id when the rollout
 * carried one, and fell back to a digest it computed from exporter-internal
 * state otherwise. Only the first form is reconstructible from the raw files,
 * so it is reproduced exactly and the rest fall back to the session-scoped
 * record position, which is stable across re-runs of this adapter.
 */
function eventIdOf(
  payload: Record<string, unknown>,
  session: string,
  index: number,
): string {
  const id = optionalText(payload["id"]);
  return id === undefined ? `${session}:${index}` : `${id}:content:0`;
}

/**
 * `developer` messages are the harness prompt rather than a turn of the
 * conversation, and the export dropped them along with every non-message
 * record kind.
 */
function toTurn(
  record: CodexRecord,
  session: string,
  index: number,
): CodexTurn | undefined {
  const occurredAt = Date.parse(record.timestamp);
  if (!Number.isFinite(occurredAt)) return undefined;
  const payloadType = optionalText(record.payload["type"]);
  const role = optionalText(record.payload["role"]);

  if (payloadType === "context_compacted") {
    return {
      index,
      occurredAt,
      text: COMPACTION_TEXT,
      canonicalKind: "compaction",
      kind: "compaction",
      eventId: eventIdOf(record.payload, session, index),
    };
  }

  const isMessage =
    (record.type === "response_item" && payloadType === "message") ||
    (record.type === "event_msg" &&
      (payloadType === "user_message" || payloadType === "agent_message"));
  if (!isMessage) return undefined;
  if (role === "developer" || role === "system") return undefined;

  const text = textOf(record.payload);
  if (text.trim() === "") return undefined;

  const resolved =
    role ?? (payloadType === "user_message" ? "user" : "assistant");
  return {
    index,
    occurredAt,
    text,
    canonicalKind: "agent_message",
    kind: "message",
    eventId: eventIdOf(record.payload, session, index),
    role: resolved,
  };
}

/** Session-level context the export attached to every turn it emitted. */
interface SessionContext {
  id: string;
  cwd?: string;
  model?: string;
}

function toEpisode(
  turn: CodexTurn,
  context: SessionContext,
): CodexRawEpisode {
  const occurredAt = new Date(turn.occurredAt).toISOString();
  /**
   * Byte-identical to the agent-log adapter's revision so a turn already
   * ingested from the normalized export resolves to the same stored revision
   * and the overlapping 602 sessions re-ingest as a record-level no-op.
   */
  const revision = createHash("sha256")
    .update(`${occurredAt}\n${turn.text}`, "utf8")
    .digest("hex");
  const { text, redactions } = maskSecrets(turn.text);
  const properties: Record<string, string> = {
    kind: turn.kind,
    canonical_kind: turn.canonicalKind,
  };
  if (context.cwd !== undefined) properties["cwd"] = context.cwd;
  if (context.model !== undefined) properties["model"] = context.model;
  const oversized = text.length > CONTENT_LIMIT;
  return {
    redactions,
    input: {
      schema: "anamnesis.original-message/1",
      content: oversized ? text.slice(0, CONTENT_LIMIT) : text,
      origin: {
        source: "codex",
        session: context.id,
        actor: turn.role ?? "unknown",
        record: turn.eventId,
      },
      source_revision: revision,
      time: { value: occurredAt, precision: "second" },
      ...(oversized
        ? {
            payload: new TextEncoder().encode(text),
            payload_media_type: "text/plain",
          }
        : {}),
      properties,
    },
  };
}

/** `rollout-<iso>-<uuid>.jsonl`: the session id is the trailing UUID. */
function sessionIdOf(fileName: string): string {
  const stem = fileName.slice(0, -".jsonl".length);
  return stem.split("-").slice(-5).join("-");
}

/**
 * A snapshot of the live session tree, taken while its rollout files were
 * still being appended to and kept beside it under a suffixed name. Every
 * file in one is a stale copy of a live session: the same session id, the
 * same turns, and the same `${session}:${index}` fallback record ids, so the
 * same `revision_key` — but a rewritten `turn_context`, and therefore a
 * different `model` on turns the live file also emits. A `revision_key`
 * carries exactly one element body and the digest guarding it covers
 * `properties` (docs/01 §1), so walking both copies offers two bodies for one
 * revision and the store rejects the second as a `revision_conflict`
 * (docs/02 §3). The live `sessions` tree is the session; a snapshot beside it
 * is not a second one.
 */
function isSessionSnapshot(name: string): boolean {
  return name.startsWith("sessions-");
}

/**
 * 12,848 rollout files sit under a `<year>/<month>/<day>` tree next to their
 * AppleDouble sidecars, which are byte-for-byte not JSON and would otherwise
 * be parsed as sessions. An unreadable root is left to throw: silently
 * collecting nothing would report a successful backfill of zero sessions.
 */
async function rolloutFiles(root: string): Promise<string[]> {
  const found: string[] = [];
  const walk = async (directory: string): Promise<void> => {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      if (entry.name.startsWith("._")) continue;
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (isSessionSnapshot(entry.name)) continue;
        await walk(path);
      } else if (entry.isFile() && entry.name.endsWith(".jsonl")) {
        found.push(path);
      }
    }
  };
  await walk(root);
  return found.sort((a, b) => a.localeCompare(b));
}

/**
 * One 9.8GB corpus does not fit in memory, so each file is streamed a line at
 * a time and only the turns that survive the export's inclusion set are kept.
 */
async function collectFile(path: string): Promise<CodexRawEpisode[]> {
  const context: SessionContext = { id: sessionIdOf(path.split("/").pop() ?? "") };
  const turns: CodexTurn[] = [];
  const stream = createReadStream(path, { encoding: "utf8" });
  const lines = createInterface({ input: stream, crlfDelay: Infinity });
  let index = -1;
  try {
    for await (const line of lines) {
      if (line.trim() === "") continue;
      index += 1;
      const record = parseRecord(line);
      if (record === undefined) continue;
      if (record.type === "session_meta") {
        const id = optionalText(record.payload["id"]);
        if (id !== undefined) context.id = id;
        const cwd = optionalText(record.payload["cwd"]);
        if (cwd !== undefined) context.cwd = cwd;
        continue;
      }
      /** The model is only ever named on the turn context that precedes a turn. */
      if (record.type === "turn_context") {
        const model = optionalText(record.payload["model"]);
        if (model !== undefined) context.model = model;
        continue;
      }
      const turn = toTurn(record, context.id, index);
      if (turn !== undefined) turns.push(turn);
    }
  } finally {
    lines.close();
    stream.destroy();
  }
  return turns.map((turn) => toEpisode(turn, context));
}

/**
 * Episodes are ordered by event time across every session, not by file: the
 * store links each arriving Episode to the latest earlier one in its session,
 * so a backdated arrival would start a second chain head and fragment the
 * session spine it belongs to. Files are streamed one at a time and only the
 * retained episodes are held, which is a small fraction of the 9.8GB corpus.
 */
export async function collectCodexRaw(
  root: string,
): Promise<CodexRawEpisode[]> {
  const episodes: CodexRawEpisode[] = [];
  for (const path of await rolloutFiles(root)) {
    episodes.push(...(await collectFile(path)));
  }
  return episodes.sort((a, b) => {
    const at = a.input.time?.value ?? "";
    const bt = b.input.time?.value ?? "";
    return at === bt
      ? a.input.origin.record.localeCompare(b.input.origin.record)
      : at.localeCompare(bt);
  });
}
