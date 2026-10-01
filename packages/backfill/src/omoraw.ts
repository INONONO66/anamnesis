import { createReadStream } from "node:fs";
import { walkSorted } from "./walk.ts";
import { createInterface } from "node:readline";
import { Database } from "bun:sqlite";
import { CONVERSATION_ROLES, SESSION_HEADER, byEpisodeTime, createSessionParser, isRecord, messageText, optionalText, parseEvent, toEpisode, type RawSessionEpisode, type SessionEvent } from "./pi-session.ts";

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

export type OmoRawEpisode = RawSessionEpisode;
const omoEpisode = (event: SessionEvent): OmoRawEpisode => {
  const properties: Record<string, string> = { kind: event.type };
  if (event.role !== undefined) properties["role"] = event.role;
  return toEpisode(event, "omo", properties);
};
export const createOmoRawParser = (): ((line: string) => OmoRawEpisode[]) => createSessionParser(omoEpisode);
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
  await walkSorted(root, (entry, path) => {
    if (!entry.isFile() || entry.name === DERIVED_TRANSCRIPT) return;
    if (entry.name.endsWith(".jsonl")) transcripts.push(path);
    else if (entry.name.endsWith(".db")) databases.push(path);
  });
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
      episodes.push(omoEpisode(event));
    }
  }
  for (const path of databases) {
    for (const event of readSessionsFrom(path)) {
      episodes.push(omoEpisode(event));
    }
  }
  return episodes.sort(byEpisodeTime);
}
