import { classifyClaudeTranscript, createClaudeRawParser, isRecord, optionalText, type RawSessionEpisode } from "@anamnesis/backfill";
import { basename, join, relative, sep } from "node:path";

/** backfill classifies paths relative to ~/.claude; a root that is itself the projects directory (any name, e.g. a snapshot) is classified as if it were. */
function isTranscript(fromRoot: string): boolean {
  return classifyClaudeTranscript(fromRoot) !== undefined || classifyClaudeTranscript(join("projects", fromRoot)) !== undefined;
}
import { revisionOf, type ArchiveRecord } from "../record.ts";
import type { ParsedSession, SourceAdapter } from "./types.ts";

type Entry = ParsedSession["records"][number];
const TOOL_PART_TYPES = new Set(["tool_use", "tool_result"]);
type Line = { value: Record<string, unknown>; episodes: RawSessionEpisode[]; line: number; fallback: string };
type Decoded = {
  kind: string; canonical_kind: ArchiveRecord["canonical_kind"]; content: unknown; message: Record<string, unknown> | undefined;
  text: string; prose: string; reasoning: string;
};

function blocks(content: unknown, type: string): string[] {
  if (!Array.isArray(content)) return [];
  return content.flatMap((part) => {
    if (!isRecord(part) || part["type"] !== type) return [];
    const text = type === "thinking" && typeof part["thinking"] === "string" ? part["thinking"] : part["text"];
    return typeof text === "string" ? [text] : [];
  });
}

/** Only a message made exclusively of tool parts is a tool record; any other message keeps its identity, even with no prose or thinking text. */
function toolOnly(content: unknown): boolean {
  return Array.isArray(content) && content.length > 0 && content.every((part) => isRecord(part) && TOOL_PART_TYPES.has(String(part["type"])));
}

function roleOf(role: unknown): ArchiveRecord["role"] {
  return role === "user" || role === "assistant" || role === "system" || role === "developer" ? role : "system";
}

function timestampOf(value: Record<string, unknown>, previous: string): string {
  const stamp = value["timestamp"];
  const millis = typeof stamp === "number" ? stamp : typeof stamp === "string" ? Date.parse(stamp) : NaN;
  return Number.isSafeInteger(millis) && millis >= 0 ? new Date(millis).toISOString() : previous;
}

function canonicalOf(kind: string): ArchiveRecord["canonical_kind"] {
  if (kind === "summary") return "summary";
  if (kind === "system") return "system-event";
  return kind === "user" || kind === "assistant" ? "message" : "other";
}

function decode(value: Record<string, unknown>): Decoded | undefined {
  const message = isRecord(value["message"]) ? value["message"] : undefined;
  const content = message?.["content"] ?? value["content"];
  const prose = typeof content === "string" ? content : blocks(content, "text").join("\n");
  const reasoning = blocks(content, "thinking").join("\n");
  const kind = optionalText(value["type"]) ?? "other";
  if ((kind === "user" || kind === "assistant") && toolOnly(content)) return undefined;
  const text = kind === "user" || kind === "assistant" ? prose
    : prose || (kind === "summary" ? optionalText(value["summary"]) : undefined)
      || (typeof value["content"] === "string" ? value["content"] : undefined) || JSON.stringify(value);
  return { kind, canonical_kind: canonicalOf(kind), content, message, text, prose, reasoning };
}

function parityEntries(decoded: Decoded, episodes: RawSessionEpisode[], line: number, occurredAt: string): Entry[] {
  const { content, text, reasoning, kind, canonical_kind } = decoded;
  const parts = blocks(content, "text");
  return episodes.map((episode) => {
    const origin = episode.input.origin;
    const suffix = /:content:(\d+)$/.exec(origin.record);
    const piece = suffix === null ? (typeof content === "string" ? content : parts.join("\n"))
      : blocks([Array.isArray(content) ? content[Number(suffix[1])] : undefined], "text")[0] ?? "";
    const date = episode.input.time?.value ?? occurredAt;
    return {
      origin, role: roleOf(origin.actor), occurred_at: date, time_precision: "second",
      text: piece === "" ? text : piece, ...(reasoning ? { reasoning } : {}),
      kind, canonical_kind: episode.input.properties?.["kind"] === "compaction" ? "compaction" : canonical_kind,
      revision: revisionOf(date, piece === "" ? text : piece), line,
    };
  });
}

function nativeEntry(decoded: Decoded, input: Line, session: string, occurredAt: string): Entry {
  const { value, line, fallback } = input;
  const { message, text, reasoning, kind, canonical_kind } = decoded;
  const native = optionalText(value["uuid"]) ?? optionalText(value["eventId"]);
  const actor = optionalText(message?.["role"]) ?? (kind === "user" || kind === "assistant" ? kind : "unknown");
  return {
    origin: { source: "claude-code", session, actor, record: native ?? fallback },
    role: roleOf(actor), occurred_at: occurredAt, time_precision: "second",
    text, ...(reasoning ? { reasoning } : {}),
    kind, canonical_kind, revision: revisionOf(occurredAt, text), line,
  };
}

function* jsonLines(bytes: Uint8Array, parser: (raw: Uint8Array) => RawSessionEpisode[], session: () => string): Generator<Line> {
  let start = 0, line = 0, index = 0;
  while (start < bytes.length) {
    const end = bytes.indexOf(10, start);
    const stop = end < 0 ? bytes.length : end;
    const raw = bytes.subarray(start, stop);
    start = stop + 1;
    line++;
    if (raw.length === 0) continue;
    const episodes = parser(raw);
    const fallback = `${session()}:${index++}`;
    let value: unknown;
    try {
      value = JSON.parse(new TextDecoder().decode(raw));
    } catch (error) {
      if (error instanceof SyntaxError) continue;
      throw error;
    }
    if (isRecord(value)) yield { value, episodes, line, fallback };
  }
}

export const claudeAdapter: SourceAdapter = {
  kind: "claude-code",
  async *files(root) {
    const candidates: string[] = [];
    for await (const path of new Bun.Glob("**/*.jsonl").scan({ cwd: root, absolute: true, onlyFiles: true })) {
      const fromRoot = relative(root, path);
      if (fromRoot.split(sep).join("/").includes("runtime/transcripts/")) continue;
      if (isTranscript(fromRoot)) candidates.push(path);
    }
    yield* candidates.sort();
  },
  async parse(path, bytes) {
    const classified = classifyClaudeTranscript(path) ?? "main";
    const sidechain = classified === "main" ? undefined : classified;
    const parser = createClaudeRawParser({ path, ...(sidechain === undefined ? {} : { sidechain }) });
    let session = sidechain?.agentId ?? basename(path, ".jsonl");
    let occurredAt = "1970-01-01T00:00:00.000Z";
    const sessions = new Map<string, Entry[]>();
    for (const input of jsonLines(bytes, parser, () => session)) {
      const { value, episodes, line } = input;
      if (sidechain === undefined) session = optionalText(value["sessionId"]) ?? session;
      occurredAt = timestampOf(value, occurredAt);
      const decoded = decode(value);
      if (decoded === undefined) continue;
      const entries = episodes.length > 0 ? parityEntries(decoded, episodes, line, occurredAt) : [nativeEntry(decoded, input, session, occurredAt)];
      for (const entry of entries) {
        const records = sessions.get(entry.origin.session) ?? [];
        records.push(entry);
        sessions.set(entry.origin.session, records);
      }
    }
    return [...sessions].map(([sessionId, records]) => ({ sessionId, records }));
  },
};
