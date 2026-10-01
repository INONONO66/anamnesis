import { byEpisodeTime, optionalText, rawEpisode, type RawSessionEpisode } from "./pi-session.ts";
import { open, readdir, readFile } from "node:fs/promises";
import { join } from "node:path";

/** Only the fields the originals contract consumes are modelled. */
interface AgentEvent {
  provider: string;
  partition_id: string;
  upstream_event_id: string;
  occurred_at?: number;
  role?: string;
  canonical_kind?: string;
  kind?: string;
  text?: string;
}

export type AgentLogEpisode = RawSessionEpisode;


function requiredText(value: unknown, field: string, line: string): string {
  const text = optionalText(value);
  if (text === undefined) {
    throw new Error(`agent event without ${field}: ${line}`);
  }
  return text;
}

function optionalNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}

function parseEvent(line: string): AgentEvent {
  const raw: Record<string, unknown> = JSON.parse(line);
  const occurredAt = optionalNumber(raw["occurred_at"]);
  const role = optionalText(raw["role"]);
  const canonicalKind = optionalText(raw["canonical_kind"]);
  const kind = optionalText(raw["kind"]);
  const text = optionalText(raw["text"]);
  return {
    provider: requiredText(raw["provider"], "provider", line),
    partition_id: requiredText(raw["partition_id"], "partition_id", line),
    upstream_event_id: requiredText(
      raw["upstream_event_id"],
      "upstream_event_id",
      line,
    ),
    ...(occurredAt === undefined ? {} : { occurred_at: occurredAt }),
    ...(role === undefined ? {} : { role }),
    ...(canonicalKind === undefined ? {} : { canonical_kind: canonicalKind }),
    ...(kind === undefined ? {} : { kind }),
    ...(text === undefined ? {} : { text }),
  };
}

/** Every field the originals contract requires is present and usable. */
interface RecallableEvent extends AgentEvent {
  occurred_at: number;
  text: string;
}

/**
 * Tool plumbing and cancelled turns reach the export without content, and an
 * event without an event time cannot be placed in the session order at all.
 */
function isRecallable(event: AgentEvent): event is RecallableEvent {
  return (event.text ?? "").trim() !== "" && event.occurred_at !== undefined;
}

function toEpisode(event: RecallableEvent): AgentLogEpisode {
  const properties: Record<string, string> = {};
  if (event.canonical_kind !== undefined) {
    properties["canonical_kind"] = event.canonical_kind;
  }
  if (event.kind !== undefined) properties["kind"] = event.kind;
  return rawEpisode(event.occurred_at, event.text, {
    source: event.provider,
    session: event.partition_id,
    actor: event.role ?? "unknown",
    record: event.upstream_event_id,
  }, properties);
}

/**
 * Manifests describe the export, AppleDouble sidecars mirror it, and a
 * provider with no captured session leaves an empty file behind.
 */
async function agentLogFiles(root: string): Promise<string[]> {
  const entries = await readdir(root, { withFileTypes: true });
  return entries
    .filter(
      (entry) =>
        entry.isFile() &&
        entry.name.endsWith(".jsonl") &&
        !entry.name.startsWith("._"),
    )
    .map((entry) => entry.name)
    .sort((a, b) => a.localeCompare(b))
    .map((name) => join(root, name));
}

/** A bounded JSONL reader for sealed normalized exports. The collector below
 * retains its historical final-line and ordering behavior; runtime snapshots
 * require a newline seal and keep physical order (Engine handles chronology).
 * Non-recallable records still expose their absolute line for replay context. */
/** One log line as its recallable episode, null for blank or non-recallable lines; invalid UTF-8 or JSON is a parse error. */
function lineEpisode(bytes: Uint8Array, at: string): AgentLogEpisode | null {
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    if (text.trim() === "") return null;
    const event = parseEvent(text);
    return isRecallable(event) ? toEpisode(event) : null;
  } catch { throw new Error(`source_parse_error: ${at}`); }
}
export async function* streamAgentLogFile(path: string, maxRecordBytes = 1024 * 1024): AsyncGenerator<{ line: number; episode: AgentLogEpisode | null }> {
  if (!Number.isSafeInteger(maxRecordBytes) || maxRecordBytes < 1) throw new Error("source_record_limit_invalid");
  const file = await open(path, "r");
  try {
    if (!(await file.stat()).isFile()) throw new Error("source_not_regular_file");
    const chunk = Buffer.allocUnsafe(64 * 1024);
    const record = Buffer.allocUnsafe(maxRecordBytes);
    let length = 0, line = 0;
    while (true) {
      const { bytesRead } = await file.read(chunk, 0, chunk.length, null);
      if (!bytesRead) break;
      let start = 0;
      for (let end = 0; end < bytesRead; end++) {
        if (chunk[end] !== 10) continue;
        const size = end - start;
        if (length + size > maxRecordBytes) throw new Error(`source_record_too_large: ${path}:${line + 1}`);
        chunk.copy(record, length, start, end); length += size;
        line++;
        yield { line, episode: lineEpisode(record.subarray(0, length), `${path}:${line}`) };
        length = 0; start = end + 1;
      }
      const size = bytesRead - start;
      if (length + size > maxRecordBytes) throw new Error(`source_record_too_large: ${path}:${line + 1}`);
      chunk.copy(record, length, start, bytesRead); length += size;
    }
    if (length) throw new Error(`source_partial_final_line: ${path}:${line + 1}; snapshot incomplete, tail/rotation unsupported`);
  } finally { await file.close(); }
}

/** Historical collector: preserves event-time ordering and signature. */
export async function collectAgentLog(root: string): Promise<AgentLogEpisode[]> {
  const episodes: AgentLogEpisode[] = [];
  for (const path of await agentLogFiles(root)) {
    const raw = await readFile(path, "utf8");
    for (const line of raw.split("\n").filter((l) => l.trim() !== "")) {
      const event = parseEvent(line);
      if (isRecallable(event)) episodes.push(toEpisode(event));
    }
  }
  return episodes.sort(byEpisodeTime);
}
