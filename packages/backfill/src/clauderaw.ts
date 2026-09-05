import { createHash } from "node:crypto";
import { readdir, readFile } from "node:fs/promises";
import { basename, join, relative, sep } from "node:path";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** Beyond this the transcript turn lives in the object store, not the node. */
const CONTENT_LIMIT = 4000;

/**
 * The normalized export this adapter backfills alongside derived one origin
 * tuple per accepted record. Re-deriving the same tuple from the raw
 * transcript is what makes the overlap a record-level no-op instead of a
 * second copy of ten thousand sessions, so every rule below mirrors the
 * exporter rather than choosing its own convention:
 *
 *   origin.record   the native `uuid`/`eventId`, suffixed `:content:<index>`
 *                   when one message contributes several blocks, and
 *                   `fallback:<blake3 of the exact record bytes>:<line>`
 *                   for the transcript dialect that carries no native id
 *   time            the record timestamp at second precision
 *   content         the `text` blocks only, joined with a newline
 *
 * `source_revision` stays keyed on `${occurred_at}\n${text}` exactly as the
 * agent-log adapter keys it, so a record ingested from either side of the
 * overlap lands on one revision.
 *
 * Delegated transcripts under `subagents/` are the one place this adapter
 * goes beyond the export, which skipped them by path. They are admitted whole
 * under identities the export never minted, so nothing about the overlap
 * changes: a sidechain record keys on its own agent id, and no record the
 * export ever produced is derived differently because of it.
 */

/** Only the fields the originals contract consumes are modelled. */
interface ClaudeRecord {
  type?: string;
  subtype?: string;
  uuid?: string;
  eventId?: string;
  timestamp?: string | number;
  sessionId?: string;
  cwd?: string;
  gitBranch?: string;
  isSidechain?: boolean;
  agentId?: string;
  isApiErrorMessage?: boolean;
  summary?: string;
  content?: unknown;
  message?: ClaudeMessage;
  /** Key set decides whether the exporter sliced the record or took it whole. */
  keys: readonly string[];
}

interface ClaudeMessage {
  role?: string;
  isError?: boolean;
  content?: unknown;
  keys: readonly string[];
}

export interface ClaudeRawEpisode {
  input: RememberInput;
  redactions: number;
}

/** A record the transcript accepted, carrying everything an episode needs. */
interface AcceptedRecord {
  record: string;
  text: string;
  role: string;
  kind: string;
  occurredAt: number;
}

function optionalText(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function optionalFlag(value: unknown): boolean | undefined {
  return typeof value === "boolean" ? value : undefined;
}

function optionalStamp(value: unknown): string | number | undefined {
  if (typeof value === "string" && value !== "") return value;
  return typeof value === "number" && Number.isFinite(value)
    ? value
    : undefined;
}

function parseMessage(value: unknown): ClaudeMessage | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return undefined;
  }
  const raw: Record<string, unknown> = { ...value };
  const role = optionalText(raw["role"]);
  const isError = optionalFlag(raw["isError"]);
  return {
    ...(role === undefined ? {} : { role }),
    ...(isError === undefined ? {} : { isError }),
    ...("content" in raw ? { content: raw["content"] } : {}),
    keys: Object.keys(raw),
  };
}

/**
 * A transcript line the runtime wrote mid-crash is the one line that must not
 * abort a 13k-file backfill, so an unreadable record is skipped rather than
 * thrown: the exporter rejected it as `malformed_json` and produced nothing.
 */
function parseRecord(line: string): ClaudeRecord | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch (error) {
    if (error instanceof SyntaxError) return undefined;
    throw error;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return undefined;
  }
  const raw: Record<string, unknown> = { ...parsed };
  const type = optionalText(raw["type"]);
  const subtype = optionalText(raw["subtype"]);
  const uuid = optionalText(raw["uuid"]);
  const eventId = optionalText(raw["eventId"]);
  const timestamp = optionalStamp(raw["timestamp"]);
  const sessionId = optionalText(raw["sessionId"]);
  const cwd = optionalText(raw["cwd"]);
  const gitBranch = optionalText(raw["gitBranch"]);
  const isSidechain = optionalFlag(raw["isSidechain"]);
  const agentId = optionalText(raw["agentId"]);
  const isApiErrorMessage = optionalFlag(raw["isApiErrorMessage"]);
  const summary = optionalText(raw["summary"]);
  const message = parseMessage(raw["message"]);
  return {
    ...(type === undefined ? {} : { type }),
    ...(subtype === undefined ? {} : { subtype }),
    ...(uuid === undefined ? {} : { uuid }),
    ...(eventId === undefined ? {} : { eventId }),
    ...(timestamp === undefined ? {} : { timestamp }),
    ...(sessionId === undefined ? {} : { sessionId }),
    ...(cwd === undefined ? {} : { cwd }),
    ...(gitBranch === undefined ? {} : { gitBranch }),
    ...(isSidechain === undefined ? {} : { isSidechain }),
    ...(agentId === undefined ? {} : { agentId }),
    ...(isApiErrorMessage === undefined ? {} : { isApiErrorMessage }),
    ...(summary === undefined ? {} : { summary }),
    ...("content" in raw ? { content: raw["content"] } : {}),
    ...(message === undefined ? {} : { message }),
    keys: Object.keys(raw),
  };
}

function eventTime(value: string | number | undefined): number | undefined {
  if (value === undefined) return undefined;
  const time = typeof value === "number" ? value : Date.parse(value);
  return Number.isSafeInteger(time) && time >= 0 ? time : undefined;
}

/**
 * The `~/.claude/transcripts` dialect writes no per-record id, so the export
 * fell back to hashing the record's exact bytes. The hash covers the line as
 * it sits on disk, and the index counts every line in the file rather than
 * only the accepted ones, so both have to be carried from the reader.
 */
function recordId(
  value: ClaudeRecord,
  bytes: Uint8Array,
  lineIndex: number,
  suffix?: string,
): string | undefined {
  const native = value.uuid ?? value.eventId;
  if (native !== undefined) {
    return suffix === undefined ? native : `${native}${suffix}`;
  }
  if (value.timestamp === undefined) return undefined;
  return `fallback:${bytesToHex(blake3(bytes))}:${lineIndex}`;
}

function hasOnlyKeys(
  keys: readonly string[],
  allowed: readonly string[],
): boolean {
  const permitted = new Set(allowed);
  return keys.every((key) => permitted.has(key));
}

interface TextBlock {
  type?: string;
  text?: string;
  keys: readonly string[];
}

function parseBlock(value: unknown): TextBlock | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return undefined;
  }
  const raw: Record<string, unknown> = { ...value };
  const type = optionalText(raw["type"]);
  const text = typeof raw["text"] === "string" ? raw["text"] : undefined;
  return {
    ...(type === undefined ? {} : { type }),
    ...(text === undefined ? {} : { text }),
    keys: Object.keys(raw),
  };
}

/**
 * A compacted session survives only as the summary the runtime wrote in place
 * of the turns it replaced, so the summary is the record: dropping it would
 * leave the session with a hole no later turn explains.
 */
function compaction(
  value: ClaudeRecord,
  bytes: Uint8Array,
  lineIndex: number,
  occurredAt: number,
): AcceptedRecord | undefined {
  const summary =
    value.type === "summary"
      ? value.summary
      : value.type === "system" &&
          value.subtype === "compact_boundary" &&
          typeof value.content === "string"
        ? value.content
        : undefined;
  if (summary === undefined || summary === "") return undefined;
  const record = recordId(value, bytes, lineIndex);
  if (record === undefined) return undefined;
  return { record, text: summary, role: "unknown", kind: "compaction", occurredAt };
}

/**
 * The export kept prose and dropped plumbing: `text` blocks become records,
 * while `thinking`, `tool_use` and `tool_result` blocks do not. Reproducing
 * that inclusion set is what keeps the overlap a no-op — widening it here
 * would write records the export never had and call them duplicates.
 *
 * The sidechain flags are read as a path question rather than a record one.
 * Every one of the 121,047 delegated records in the raw tree sits under a
 * `subagents/` directory, and no main transcript carries either flag, so a
 * flagged record inside a main transcript is a record the export dropped and
 * this adapter keeps dropping — admitting it there would rewrite output the
 * export already ingested, which is the one thing widening the set must not
 * do.
 */
function acceptedRecords(
  value: ClaudeRecord,
  bytes: Uint8Array,
  lineIndex: number,
  sidechain: SidechainOrigin | undefined,
): readonly AcceptedRecord[] {
  if (
    (sidechain === undefined &&
      (value.isSidechain === true || value.agentId !== undefined)) ||
    value.isApiErrorMessage === true ||
    value.message?.isError === true
  ) {
    return [];
  }
  const occurredAt = eventTime(value.timestamp);
  if (occurredAt === undefined) return [];
  const compacted = compaction(value, bytes, lineIndex, occurredAt);
  if (compacted !== undefined) return [compacted];
  if (value.type !== "user" && value.type !== "assistant") return [];
  if (value.message?.role !== undefined && value.message.role !== value.type) {
    return [];
  }
  const role = value.type;
  const nested = value.message?.content !== undefined;
  const content = nested ? value.message?.content : value.content;
  if (typeof content === "string") {
    if (content === "") return [];
    const record = recordId(value, bytes, lineIndex);
    if (record === undefined) return [];
    return [{ record, text: content, role, kind: "agent_message", occurredAt }];
  }
  if (!Array.isArray(content) || content.length === 0) return [];
  const blocks = content.map(parseBlock);
  const accepted = blocks.flatMap((block, index) =>
    block?.type === "text" && block.text !== undefined ? [index] : [],
  );
  if (accepted.length === 0) return [];
  /**
   * A message whose blocks are all prose was stored whole under the message's
   * own id; one that mixes prose with plumbing was sliced, and each surviving
   * block took an id suffixed with its position. The suffix therefore depends
   * on the whole message, not on the block, which is why the shape is decided
   * before any block is turned into a record.
   */
  const whole =
    accepted.length === content.length &&
    hasOnlyKeys(value.keys, [
      "type",
      "uuid",
      "eventId",
      "timestamp",
      "sessionId",
      "cwd",
      nested ? "message" : "content",
    ]) &&
    (!nested ||
      hasOnlyKeys(value.message?.keys ?? [], ["role", "model", "content"])) &&
    blocks.every((block) => hasOnlyKeys(block?.keys ?? [], ["type", "text"]));
  if (whole) {
    const record = recordId(value, bytes, lineIndex);
    if (record === undefined) return [];
    const text = accepted
      .map((index) => blocks[index]?.text ?? "")
      .join("\n");
    return [{ record, text, role, kind: "agent_message", occurredAt }];
  }
  return accepted.flatMap((index) => {
    const text = blocks[index]?.text;
    const record = recordId(value, bytes, lineIndex, `:content:${index}`);
    if (text === undefined || record === undefined) return [];
    return [{ record, text, role, kind: "agent_message", occurredAt }];
  });
}

/**
 * A delegate's transcript names the session that spawned it in `sessionId`,
 * not itself, so keying the episode on `sessionId` would thread 838 separate
 * delegations onto their parent's single spine and interleave them into one
 * unreadable chain. The agent id is the transcript's own identity, so the
 * session splits on that and the parent it belongs to is kept as a property.
 */
interface SidechainOrigin {
  agentId: string;
  transcriptKind: "subagent" | "workflow";
}

interface SessionContext {
  session: string;
  cwd?: string;
  gitBranch?: string;
  /** Absent for a main transcript, which keeps its pre-sidechain shape. */
  sidechain?: SidechainSession;
}

interface SidechainSession extends SidechainOrigin {
  /** The main session this delegation hangs off, when a record names one. */
  parentSession?: string;
}

function toEpisode(
  accepted: AcceptedRecord,
  context: SessionContext,
): ClaudeRawEpisode {
  const occurredAt = new Date(accepted.occurredAt).toISOString();
  /**
   * Keyed on the raw text, not the masked one, and on the same pair the
   * agent-log adapter keys on: a record ingested from the export and again
   * from its raw transcript has to land on one revision, not two.
   */
  const revision = createHash("sha256")
    .update(`${occurredAt}\n${accepted.text}`, "utf8")
    .digest("hex");
  const { text, redactions } = maskSecrets(accepted.text);
  const properties: Record<string, string> = { kind: accepted.kind };
  if (context.cwd !== undefined) properties["cwd"] = context.cwd;
  if (context.gitBranch !== undefined) {
    properties["git_branch"] = context.gitBranch;
  }
  if (context.sidechain !== undefined) {
    properties["is_sidechain"] = "true";
    properties["agent_id"] = context.sidechain.agentId;
    properties["transcript_kind"] = context.sidechain.transcriptKind;
    if (context.sidechain.parentSession !== undefined) {
      properties["parent_session_id"] = context.sidechain.parentSession;
    }
  }
  const oversized = text.length > CONTENT_LIMIT;
  return {
    redactions,
    input: {
      schema: "anamnesis.original-message/1",
      content: oversized ? text.slice(0, CONTENT_LIMIT) : text,
      origin: {
        source: "claude-code",
        session: context.session,
        actor: accepted.role,
        record: accepted.record,
      },
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
 * Subagent and workflow transcripts sit beside the session that spawned them:
 * `<session>/subagents/agent-<id>.jsonl` for a delegated task, and one level
 * deeper under `subagents/workflows/wf_<id>/` for a workflow step. Both are
 * transcripts of real delegated work, so both are read — but as their own
 * sessions rather than as more turns of the parent.
 *
 * A workflow's `journal.jsonl` is the orchestrator's own bookkeeping, not a
 * conversation, and holds no user or assistant turn to recall.
 *
 * AppleDouble sidecars mirror every file on a copied volume and parse as
 * neither.
 */
const TRANSCRIPT_ROOTS = [
  "projects",
  "transcripts",
  "pre-compact-session-histories",
] as const;

/**
 * A transcript root is recognized wherever it sits rather than only at the
 * top of the dataset root: a snapshot of the raw tree keeps the home it was
 * copied from, so the production root holds `home/.claude/transcripts/...`
 * while a live `~/.claude` root holds `transcripts/...` directly. Anchoring
 * on the first path segment read neither dialect out of the snapshot and left
 * the delegated transcripts — matched at any depth already — as the only
 * thing the adapter collected there.
 */
function classify(pathFromRoot: string): SidechainOrigin | "main" | undefined {
  const parts = pathFromRoot.split(sep);
  if (parts.some((part) => part.startsWith("._"))) return undefined;
  if (parts.includes("subagents")) {
    const name = basename(pathFromRoot, ".jsonl");
    if (name === "journal") return undefined;
    return {
      agentId: name.replace(/^agent-/, ""),
      transcriptKind: parts.includes("workflows") ? "workflow" : "subagent",
    };
  }
  if (parts.length === 1) return "main";
  const directories = parts.slice(0, -1);
  return directories.some((part) =>
    TRANSCRIPT_ROOTS.some((known) => known === part),
  )
    ? "main"
    : undefined;
}

interface Transcript {
  path: string;
  sidechain?: SidechainOrigin;
}

async function transcriptFiles(root: string): Promise<Transcript[]> {
  const found: Transcript[] = [];
  const walk = async (directory: string): Promise<void> => {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of [...entries].sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.isFile() && entry.name.endsWith(".jsonl")) {
        const kind = classify(relative(root, path));
        if (kind === undefined) continue;
        found.push(kind === "main" ? { path } : { path, sidechain: kind });
      }
    }
  };
  await walk(root);
  return found;
}

/**
 * Splitting on the raw bytes rather than on a decoded string keeps each
 * record's exact bytes intact, which the fallback id hashes: decoding first
 * would round-trip lone surrogates and silently change the id.
 */
function recordLines(file: Uint8Array): Uint8Array[] {
  const lines: Uint8Array[] = [];
  let start = 0;
  while (start <= file.length) {
    let end = file.indexOf(0x0a, start);
    if (end === -1) end = file.length;
    if (end > start) lines.push(file.subarray(start, end));
    if (end === file.length) break;
    start = end + 1;
  }
  return lines;
}

async function collectFile(
  transcript: Transcript,
): Promise<ClaudeRawEpisode[]> {
  const { path, sidechain } = transcript;
  const file = new Uint8Array(await readFile(path));
  const decoder = new TextDecoder();
  const episodes: ClaudeRawEpisode[] = [];
  /**
   * The session a record belongs to is the one it names; the file name is
   * only the opening guess, and a resumed session renames itself mid-file.
   * A delegate names its parent there instead, so its own session is pinned
   * to the agent id and `sessionId` is recorded as the parent it hangs off.
   */
  let context: SessionContext =
    sidechain === undefined
      ? { session: basename(path, ".jsonl") }
      : { session: sidechain.agentId, sidechain };
  let lineIndex = 0;
  for (const bytes of recordLines(file)) {
    const value = parseRecord(decoder.decode(bytes));
    lineIndex += 1;
    if (value === undefined) continue;
    context = {
      session:
        sidechain === undefined
          ? (value.sessionId ?? context.session)
          : context.session,
      ...(value.cwd === undefined
        ? context.cwd === undefined
          ? {}
          : { cwd: context.cwd }
        : { cwd: value.cwd }),
      ...(value.gitBranch === undefined
        ? context.gitBranch === undefined
          ? {}
          : { gitBranch: context.gitBranch }
        : { gitBranch: value.gitBranch }),
      ...(sidechain === undefined
        ? {}
        : {
            sidechain: {
              ...sidechain,
              ...(value.sessionId === undefined
                ? context.sidechain?.parentSession === undefined
                  ? {}
                  : { parentSession: context.sidechain.parentSession }
                : { parentSession: value.sessionId }),
            },
          }),
    };
    for (const accepted of acceptedRecords(
      value,
      bytes,
      lineIndex - 1,
      sidechain,
    )) {
      episodes.push(toEpisode(accepted, context));
    }
  }
  return episodes;
}

/**
 * Episodes are returned in event-time order across every transcript, not in
 * file order: the store links each arriving Episode to the latest earlier one
 * in its session, so a backdated arrival would start a second chain head and
 * fragment the session spine it belongs to. Files are read one at a time —
 * the raw tree is several gigabytes and holding it open buys nothing, since
 * the sort needs the episodes rather than the transcripts.
 */
export async function collectClaudeRaw(
  root: string,
): Promise<ClaudeRawEpisode[]> {
  const episodes: ClaudeRawEpisode[] = [];
  for (const transcript of await transcriptFiles(root)) {
    episodes.push(...(await collectFile(transcript)));
  }
  return episodes.sort((a, b) => {
    const at = a.input.time?.value ?? "";
    const bt = b.input.time?.value ?? "";
    return at === bt
      ? a.input.origin.record.localeCompare(b.input.origin.record)
      : at.localeCompare(bt);
  });
}
