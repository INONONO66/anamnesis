import { isRecord, optionalText } from "@anamnesis/backfill";
import { join, resolve } from "node:path";
import { revisionOf } from "../record.ts";
import type { ArchiveRecord } from "../record.ts";
import type { ParsedSession, SourceAdapter } from "./types.ts";
import { readdirOrEmpty } from "./fs.ts";

type Entry = ParsedSession["records"][number];
const decoder = new TextDecoder("utf-8", { fatal: true });

async function* walk(directory: string): AsyncGenerator<string> {
  for (const entry of (await readdirOrEmpty(directory)).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name.startsWith("._")) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) yield* walk(path);
    else if (entry.isFile() && (entry.name.endsWith(".json") || entry.name.endsWith(".jsonl"))) yield path;
  }
}

function canonicalKind(messageType: string): ArchiveRecord["canonical_kind"] {
  if (messageType === "text") return "message";
  if (messageType === "type_0") return "system-event";
  return "other";
}

function row(raw: unknown, line: number): Entry | undefined {
  if (!isRecord(raw)) return undefined;
  const session = optionalText(raw["chat_id"]);
  const record = optionalText(raw["message_id"]);
  const timestamp = optionalText(raw["timestamp"]);
  if (!session || !record || !timestamp || Number.isNaN(Date.parse(timestamp))) return undefined;
  const occurred_at = new Date(timestamp).toISOString();
  const text = typeof raw["text"] === "string" ? raw["text"] : "";
  const kind = optionalText(raw["message_type"]) ?? "unknown";
  return {
    origin: { source: "kakao", session, actor: optionalText(raw["sender_id"]) ?? "unknown", record },
    role: "user", occurred_at, time_precision: "second", text, kind,
    canonical_kind: canonicalKind(kind), revision: revisionOf(occurred_at, text), line,
  };
}

/** Rows exported from the lazykatok `messages` table (sqlite3 -json or one row per JSONL line); one session per chat_id. */
export const kakaoAdapter: SourceAdapter = {
  kind: "kakao",
  async *files(root) {
    yield* walk(resolve(root));
  },
  async parse(path, bytes) {
    const raw = decoder.decode(bytes);
    const rows: { value: unknown; line: number }[] = path.endsWith(".jsonl")
      ? raw.split(/\r?\n/).flatMap((value, index) => value.trim() === "" ? [] : [{ value: JSON.parse(value) as unknown, line: index + 1 }])
      : ((parsed: unknown) => Array.isArray(parsed) ? parsed.map((value: unknown) => ({ value, line: 0 })) : [])(JSON.parse(raw));
    const sessions = new Map<string, Entry[]>();
    for (const { value, line } of rows) {
      const record = row(value, line);
      if (!record) continue;
      const records = sessions.get(record.origin.session) ?? [];
      records.push(record);
      sessions.set(record.origin.session, records);
    }
    return [...sessions].map(([sessionId, records]) => ({ sessionId, records }));
  },
};
