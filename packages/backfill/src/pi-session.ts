import { SCHEMA_ID } from "@anamnesis/protocol";
import { createHash } from "node:crypto";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** Shared reader for the pi-style session JSONL that both the gjc and omo raw stores write:
 * a `session` header line, then `message` and `compaction` events keyed by id and timestamp. */
const CONTENT_LIMIT = 4000;
export const SESSION_HEADER = "session";

export interface RawEvent {
  type: string;
  id: string;
  timestamp: string;
  role?: string;
  text: string;
}
export interface SessionEvent extends RawEvent {
  session: string;
}
export interface RawSessionEpisode {
  input: RememberInput;
  redactions: number;
}

export function optionalText(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}
/** A string that parses as a time; a torn or non-string time is skipped like an invalid database row, not thrown from toEpisode. */
function optionalTime(value: unknown): string | undefined {
  return typeof value === "string" && !Number.isNaN(Date.parse(value)) ? value : undefined;
}
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
export const asRecord = (value: unknown): Record<string, unknown> | undefined => isRecord(value) ? value : undefined;
/** Collected episodes sort by occurrence time, then by origin record, so every adapter replays in one order. */
export function byEpisodeTime(a: { input: RememberInput }, b: { input: RememberInput }): number {
  const at = a.input.time?.value ?? "";
  const bt = b.input.time?.value ?? "";
  return at === bt ? a.input.origin.record.localeCompare(b.input.origin.record) : at.localeCompare(bt);
}
/** Only `text` parts carry the turn; thinking, toolCall and toolResult parts are not conversation. */
export function messageText(content: unknown): string | undefined {
  if (!Array.isArray(content)) return undefined;
  const parts: string[] = [];
  for (const part of content) {
    if (!isRecord(part) || part["type"] !== "text") continue;
    const text = optionalText(part["text"]);
    if (text !== undefined && text.trim() !== "") parts.push(text);
  }
  return parts.length === 0 ? undefined : parts.join("\n");
}
/** `user` and `assistant` are the conversation roles; toolResult, custom and harness records are plumbing. */
export const CONVERSATION_ROLES = new Set(["user", "assistant"]);
export function parseEvent(line: string): RawEvent | undefined {
  const raw: unknown = JSON.parse(line);
  if (!isRecord(raw)) return undefined;
  const type = optionalText(raw["type"]);
  const id = optionalText(raw["id"]);
  const timestamp = optionalTime(raw["timestamp"]);
  // A missing type falls through the two type checks below.
  if (id === undefined || timestamp === undefined) return undefined;
  if (type === "compaction") {
    const summary = optionalText(raw["summary"]);
    return summary === undefined || summary.trim() === "" ? undefined : { type, id, timestamp, text: summary };
  }
  if (type !== "message") return undefined;
  const message = raw["message"];
  if (!isRecord(message)) return undefined;
  const role = optionalText(message["role"]);
  if (role === undefined || !CONVERSATION_ROLES.has(role)) return undefined;
  const text = messageText(message["content"]);
  return text === undefined ? undefined : { type, id, timestamp, role, text };
}

export interface RawOrigin {
  source: string;
  session: string;
  actor: string;
  record: string;
}
/**
 * One episode shape for every raw adapter. The revision is keyed on the raw
 * time and raw text, not the masked text or the provider's record id: a
 * masking-rule change would otherwise open a false revision of every turn a
 * new rule touches, and providers re-emit one id as the message it names
 * evolves. Byte-identical across adapters, so a turn ingested from a
 * normalized export and again from its raw transcript lands on one revision.
 */
export function rawEpisode(occurred: string | number, rawText: string, origin: RawOrigin, properties: Record<string, string>): RawSessionEpisode {
  const occurredAt = new Date(occurred).toISOString();
  const revision = createHash("sha256").update(`${occurredAt}\n${rawText}`, "utf8").digest("hex");
  const { text, redactions } = maskSecrets(rawText);
  const oversized = text.length > CONTENT_LIMIT;
  return {
    redactions,
    input: {
      schema: SCHEMA_ID.ORIGINAL_MESSAGE,
      content: oversized ? text.slice(0, CONTENT_LIMIT) : text,
      origin,
      source_revision: revision,
      time: { value: occurredAt, precision: "second" },
      ...(oversized ? { payload: new TextEncoder().encode(text), payload_media_type: "text/plain" } : {}),
      properties,
    },
  };
}
export function toEpisode(event: SessionEvent, source: string, properties: Record<string, string>): RawSessionEpisode {
  return rawEpisode(event.timestamp, event.text, { source, session: event.session, actor: event.role ?? "unknown", record: event.id }, properties);
}
/** Line parser that remembers the current session header; lines before a header yield nothing. */
export function createSessionParser<E>(build: (event: SessionEvent) => E): (line: string) => E[] {
  let session: string | undefined;
  return (line: string): E[] => {
    const raw: unknown = JSON.parse(line);
    if (isRecord(raw) && raw["type"] === SESSION_HEADER) {
      session = optionalText(raw["id"]);
      return [];
    }
    if (session === undefined) return [];
    const event = parseEvent(line);
    return event === undefined ? [] : [build({ ...event, session })];
  };
}
