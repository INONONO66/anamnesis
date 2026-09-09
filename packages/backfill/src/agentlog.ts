import { createHash } from "node:crypto";
import { open, readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** Beyond this the transcript turn lives in the object store, not the node. */
const CONTENT_LIMIT = 4000;

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

export interface AgentLogEpisode {
  input: RememberInput;
  redactions: number;
}

function optionalText(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

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
  const occurredAt = new Date(event.occurred_at).toISOString();
  /**
   * Keyed on the raw event, not the masked one: a change to the masking rules
   * would otherwise rewrite every revision key and open a false revision of
   * every event whose text a new rule touches.
   */
  const revision = createHash("sha256")
    .update(`${occurredAt}\n${event.text}`, "utf8")
    .digest("hex");
  const { text, redactions } = maskSecrets(event.text);
  const properties: Record<string, string> = {};
  if (event.canonical_kind !== undefined) {
    properties["canonical_kind"] = event.canonical_kind;
  }
  if (event.kind !== undefined) properties["kind"] = event.kind;
  const oversized = text.length > CONTENT_LIMIT;
  return {
    redactions,
    input: {
      schema: "anamnesis.original-message/1",
      content: oversized ? text.slice(0, CONTENT_LIMIT) : text,
      origin: {
        source: event.provider,
        session: event.partition_id,
        actor: event.role ?? "unknown",
        record: event.upstream_event_id,
      },
      /**
       * Providers re-emit one event id as the message it names evolves, so the
       * id alone is not a revision: keying the revision on the content instead
       * lets a re-emission supersede its predecessor through INVALIDATES,
       * where keying it on the id collides two contents on one revision.
       */
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
 * Manifests describe the export, AppleDouble sidecars mirror it, and a
 * provider with no captured session leaves an empty file behind.
 */
export async function agentLogFiles(root: string): Promise<string[]> {
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
        let episode: AgentLogEpisode | null = null;
        try {
          const text = new TextDecoder("utf-8", { fatal: true }).decode(record.subarray(0, length));
          if (text.trim() !== "") {
            const event = parseEvent(text);
            if (isRecallable(event)) episode = toEpisode(event);
          }
        } catch { throw new Error(`source_parse_error: ${path}:${line}`); }
        yield { line, episode };
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
  return episodes.sort((a, b) => {
    const at = a.input.time?.value ?? "";
    const bt = b.input.time?.value ?? "";
    return at === bt
      ? a.input.origin.record.localeCompare(b.input.origin.record)
      : at.localeCompare(bt);
  });
}
