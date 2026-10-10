import { isRecord, optionalText } from "@anamnesis/backfill";
import { Database } from "bun:sqlite";
import { readFile } from "node:fs/promises";
import { join, resolve, sep } from "node:path";
import { revisionOf } from "../record.ts";
import type { ArchiveRecord } from "../record.ts";
import type { ParsedSession, SourceAdapter } from "./types.ts";
import { readdirOrEmpty } from "./fs.ts";

type Entry = ParsedSession["records"][number];
const decoder = new TextDecoder();

function roleOf(role: string): ArchiveRecord["role"] {
  switch (role) {
    case "user": case "assistant": case "system": case "developer": case "tool":
    case "document": case "operator": return role;
    case "toolResult": return "tool";
    default: return "operator";
  }
}

function textual(value: unknown): string | undefined {
  if (typeof value === "string") return value === "" ? undefined : value;
  if (!Array.isArray(value) || value.length === 0) return undefined;
  const parts = value.filter(isRecord);
  const toolTypes = new Set(["toolCall", "toolResult", "tool_use", "tool_result", "function_call", "function_call_output"]);
  if (parts.length === 0 || parts.every((part) => toolTypes.has(String(part["type"])))) return undefined;
  return parts.flatMap((part) => part["type"] === "text" && typeof part["text"] === "string" ? [part["text"]] : []).join("\n");
}

function entry(source: string, session: string, id: string, role: string, timestamp: string, kind: string, text: string, line: number, reasoning?: string): Entry {
  const occurred_at = new Date(timestamp).toISOString();
  const canonical_kind = kind === "message" ? "message" : kind === "compaction" ? "compaction" : kind === "summary" ? "summary" : kind === "custom" || kind === "harness" ? "system-event" : "other";
  return {
    origin: { source, session, actor: role, record: id },
    role: roleOf(role), occurred_at, time_precision: "second", text, kind,
    canonical_kind, revision: revisionOf(occurred_at, text), line,
    ...(reasoning === undefined ? {} : { reasoning }),
  };
}

function headerId(bytes: Uint8Array): string | undefined {
  for (const line of decoder.decode(bytes).split(/\r?\n/)) {
    if (line.trim() === "") continue;
    try {
      const raw: unknown = JSON.parse(line);
      if (isRecord(raw) && raw["type"] === "session") return optionalText(raw["id"]);
    } catch (error) {
      if (!(error instanceof SyntaxError)) throw error;
    }
  }
  return undefined;
}

function eventEntry(raw: Record<string, unknown>, source: string, session: string, line: number): Entry | undefined {
  const kind = optionalText(raw["type"]);
  const id = optionalText(raw["id"]);
  const timestamp = optionalText(raw["timestamp"]);
  if (!kind || !id || !timestamp || Number.isNaN(Date.parse(timestamp))) return undefined;
  const message = isRecord(raw["message"]) ? raw["message"] : undefined;
  const actor = optionalText(message?.["role"]) ?? "unknown";
  const content = message?.["content"];
  const text = kind === "compaction" ? textual(raw["summary"]) : kind === "message"
    ? textual(content) : textual(raw["text"]) ?? textual(raw["summary"]) ?? textual(raw["content"]) ?? JSON.stringify(raw);
  // Only messages made exclusively of tool parts (or empty content) have no text.
  if (text === undefined) return undefined;
  return entry(source, session, id, actor, timestamp, kind, text, line, thinkingText(content));
}

function thinkingText(content: unknown): string | undefined {
  if (!Array.isArray(content)) return undefined;
  const parts = content.flatMap((part) =>
    isRecord(part) && (part["type"] === "thinking" || part["type"] === "reasoning") ? [optionalText(part["text"])] : [])
    .filter((part): part is string => part !== undefined);
  return parts.length === 0 ? undefined : parts.join("\n");
}

function sqliteRow(raw: unknown, source: string, line: number): Entry | undefined {
  if (!isRecord(raw)) return undefined;
  const id = optionalText(raw["id"]), session = optionalText(raw["session_id"]);
  const role = optionalText(raw["role"]), timestamp = optionalText(raw["created_at"]);
  if (!id || !session || !role || !timestamp || Number.isNaN(Date.parse(timestamp))) return undefined;
  let content: unknown = raw["content"];
  if (typeof content !== "string" || content === "") return undefined;
  if (content.startsWith("[") || content.startsWith("{")) {
    try { content = JSON.parse(content); } catch (error) { if (!(error instanceof SyntaxError)) throw error; }
  }
  const text = isRecord(content) ? textual(content["text"]) : textual(content);
  if (text === undefined) return undefined;
  return entry(source, session, id, role, timestamp, "message", text, line, thinkingText(content));
}

function sqliteSessions(path: string, source: string): ParsedSession[] {
  const db = new Database(path, { readonly: true });
  try {
    const tables = db.query("SELECT name FROM sqlite_master WHERE type = 'table'").all();
    if (!tables.some((table) => isRecord(table) && table["name"] === "messages")) return [];
    const sessions = new Map<string, Entry[]>();
    const rows = db.query("SELECT id, session_id, role, content, created_at FROM messages ORDER BY created_at").all();
    for (const raw of rows) {
      const record = sqliteRow(raw, source, 0);
      if (!record) continue;
      const records = sessions.get(record.origin.session) ?? [];
      records.push(record);
      sessions.set(record.origin.session, records);
    }
    return [...sessions].map(([sessionId, records]) => ({ sessionId, records }));
  } finally {
    db.close();
  }
}

async function* walk(root: string): AsyncGenerator<string> {
  const entries = await readdirOrEmpty(root);
  for (const item of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (item.name.startsWith("._")) continue;
    const path = join(root, item.name);
    if (item.isDirectory()) yield* walk(path);
    else if (item.isFile()) yield path;
  }
}

/**
 * Native pi/omo/gjc sessions are recognized by their session header, not their
 * filename. The runtime/transcripts/<id>/transcript.jsonl copy re-encodes an
 * existing native transcript, losing part boundaries, and is not a source.
 */
export function sessionAdapter(source: "omo" | "pi" | "gjc", scope: (root: string) => string): SourceAdapter {
  return {
    kind: source,
    async *files(root) {
      for await (const path of walk(resolve(scope(root)))) {
        if (source === "omo" && path.includes(`${sep}runtime${sep}transcripts${sep}`) && path.endsWith(`${sep}transcript.jsonl`)) continue;
        if (path.endsWith(".jsonl")) {
          if (headerId(await readFile(path)) !== undefined) yield path;
        } else if (source === "omo" && path.endsWith(".db")) {
          const db = new Database(path, { readonly: true });
          try {
            if (db.query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'messages'").get()) yield path;
          } finally { db.close(); }
        }
      }
    },
    async parse(path, bytes) {
      if (path.endsWith(".db")) return sqliteSessions(path, source);
      let sessionId: string | undefined;
      const sessions = new Map<string, Entry[]>();
      for (const [index, line] of decoder.decode(bytes).split(/\r?\n/).entries()) {
        if (line.trim() === "") continue;
        let raw: unknown;
        try { raw = JSON.parse(line); }
        catch (error) { if (error instanceof SyntaxError) continue; throw error; }
        if (!isRecord(raw)) continue;
        if (raw["type"] === "session") {
          sessionId = optionalText(raw["id"]);
          if (sessionId && !sessions.has(sessionId)) sessions.set(sessionId, []);
          continue;
        }
        if (!sessionId) continue;
        const record = eventEntry(raw, source, sessionId, index + 1);
        if (record) sessions.get(sessionId)?.push(record);
      }
      return [...sessions].map(([id, records]) => ({ sessionId: id, records }));
    },
  };
}

export const omoAdapter = sessionAdapter("omo", (root) => root);
