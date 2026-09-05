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
 * The raw store keeps one session per file under a per-workspace directory,
 * and subagent transcripts nest one level deeper beside their parent's tool
 * logs, so the walk is recursive rather than a single readdir.
 */
const SESSIONS = join("home", ".gjc", "agent", "sessions");

/**
 * A transcript event as the agent wrote it. The outer `id` and `timestamp` are
 * the session-level identity the normalized export was built from; the inner
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

export interface GjcRawEpisode {
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
 * Only `text` parts carry the turn. `thinking` is provider scratch space the
 * export never published, and `toolCall` / `toolResult` parts hold arguments
 * and command output rather than conversation.
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
 * `user` and `assistant` are the conversation roles. `toolResult` and
 * `fileMention` reach the transcript as plumbing for the turn that follows
 * them, and the normalized export rejected both.
 */
const CONVERSATION_ROLES = new Set(["user", "assistant"]);

/**
 * Returns the event when it carries conversation content, `undefined` when the
 * line is runtime bookkeeping: model and mode changes, workspace reminders,
 * session headers and tool plumbing all reach the transcript without a turn.
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

function toEpisode(event: SessionEvent): GjcRawEpisode {
  const occurredAt = new Date(event.timestamp).toISOString();
  /**
   * Keyed exactly as the normalized export keyed it, on the raw event time and
   * raw text: the same turn reached from either side has to produce the same
   * revision, or re-reading the raw store would open a false revision of every
   * event the export already carries.
   */
  const revision = createHash("sha256")
    .update(`${occurredAt}\n${event.text}`, "utf8")
    .digest("hex");
  const { text, redactions } = maskSecrets(event.text);
  /**
   * The same shape the normalized export wrote for this turn. A `revision_key`
   * carries exactly one element body, and the digest that guards it covers
   * `properties` (docs/01 §1), so agreeing on the revision token is not enough
   * on its own: a turn reached from the export and again from raw has to build
   * the same properties or the second pass is rejected as a
   * `revision_conflict` instead of resolving to the stored revision.
   *
   * The author is already carried by `origin.actor`, which is the field the
   * export keyed it on too, so publishing it a second time under `role` only
   * adds the disagreement.
   */
  const properties: Record<string, string> = {
    canonical_kind: event.role === undefined ? "compaction" : "agent_message",
    kind: event.type,
  };
  const oversized = text.length > CONTENT_LIMIT;
  return {
    redactions,
    input: {
      schema: "anamnesis.original-message/1",
      content: oversized ? text.slice(0, CONTENT_LIMIT) : text,
      /**
       * The tuple the normalized export wrote for the same turn, so an overlap
       * between the two backfills is a record-level no-op rather than a second
       * copy of every session already ingested.
       */
      origin: {
        source: "gjc",
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
 * The session header opens every transcript and names the id the export
 * partitioned on. A file whose header never arrives cannot be attributed to a
 * session, so its events are left out rather than given a synthetic partition.
 */
async function readSession(path: string): Promise<SessionEvent[]> {
  const events: SessionEvent[] = [];
  let session: string | undefined;
  const lines = createInterface({
    input: createReadStream(path, "utf8"),
    crlfDelay: Infinity,
  });
  for await (const line of lines) {
    if (line.trim() === "") continue;
    const raw: unknown = JSON.parse(line);
    if (!isRecord(raw)) continue;
    if (raw["type"] === "session") {
      session = optionalText(raw["id"]);
      continue;
    }
    if (session === undefined) continue;
    const event = parseEvent(line);
    if (event !== undefined) events.push({ ...event, session });
  }
  return events;
}

/**
 * Transcripts sit one or two directories below the sessions root, beside the
 * `<n>.bash.log` captures of the commands their tool calls ran. AppleDouble
 * sidecars mirror every one of those files on a macOS-written export.
 */
async function transcripts(root: string): Promise<string[]> {
  const found: string[] = [];
  const walk = async (directory: string): Promise<void> => {
    const entries = await readdir(directory, { withFileTypes: true }).catch(
      () => [],
    );
    for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      if (entry.name.startsWith("._")) continue;
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.isFile() && entry.name.endsWith(".jsonl")) found.push(path);
    }
  };
  await walk(join(root, SESSIONS));
  return found;
}

/**
 * Episodes are returned in event-time order across every session file, not in
 * file order: the store links each arriving Episode to the latest earlier one
 * in its session, so a backdated arrival would start a second chain head and
 * fragment the session spine it belongs to.
 */
export async function collectGjcRaw(root: string): Promise<GjcRawEpisode[]> {
  const episodes: GjcRawEpisode[] = [];
  for (const path of await transcripts(root)) {
    for (const event of await readSession(path)) {
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
