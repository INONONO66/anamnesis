import { createReadStream } from "node:fs";
import { walkSorted } from "./walk.ts";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { byEpisodeTime, createSessionParser, isRecord, optionalText, parseEvent, toEpisode, type RawSessionEpisode, type SessionEvent } from "./pi-session.ts";

/** Beyond this the transcript turn lives in the object store, not the node. */

/**
 * The raw store keeps one session per file under a per-workspace directory,
 * and subagent transcripts nest one level deeper beside their parent's tool
 * logs, so the walk is recursive rather than a single readdir.
 */
const SESSIONS = join("home", ".gjc", "agent", "sessions");

export type GjcRawEpisode = RawSessionEpisode;
/** The same properties the normalized export wrote for this turn: a revision_key carries one element body
 * and its digest covers properties, so the raw pass must build them identically or be rejected as a
 * revision_conflict. The author is already carried by origin.actor. */
const gjcEpisode = (event: SessionEvent): GjcRawEpisode =>
  toEpisode(event, "gjc", { canonical_kind: event.role === undefined ? "compaction" : "agent_message", kind: event.type });
export const createGjcRawParser = (): ((line: string) => GjcRawEpisode[]) => createSessionParser(gjcEpisode);
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
  await walkSorted(join(root, SESSIONS), (entry, path) => {
    if (entry.isFile() && entry.name.endsWith(".jsonl")) found.push(path);
  });
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
      episodes.push(gjcEpisode(event));
    }
  }
  return episodes.sort(byEpisodeTime);
}
