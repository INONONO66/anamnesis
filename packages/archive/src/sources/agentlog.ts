import { isRecord, rawEpisode, streamAgentLogFile } from "@anamnesis/backfill";
import { readdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { ArchiveCanonicalKind, ArchiveRole, revisionOf } from "../record.ts";
import type { ParsedSession, SourceAdapter } from "./types.ts";

type Entry = ParsedSession["records"][number];
type RememberInput = ReturnType<typeof rawEpisode>["input"];

const nonEmpty = (value: unknown, fallback: string): string => typeof value === "string" && value !== "" ? value : fallback;

/** The replayed backfill input for a line, or a fresh one when backfill skipped the record (oversized, non-recallable). */
function inputFor(event: Record<string, unknown>, replay: RememberInput | undefined): RememberInput | undefined {
  if (replay !== undefined) return replay;
  const time = event["occurred_at"];
  const provider = event["provider"], partition = event["partition_id"], id = event["upstream_event_id"];
  if (typeof time !== "number" || !Number.isFinite(time)) return undefined;
  if (typeof provider !== "string" || typeof partition !== "string" || typeof id !== "string") return undefined;
  const text = typeof event["text"] === "string" ? event["text"] : "";
  return rawEpisode(time, text, { source: provider, session: partition, actor: nonEmpty(event["role"], "unknown"), record: id }, {}).input;
}

function toEntry(event: Record<string, unknown>, input: RememberInput, line: number): Entry {
  const occurred_at = input.time?.value ?? "";
  const text = typeof event["text"] === "string" ? event["text"] : "";
  const kind = nonEmpty(event["kind"], "message");
  const canonical = ArchiveCanonicalKind.safeParse(event["canonical_kind"]);
  const role = ArchiveRole.safeParse(nonEmpty(event["role"], "unknown"));
  return {
    origin: input.origin, role: role.success ? role.data : "operator",
    occurred_at, time_precision: "second", text, kind,
    canonical_kind: canonical.success ? canonical.data : kind === "message" ? "message" : "other",
    revision: revisionOf(occurred_at, text), line,
  };
}

export const agentlogAdapter: SourceAdapter = {
  kind: "agentlog",
  async *files(root) {
    const directory = resolve(root);
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      if (entry.isFile() && entry.name.endsWith(".jsonl") && !entry.name.startsWith("._")) yield join(directory, entry.name);
    }
  },
  async parse(path, bytes) {
    const lines = new TextDecoder("utf-8", { fatal: true }).decode(bytes).split(/\r?\n/);
    const streamed = new Map<number, ReturnType<typeof rawEpisode> | null>();
    // A complete file can contain a record larger than the backfill replay guard.
    // Pass its byte length so no archive record is discarded for its size.
    for await (const item of streamAgentLogFile(path, Math.max(bytes.length, 1))) streamed.set(item.line, item.episode);
    const sessions = new Map<string, Entry[]>();
    for (const [index, line] of lines.entries()) {
      if (line.trim() === "") continue;
      const event: unknown = JSON.parse(line);
      if (!isRecord(event)) continue;
      const input = inputFor(event, streamed.get(index + 1)?.input);
      if (input === undefined) continue;
      const records = sessions.get(input.origin.session) ?? [];
      records.push(toEntry(event, input, index + 1));
      sessions.set(input.origin.session, records);
    }
    return [...sessions].map(([sessionId, records]) => ({ sessionId, records }));
  },
};
