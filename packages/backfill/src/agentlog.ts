import { readdir, readFile } from "node:fs/promises";
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
      // Events are immutable appends: the event id is the only revision.
      source_revision: event.upstream_event_id,
      time: {
        value: new Date(event.occurred_at).toISOString(),
        precision: "second",
      },
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
async function logFiles(root: string): Promise<string[]> {
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

/**
 * Episodes are returned in event-time order across every provider file, not in
 * file order: the store links each arriving Episode to the latest earlier one
 * in its session, so a backdated arrival would start a second chain head and
 * fragment the session spine it belongs to.
 */
export async function collectAgentLog(root: string): Promise<AgentLogEpisode[]> {
  const episodes: AgentLogEpisode[] = [];
  for (const path of await logFiles(root)) {
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
