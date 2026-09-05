import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { Database } from "bun:sqlite";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** Beyond this the transcript turn lives in the object store, not the node. */
const CONTENT_LIMIT = 4000;

/**
 * The snapshot holds two homes side by side — the agent's own `~/.omo` store
 * and the per-project `.omo` directories checked out under `~/Develop` — so
 * the walk starts at the parent of both rather than at one sessions root.
 * Native transcripts are identified by the session header they open with, not
 * by their path: the same format appears under the agent's own sessions
 * directory, under the runtime directory where the memory extension runs its
 * own agents, and under the children directory where delegated subagents
 * write theirs.
 */
const SESSION_HEADER = "session";

/**
 * A transcript event as the agent wrote it. The outer `id` and `timestamp` are
 * the session-level identity every record is keyed on; the inner
 * `message.timestamp` records when the provider call was issued and drifts
 * from it by the request duration, so it cannot key the same records.
 */
interface RawEvent {
  type: string;
  id: string;
  timestamp: string;
  role?: string;
  text: string;
}

export interface OmoRawEpisode {
  input: RememberInput;
  redactions: number;
}

function optionalText(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Only `text` parts carry the turn. `thinking` is provider scratch space whose
 * `thinkingSignature` is an encrypted provider blob rather than prose, and
 * `toolCall` / `toolResult` parts hold arguments and command output rather
 * than conversation.
 */
function messageText(content: unknown): string | undefined {
  if (!Array.isArray(content)) return undefined;
  const parts: string[] = [];
  for (const part of content) {
    if (!isRecord(part) || part["type"] !== "text") continue;
    const text = optionalText(part["text"]);
    if (text !== undefined && text.trim() !== "") parts.push(text);
  }
  return parts.length === 0 ? undefined : parts.join("\n");
}

/**
 * `user` and `assistant` are the conversation roles. `toolResult` outnumbers
 * both in this snapshot (3,167 records against 2,697) and carries command
 * output addressed to the model rather than a turn of the conversation.
 */
const CONVERSATION_ROLES = new Set(["user", "assistant"]);

/**
 * Returns the event when it carries conversation content, `undefined` when the
 * line is runtime bookkeeping. The snapshot interleaves 748 `custom` records
 * (rule scans, todo state, hook state, memory bindings), 155 `custom_message`
 * injections the harness addresses to the model, and model, thinking-level and
 * session-name changes — none of which is a turn either party spoke.
 */
function parseEvent(line: string): RawEvent | undefined {
  const raw: unknown = JSON.parse(line);
  if (!isRecord(raw)) return undefined;
  const type = optionalText(raw["type"]);
  const id = optionalText(raw["id"]);
  const timestamp = optionalText(raw["timestamp"]);
  if (type === undefined || id === undefined || timestamp === undefined) {
    return undefined;
  }
  /**
   * A compaction replaces the history it summarizes, so the summary is the
   * only surviving record of those turns. It has no author of its own.
   */
  if (type === "compaction") {
    const summary = optionalText(raw["summary"]);
    return summary === undefined || summary.trim() === ""
      ? undefined
      : { type, id, timestamp, text: summary };
  }
  if (type !== "message") return undefined;
  const message = raw["message"];
  if (!isRecord(message)) return undefined;
  const role = optionalText(message["role"]);
  if (role === undefined || !CONVERSATION_ROLES.has(role)) return undefined;
  const text = messageText(message["content"]);
  return text === undefined ? undefined : { type, id, timestamp, role, text };
}

/** Every field the originals contract requires, resolved against its session. */
interface SessionEvent extends RawEvent {
  session: string;
}

function toEpisode(event: SessionEvent): OmoRawEpisode {
  const occurredAt = new Date(event.timestamp).toISOString();
  /**
   * Keyed on the raw event time and raw text, the convention every adapter in
   * this package shares: a change to the masking rules must not rewrite the
   * revision key and open a false revision of every event a new rule touches.
   */
  const revision = createHash("sha256")
    .update(`${occurredAt}\n${event.text}`, "utf8")
    .digest("hex");
  const { text, redactions } = maskSecrets(event.text);
  const properties: Record<string, string> = { kind: event.type };
  if (event.role !== undefined) properties["role"] = event.role;
  const oversized = text.length > CONTENT_LIMIT;
  return {
    redactions,
    input: {
      schema: "anamnesis.original-message/1",
      content: oversized ? text.slice(0, CONTENT_LIMIT) : text,
      origin: {
        source: "omo",
        session: event.session,
        actor: event.role ?? "unknown",
        record: event.id,
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

/**
 * The session header opens every transcript and names the id the records are
 * partitioned on. A file whose header never arrives cannot be attributed to a
 * session, so its events are left out rather than given a synthetic partition.
 *
 * Lines are parsed one at a time rather than read whole: the memory runtime
 * writes transcripts past a megabyte, and a session killed mid-write leaves a
 * truncated final line that must not lose the session it precedes.
 */
async function readSession(path: string): Promise<SessionEvent[]> {
  const events: SessionEvent[] = [];
  let session: string | undefined;
  const stream = createReadStream(path, { encoding: "utf8" });
  const lines = createInterface({ input: stream, crlfDelay: Infinity });
  try {
    for await (const line of lines) {
      if (line.trim() === "") continue;
      let raw: unknown;
      try {
        raw = JSON.parse(line);
      } catch {
        continue;
      }
      if (!isRecord(raw)) continue;
      if (raw["type"] === SESSION_HEADER) {
        session = optionalText(raw["id"]);
        continue;
      }
      if (session === undefined) continue;
      const event = parseEvent(line);
      if (event !== undefined) events.push({ ...event, session });
    }
  } finally {
    lines.close();
    stream.destroy();
  }
  return events;
}

/**
 * The memory extension re-encodes sessions it has already observed into
 * `runtime/transcripts/<session>/transcript.jsonl` under a flat
 * `kind`/`text`/`captured_at` shape keyed by `source_message_id`. In this
 * snapshot 453 of its 848 records name a message id that is present natively,
 * and the re-encoding drops the session header, the parent chain and the part
 * structure while flattening reasoning and tool calls into the same stream.
 * The native transcript is therefore the authoritative form and the derived
 * copy is skipped, so one turn cannot enter the graph under two records.
 */
const DERIVED_TRANSCRIPT = "transcript.jsonl";

/**
 * Conversation lives in these columns when an OMO store keeps sessions in
 * sqlite. The snapshot's own databases are `codegraph.db` code indexes whose
 * tables are `nodes`, `edges`, `files` and their FTS shadows, so the probe
 * below finds no conversation table and skips all 27 of them rather than
 * reading 20GB of symbol rows.
 */
const MESSAGE_TABLE = "messages";

function textOfRow(value: unknown): string | undefined {
  if (typeof value !== "string" || value === "") return undefined;
  /**
   * A part row stores either bare text or the JSON encoding of the part list
   * the native transcript writes, so the same reader has to accept both.
   */
  if (!value.startsWith("[") && !value.startsWith("{")) return value;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return value;
  }
  if (Array.isArray(parsed)) return messageText(parsed);
  return isRecord(parsed) ? optionalText(parsed["text"]) : value;
}

function rowText(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

/**
 * Opened read-only so neither the database nor its write-ahead log is
 * checkpointed: the snapshot is a read-only dataset and one of its logs holds
 * 1.1MB of uncheckpointed pages that a writable open would fold into the file.
 */
function readSessionsFrom(path: string): SessionEvent[] {
  const db = new Database(path, { readonly: true });
  try {
    const tables = db
      .query("SELECT name FROM sqlite_master WHERE type = 'table'")
      .all();
    const names = new Set(
      tables
        .map((table) => (isRecord(table) ? rowText(table["name"]) : undefined))
        .filter((name): name is string => name !== undefined),
    );
    if (!names.has(MESSAGE_TABLE)) return [];
    const rows = db
      .query(
        "SELECT id, session_id, role, content, created_at FROM messages ORDER BY created_at",
      )
      .all();
    const events: SessionEvent[] = [];
    for (const row of rows) {
      if (!isRecord(row)) continue;
      const id = rowText(row["id"]);
      const session = rowText(row["session_id"]);
      const role = rowText(row["role"]);
      const createdAt = rowText(row["created_at"]);
      const text = textOfRow(row["content"]);
      if (
        id === undefined ||
        session === undefined ||
        role === undefined ||
        createdAt === undefined ||
        text === undefined ||
        text.trim() === "" ||
        !CONVERSATION_ROLES.has(role)
      ) {
        continue;
      }
      if (Number.isNaN(new Date(createdAt).getTime())) continue;
      events.push({
        type: "message",
        id,
        timestamp: createdAt,
        role,
        text,
        session,
      });
    }
    return events;
  } finally {
    db.close();
  }
}

/** The two file classes this adapter reads out of the snapshot. */
interface Sources {
  transcripts: string[];
  databases: string[];
}

/**
 * Both homes are walked from their shared parent. AppleDouble sidecars mirror
 * every file in the snapshot — 108 transcripts arrive with 108 `._` twins
 * whose binary header is not JSON — so they are excluded by name before any
 * read. `.log` captures of child process output, `.txt` notes and the
 * `posthog-activity.json` telemetry caches carry no conversation and are never
 * opened.
 */
async function sources(root: string): Promise<Sources> {
  const transcripts: string[] = [];
  const databases: string[] = [];
  const walk = async (directory: string): Promise<void> => {
    const entries = await readdir(directory, { withFileTypes: true }).catch(
      () => [],
    );
    for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      if (entry.name.startsWith("._")) continue;
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        await walk(path);
        continue;
      }
      if (!entry.isFile()) continue;
      if (entry.name === DERIVED_TRANSCRIPT) continue;
      if (entry.name.endsWith(".jsonl")) transcripts.push(path);
      else if (entry.name.endsWith(".db")) databases.push(path);
    }
  };
  await walk(root);
  return { transcripts, databases };
}

/**
 * Episodes are returned in event-time order across every session in both
 * homes, not in file order: the store links each arriving Episode to the
 * latest earlier one in its session, so a backdated arrival would start a
 * second chain head and fragment the session spine it belongs to.
 */
export async function collectOmoRaw(root: string): Promise<OmoRawEpisode[]> {
  const { transcripts, databases } = await sources(root);
  const episodes: OmoRawEpisode[] = [];
  for (const path of transcripts) {
    for (const event of await readSession(path)) {
      episodes.push(toEpisode(event));
    }
  }
  for (const path of databases) {
    for (const event of readSessionsFrom(path)) {
      episodes.push(toEpisode(event));
    }
  }
  return episodes.sort((a, b) => {
    const at = a.input.time?.value ?? "";
    const bt = b.input.time?.value ?? "";
    return at === bt
      ? a.input.origin.record.localeCompare(b.input.origin.record)
      : at.localeCompare(bt);
  });
}
