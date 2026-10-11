import { createCodexRawParser, isRecord, optionalText, type RawSessionEpisode } from "@anamnesis/backfill";
import { basename, relative, sep } from "node:path";
import { revisionOf, type ArchiveRecord } from "../record.ts";
import type { ParsedSession, SourceAdapter } from "./types.ts";

type Entry = ParsedSession["records"][number];
type Payload = Record<string, unknown>;

const TOOL_PART_TYPES = new Set(["tool_result", "tool_use", "function_call", "function_call_output"]);
const MESSAGE_TYPES = new Set(["message", "user_message", "agent_message"]);
const SYSTEM_EVENT_TYPES = new Set(["event_msg", "session_meta", "turn_context"]);

function texts(parts: unknown, keep: (part: Record<string, unknown>) => boolean): string {
  if (!Array.isArray(parts)) return "";
  return parts.flatMap((part) => isRecord(part) && typeof part["text"] === "string" && keep(part) ? [part["text"]] : []).join("\n");
}

function contentText(payload: Payload): string {
  if (typeof payload["message"] === "string") return payload["message"];
  return texts(payload["content"], (part) => !TOOL_PART_TYPES.has(String(part["type"])) && part["type"] !== "reasoning");
}

function reasoningText(payload: Payload, subtype: string): string {
  if (subtype === "reasoning") return texts(payload["summary"], () => true);
  return texts(payload["content"], (part) => part["type"] === "reasoning");
}

function roleOf(value: unknown): ArchiveRecord["role"] {
  return value === "user" || value === "assistant" || value === "system" || value === "developer" ? value : "system";
}

function actorOf(payload: Payload): string {
  const role = optionalText(payload["role"]);
  if (role !== undefined) return role;
  return payload["type"] === "user_message" ? "user" : payload["type"] === "agent_message" ? "assistant" : "unknown";
}

function occurredAt(value: Record<string, unknown>): string {
  const stamp = value["timestamp"];
  const time = typeof stamp === "string" ? Date.parse(stamp) : NaN;
  return Number.isFinite(time) ? new Date(time).toISOString() : "1970-01-01T00:00:00.000Z";
}

function canonicalOf(subtype: string, type: string): ArchiveRecord["canonical_kind"] {
  if (subtype === "context_compacted") return "compaction";
  if (MESSAGE_TYPES.has(subtype)) return "message";
  return SYSTEM_EVENT_TYPES.has(type) ? "system-event" : "other";
}

function parseLine(raw: string): Record<string, unknown> | undefined {
  try {
    const value: unknown = JSON.parse(raw);
    return isRecord(value) && isRecord(value["payload"]) ? value : undefined;
  } catch (error) {
    if (error instanceof SyntaxError) return undefined;
    throw error;
  }
}

function isToolOnly(parts: unknown): boolean {
  return Array.isArray(parts) && parts.length > 0 && parts.every((part) =>
    isRecord(part) && TOOL_PART_TYPES.has(String(part["type"])));
}

/** Tool calls, tool results and messages made exclusively of tool parts are tool records; every other message keeps its identity, even with no text. */
function omitMessage(subtype: string, parts: unknown): boolean {
  if (subtype === "function_call_output" || subtype === "function_call") return true;
  return MESSAGE_TYPES.has(subtype) && isToolOnly(parts);
}

function toEntry(value: Record<string, unknown>, episode: RawSessionEpisode | undefined, session: string, index: number, line: number): Entry | undefined {
  const payload = value["payload"] as Payload;
  const type = optionalText(value["type"]) ?? "other";
  const subtype = optionalText(payload["type"]) ?? type;
  const prose = contentText(payload);
  if (omitMessage(subtype, payload["content"])) return undefined;
  const native = optionalText(payload["id"]);
  const origin = episode?.input.origin ?? {
    source: "codex", session, actor: actorOf(payload),
    record: native === undefined ? `${session}:${index}` : `${native}:content:0`,
  };
  const reasoning = reasoningText(payload, subtype);
  const text = episode?.input.properties?.["kind"] === "compaction" ? "context_compacted"
    : MESSAGE_TYPES.has(subtype) ? prose : prose || JSON.stringify(value);
  const date = episode?.input.time?.value ?? occurredAt(value);
  return {
    origin, role: subtype === "function_call_output" ? "tool" : roleOf(origin.actor),
    occurred_at: date, time_precision: "second", text,
    ...(reasoning ? { reasoning } : {}),
    kind: subtype, canonical_kind: canonicalOf(subtype, type), revision: revisionOf(date, text), line,
  };
}

export const codexAdapter: SourceAdapter = {
  kind: "codex",
  async *files(root) {
    const paths: string[] = [];
    for await (const path of new Bun.Glob("**/rollout-*.jsonl").scan({ cwd: root, absolute: true, onlyFiles: true })) {
      const relativePath = relative(root, path).split(sep);
      if (relativePath.some((part) => part.startsWith("._") || part.startsWith("sessions-"))) continue;
      if (relativePath.join("/").includes("runtime/transcripts/")) continue;
      paths.push(path);
    }
    yield* paths.sort();
  },
  async parse(path, bytes) {
    let session = basename(path, ".jsonl").split("-").slice(-5).join("-");
    const parser = createCodexRawParser(session);
    const sessions = new Map<string, Entry[]>();
    let index = -1;
    let line = 0;
    for (const raw of new TextDecoder().decode(bytes).split("\n")) {
      line++;
      if (raw.trim() === "") continue;
      index++;
      const episodes = parser(raw);
      const value = parseLine(raw);
      if (value === undefined) continue;
      if (value["type"] === "session_meta") session = optionalText((value["payload"] as Payload)["id"]) ?? session;
      const entry = toEntry(value, episodes[0], session, index, line);
      if (entry === undefined) continue;
      const records = sessions.get(entry.origin.session) ?? [];
      records.push(entry);
      sessions.set(entry.origin.session, records);
    }
    return [...sessions].map(([sessionId, records]) => ({ sessionId, records }));
  },
};
