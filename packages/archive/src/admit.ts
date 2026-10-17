import { createHash } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { notionEpisode, rawEpisode, slackEpisode } from "@anamnesis/backfill";
import { RpcErrorCode, RpcRememberParams, type RpcRememberResult } from "@anamnesis/protocol";
import { z } from "zod";
import { Manifest } from "./manifest.ts";
import type { ArchiveRecord } from "./record.ts";

type Episode = ReturnType<typeof rawEpisode>["input"];

export interface Rememberer {
  remember(params: RpcRememberParams, payload?: { bytes_b64: string; media_type: string }): Promise<RpcRememberResult>;
}

const Position = z.strictObject({ session_id: z.string(), index: z.number().int().nonnegative() });
type Position = z.infer<typeof Position>;
const AdmitCursor = z.strictObject({
  status: z.enum(["in_progress", "complete"]),
  last: Position.nullable(),
  created: z.number().int().nonnegative(),
  duplicates: z.number().int().nonnegative(),
  updated_at: z.iso.datetime(),
});
type AdmitCursor = z.infer<typeof AdmitCursor>;

/** Backfill's Episode for the same source kind, rebuilt from what the archive record keeps. */
function episodeOf(source: string, record: ArchiveRecord): Episode {
  const { origin, occurred_at: at, text, kind } = record;
  switch (source) {
    case "slack": return slackEpisode({ ts: origin.record, user: origin.actor, text }, origin.session, origin.session).input;
    case "notion": return notionEpisode("/", `/${origin.record}`, text, new Date(at)).input;
    case "omo": case "pi": return rawEpisode(at, text, origin, { kind, ...(kind === "message" ? { role: origin.actor } : {}) }).input;
    case "gjc": return rawEpisode(at, text, origin, { canonical_kind: kind === "compaction" ? "compaction" : "agent_message", kind }).input;
    case "claude-code": return rawEpisode(at, text, origin, record.canonical_kind === "compaction"
      ? { canonical_kind: "compaction", kind: "compaction" } : { canonical_kind: "agent_message", kind: "message" }).input;
    case "codex": case "agentlog": return rawEpisode(at, text, origin, { kind, canonical_kind: record.canonical_kind }).input;
    default: return rawEpisode(at, text, origin, { kind }).input;
  }
}

/** A daemon refusal is the record's outcome; anything else (transport, retryable, local) stops the run. */
async function deliver(client: Rememberer, source: string, record: ArchiveRecord): Promise<{ created: boolean } | { reason: string }> {
  const input = episodeOf(source, record);
  const body = input.payload === undefined ? undefined : Buffer.from(input.payload);
  const params = RpcRememberParams.safeParse({
    episode: { schema: input.schema, time: input.time, content: input.content, origin: input.origin, properties: input.properties },
    source_revision: input.source_revision, expected_previous_revision_key: null,
    ...(body ? { payload_hash: createHash("sha256").update(body).digest("hex") } : {}),
  });
  if (!params.success) return { reason: "invalid_params" };
  let result: RpcRememberResult;
  try {
    result = await client.remember(params.data, body && { bytes_b64: body.toString("base64"), media_type: input.payload_media_type! });
  } catch (error) {
    if (!(error instanceof Error) || !("retryable" in error) || error.retryable !== false || !("code" in error)) throw error;
    const code = RpcErrorCode.safeParse(error.code);
    if (!code.success) throw error;
    return { reason: code.data };
  }
  if (result.state !== "committed") throw new Error(`remember_${result.state}`);
  return { created: result.created };
}

async function readCursor(path: string): Promise<AdmitCursor | undefined> {
  try { return AdmitCursor.parse(JSON.parse(await readFile(path, "utf8"))); }
  catch (error) { if (error instanceof Error && "code" in error && error.code === "ENOENT") return undefined; throw error; }
}

async function writeCursor(path: string, cursor: Omit<AdmitCursor, "updated_at">): Promise<void> {
  const temporary = `${path}.${process.pid}.tmp`;
  await writeFile(temporary, `${JSON.stringify({ ...cursor, updated_at: new Date().toISOString() })}\n`);
  await rename(temporary, path);
}

function* positions(manifest: Manifest, source: string, after: Position | null): Generator<{ at: Position; record: ArchiveRecord }> {
  for (const sessionId of manifest.sessionIds(source)) {
    if (after !== null && sessionId < after.session_id) continue;
    for (const [index, record] of manifest.recordsFor(source, sessionId).entries()) {
      if (after !== null && sessionId === after.session_id && index <= after.index) continue;
      yield { at: { session_id: sessionId, index }, record };
    }
  }
}

export interface AdmitReport {
  source: string;
  records: number;
  created: number;
  duplicates: number;
  failed: number;
  skipped: number;
  since: string | null;
  resumed_from: Position | null;
}

/** Counts cover this run; the cursor carries the totals of an in-progress run across resumes. */
export async function admit(options: {
  readonly archiveDir: string;
  readonly source: string;
  readonly client: Rememberer;
  readonly stopped: () => boolean;
  /** ISO datetime; records that occurred strictly before it are skipped and never move the cursor. */
  readonly since?: string;
}): Promise<AdmitReport> {
  const { archiveDir, source, client, stopped, since } = options;
  const sinceMs = since === undefined ? undefined : Date.parse(since);
  const directory = join(archiveDir, "admit");
  await mkdir(directory, { recursive: true });
  const path = join(directory, `${source}.cursor.json`);
  const previous = await readCursor(path);
  const resumed = previous?.status === "in_progress" ? previous.last : null;
  const cursor = { status: "in_progress" as const, last: resumed, created: resumed === null ? 0 : previous!.created, duplicates: resumed === null ? 0 : previous!.duplicates };
  const report: AdmitReport = { source, records: 0, created: 0, duplicates: 0, failed: 0, skipped: 0, since: since ?? null, resumed_from: resumed };
  const manifest = Manifest.open(archiveDir);
  try {
    for (const { at, record } of positions(manifest, source, resumed)) {
      if (stopped()) return report;
      if (sinceMs !== undefined && Date.parse(record.occurred_at) < sinceMs) { report.skipped++; continue; }
      report.records++;
      const outcome = await deliver(client, source, record);
      if ("reason" in outcome) {
        report.failed++;
        console.error(JSON.stringify({ source, ...at, reason: outcome.reason }));
        continue;
      }
      if (outcome.created) { report.created++; cursor.created++; } else { report.duplicates++; cursor.duplicates++; }
      cursor.last = at;
      await writeCursor(path, cursor);
    }
    await writeCursor(path, { ...cursor, last: cursor.last ?? previous?.last ?? null, status: "complete" });
    return report;
  } finally {
    manifest.close();
  }
}
