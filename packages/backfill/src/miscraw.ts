import { createHash } from "node:crypto";
import { copyFile, mkdtemp, readdir, readFile, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { Database } from "bun:sqlite";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** Beyond this the transcript turn lives in the object store, not the node. */
const CONTENT_LIMIT = 4000;

/**
 * The raw snapshot keeps one directory per agent product under `raw/`, and
 * most of those directories hold no conversation at all: an editor's crash
 * log, a password vault, a calendar cache and a not-yet-used event bus all
 * sit beside the four stores that actually carry recallable turns. Each
 * verdict below was taken against the snapshot's own bytes, and the ones that
 * carry nothing are named here rather than silently skipped, so a store that
 * starts carrying conversation later shows up as an unrecognized directory
 * instead of disappearing into a wildcard:
 *
 *   aside               ingest  `sessions/<n>/messages.jsonl` conversation turns
 *   gemini-antigravity  ingest  `brain/<id>/.../transcript.jsonl` steps
 *   opencode            ingest  `prompt-history.jsonl` submitted prompts
 *   openomni            skip    every conversation table is empty
 *   claude-project      skip    a calendar dataset, not a transcript
 *   cursor              skip    editor and extension diagnostics
 *   codex-desktop       skip    Electron IPC and updater diagnostics
 *   zed                 skip    memory and worktree diagnostics
 *   ouro                skip    structured runtime logs
 *   anamnesis           skip    this project's own sync log
 */
const SKIPPED_STORES: readonly string[] = [
  "anamnesis",
  "claude-project",
  "codex-desktop",
  "cursor",
  "openomni",
  "ouro",
  "zed",
];

export interface MiscRawEpisode {
  input: RememberInput;
  redactions: number;
}

/** One accepted turn, resolved against the store that produced it. */
interface MiscTurn {
  source: string;
  session: string;
  actor: string;
  record: string;
  occurredAt: number;
  text: string;
  properties: Record<string, string>;
}

function optionalText(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

/**
 * A transcript line the runtime wrote mid-crash is the one line that must not
 * abort the backfill, so an unreadable record is skipped rather than thrown.
 */
function parseLine(line: string): Record<string, unknown> | undefined {
  let raw: unknown;
  try {
    raw = JSON.parse(line);
  } catch {
    return undefined;
  }
  return asRecord(raw);
}

function toEpisode(turn: MiscTurn): MiscRawEpisode {
  const occurredAt = new Date(turn.occurredAt).toISOString();
  /**
   * Keyed on the raw text and the same pair every other adapter keys on, not
   * on the masked text: a change to the masking rules would otherwise rewrite
   * every revision key and open a false revision of each turn a new rule
   * touches.
   */
  const revision = createHash("sha256")
    .update(`${occurredAt}\n${turn.text}`, "utf8")
    .digest("hex");
  const { text, redactions } = maskSecrets(turn.text);
  const oversized = text.length > CONTENT_LIMIT;
  return {
    redactions,
    input: {
      schema: "anamnesis.original-message/1",
      content: oversized ? text.slice(0, CONTENT_LIMIT) : text,
      origin: {
        source: turn.source,
        session: turn.session,
        actor: turn.actor,
        record: turn.record,
      },
      source_revision: revision,
      time: { value: occurredAt, precision: "second" },
      ...(oversized
        ? {
            payload: new TextEncoder().encode(text),
            payload_media_type: "text/plain",
          }
        : {}),
      properties: turn.properties,
    },
  };
}

/**
 * AppleDouble sidecars mirror every file on a macOS-written export and parse
 * as neither JSON nor SQLite, so they are excluded at every walk rather than
 * failed on at every reader.
 */
async function entries(directory: string): Promise<string[]> {
  const found = await readdir(directory, { withFileTypes: true }).catch(
    () => [],
  );
  return found
    .filter((entry) => !entry.name.startsWith("._"))
    .map((entry) => entry.name)
    .sort((a, b) => a.localeCompare(b));
}

/** Only ever asked about a path `entries` has just listed, so it exists. */
async function isDirectory(path: string): Promise<boolean> {
  return (await stat(path)).isDirectory();
}

/**
 * A SQLite store whose last transactions are still in the write-ahead log
 * reads short unless the log is opened with it, and opening it in place would
 * write to a read-only dataset directory. Copying the trio to scratch first
 * keeps the dataset untouched while still seeing every committed row.
 */
async function readOnlyCopy(
  path: string,
  scratch: string,
): Promise<string | undefined> {
  const name = basename(path);
  const target = join(scratch, name);
  try {
    await copyFile(path, target);
  } catch {
    return undefined;
  }
  for (const suffix of ["-wal", "-shm"]) {
    await copyFile(`${path}${suffix}`, `${target}${suffix}`).catch(() => {});
  }
  return target;
}

/**
 * `bun:sqlite` opened read-only still needs the write-ahead log beside the
 * main file, which `readOnlyCopy` has already placed there.
 */
function openReadOnly(path: string): Database {
  return new Database(path, { readonly: true });
}

/** Aside: `<root>/aside/home/.aside/u/<n>/sessions/<name>/messages.jsonl`. */
const ASIDE_USERS = join("home", ".aside", "u");

/**
 * The conversation roles. `toolResult` carries command output and screenshots
 * for the turn that follows it, `user-message-metadata` carries the browser
 * tabs attached to a prompt, and `system-message` is the harness injecting
 * skill documentation and mode reminders — none of the three is a turn.
 */
const ASIDE_ROLES = new Set(["user", "assistant"]);

/**
 * Aside writes `user` turns as a plain string and `assistant` turns as a block
 * array. Only `text` blocks carry the turn: `thinking` is provider scratch
 * space the product never shows back, and `toolCall` holds the arguments of a
 * browser action rather than conversation.
 */
function asideText(content: unknown): string | undefined {
  if (typeof content === "string") {
    return content.trim() === "" ? undefined : content;
  }
  if (!Array.isArray(content)) return undefined;
  const parts: string[] = [];
  for (const item of content) {
    const block = asRecord(item);
    if (block === undefined || block["type"] !== "text") continue;
    const text = optionalText(block["text"]);
    if (text !== undefined && text.trim() !== "") parts.push(text);
  }
  return parts.length === 0 ? undefined : parts.join("\n");
}

/**
 * The session directory is named `<date>_<id>` and the id half is what the
 * product's own `state.db` joins its rows on, so the session partition is the
 * id rather than the directory name.
 */
function asideSessionId(directory: string): string {
  const index = directory.indexOf("_");
  return index === -1 ? directory : directory.slice(index + 1);
}

/**
 * `state.db` indexes the same transcript rather than duplicating it: each
 * `session_runs` row stores a byte range of the very `messages.jsonl` this
 * adapter reads, so ingesting the table too would write every turn twice. The
 * one thing the table adds is the session's title and working directory, and
 * that is all it is read for.
 */
async function asideSessions(
  userRoot: string,
  scratch: string,
): Promise<Map<string, Record<string, string>>> {
  const titles = new Map<string, Record<string, string>>();
  const copied = await readOnlyCopy(join(userRoot, "state.db"), scratch);
  if (copied === undefined) return titles;
  const database = openReadOnly(copied);
  try {
    const rows = database
      .query("select id, title, cwd from sessions")
      .all() as { id: unknown; title: unknown; cwd: unknown }[];
    for (const row of rows) {
      const id = optionalText(row.id);
      if (id === undefined) continue;
      const title = optionalText(row.title);
      const cwd = optionalText(row.cwd);
      titles.set(id, {
        ...(title === undefined ? {} : { session_title: title }),
        ...(cwd === undefined ? {} : { cwd }),
      });
    }
  } finally {
    database.close();
  }
  return titles;
}

/**
 * Aside writes no per-message id, so the record is the message's position in
 * the session file. That is stable across re-runs because the file is only
 * ever appended to, which is the same property `state.db` relies on when it
 * stores byte offsets into it.
 */
async function collectAsideStore(storeRoot: string): Promise<MiscTurn[]> {
  const turns: MiscTurn[] = [];
  const usersRoot = join(storeRoot, ASIDE_USERS);
  /**
   * The SQLite copies this store's index is read through land here rather than
   * in the dataset, which is mounted read-only and has to stay byte-identical
   * after a backfill. It is created only once a user directory is found, so a
   * snapshot without an aside store leaves no scratch behind at all.
   */
  let scratch: string | undefined;
  for (const user of await entries(usersRoot)) {
    const userRoot = join(usersRoot, user);
    if (!(await isDirectory(userRoot))) continue;
    scratch ??= await mkdtemp(join(tmpdir(), "anamnesis-miscraw-"));
    const titles = await asideSessions(userRoot, scratch);
    const sessionsRoot = join(userRoot, "sessions");
    for (const directory of await entries(sessionsRoot)) {
      const path = join(sessionsRoot, directory, "messages.jsonl");
      const raw = await readFile(path, "utf8").catch(() => undefined);
      if (raw === undefined) continue;
      const session = asideSessionId(directory);
      const context = titles.get(session) ?? {};
      let index = -1;
      for (const line of raw.split("\n")) {
        if (line.trim() === "") continue;
        index += 1;
        const record = parseLine(line);
        if (record === undefined) continue;
        const role = optionalText(record["role"]);
        if (role === undefined || !ASIDE_ROLES.has(role)) continue;
        const text = asideText(record["content"]);
        const timestamp = record["timestamp"];
        if (text === undefined || typeof timestamp !== "number") continue;
        turns.push({
          source: "aside",
          session,
          actor: role,
          record: `${session}:${index}`,
          occurredAt: timestamp,
          text,
          properties: { kind: "message", ...context },
        });
      }
    }
  }
  return turns;
}

/** Antigravity: `<root>/gemini-antigravity/home/.gemini/antigravity-cli`. */
const ANTIGRAVITY_CLI = join("home", ".gemini", "antigravity-cli");

/**
 * The conversation step types. Antigravity records every tool invocation as a
 * step of its own — `RUN_COMMAND`, `VIEW_FILE`, `CODE_ACTION` and the rest all
 * carry a rendered "Created At: … Completed At: …" tool report as `content`,
 * and `EPHEMERAL_MESSAGE` / `SYSTEM_MESSAGE` carry harness reminders that
 * announce themselves as "not actually sent by the user". None is a turn.
 */
const ANTIGRAVITY_STEPS: Readonly<Record<string, string>> = {
  USER_INPUT: "user",
  PLANNER_RESPONSE: "assistant",
};

/**
 * Antigravity wraps the submitted prompt in `<USER_REQUEST>` and appends the
 * local time and any settings the turn changed. Only the request is the turn;
 * the rest is harness metadata that would otherwise dominate every short
 * prompt's content and defeat recall on it.
 */
const USER_REQUEST = /<USER_REQUEST>\n?([\s\S]*?)\n?<\/USER_REQUEST>/;

function antigravityText(type: string, content: string): string | undefined {
  if (type !== "USER_INPUT") return content.trim() === "" ? undefined : content;
  const matched = USER_REQUEST.exec(content);
  const request = matched?.[1] ?? content;
  return request.trim() === "" ? undefined : request;
}

/**
 * The `conversations/<id>.db` files hold the same steps as protobuf blobs
 * written by the product's own Go runtime, without a schema to decode them
 * by. `brain/<id>/.system_generated/logs/transcript.jsonl` is that same
 * trajectory already decoded to JSON by the product, so the JSONL is the
 * readable copy of the database rather than a second source.
 * `transcript_full.jsonl` repeats it record for record — verified equal line
 * counts and identical fields on every conversation in the snapshot — so
 * reading both would double every turn.
 */
async function collectAntigravityStore(storeRoot: string): Promise<MiscTurn[]> {
  const turns: MiscTurn[] = [];
  const brainRoot = join(storeRoot, ANTIGRAVITY_CLI, "brain");
  for (const conversation of await entries(brainRoot)) {
    const path = join(
      brainRoot,
      conversation,
      ".system_generated",
      "logs",
      "transcript.jsonl",
    );
    const raw = await readFile(path, "utf8").catch(() => undefined);
    if (raw === undefined) continue;
    for (const line of raw.split("\n")) {
      if (line.trim() === "") continue;
      const record = parseLine(line);
      if (record === undefined) continue;
      const type = optionalText(record["type"]);
      const actor = type === undefined ? undefined : ANTIGRAVITY_STEPS[type];
      const content = optionalText(record["content"]);
      const stepIndex = record["step_index"];
      const createdAt = optionalText(record["created_at"]);
      if (
        type === undefined ||
        actor === undefined ||
        content === undefined ||
        typeof stepIndex !== "number" ||
        createdAt === undefined
      ) {
        continue;
      }
      const occurredAt = Date.parse(createdAt);
      const text = antigravityText(type, content);
      if (text === undefined || !Number.isFinite(occurredAt)) continue;
      turns.push({
        source: "gemini-antigravity",
        session: conversation,
        actor,
        record: `${conversation}:${stepIndex}`,
        occurredAt,
        text,
        properties: { kind: type },
      });
    }
  }
  return turns;
}

/** OpenCode: `<root>/opencode/home/.local/state/opencode`. */
const OPENCODE_STATE = join("home", ".local", "state", "opencode");

/**
 * The snapshot captured OpenCode's TUI state directory, not its session store:
 * there is no `messages/`, no `parts/` and no `opencode.db` here. What it does
 * hold is `prompt-history.jsonl`, the verbatim prompts submitted to the agent,
 * including the full text of everything pasted into them — recallable
 * conversation even though the replies are not in this snapshot.
 *
 * `frecency.jsonl` beside it is the file picker's ranking of recently opened
 * paths and carries no conversation, so it is not read.
 *
 * The history carries no per-entry timestamp, so the file's own modification
 * time is the only event time available. Every prompt in the file therefore
 * shares one time, and the line index both breaks that tie into a total order
 * and identifies the record.
 */
async function collectOpencodeStore(storeRoot: string): Promise<MiscTurn[]> {
  const path = join(storeRoot, OPENCODE_STATE, "prompt-history.jsonl");
  const raw = await readFile(path, "utf8").catch(() => undefined);
  if (raw === undefined) return [];
  const info = await stat(path);
  const occurredAt = info.mtime.getTime();
  const turns: MiscTurn[] = [];
  let index = -1;
  for (const line of raw.split("\n")) {
    if (line.trim() === "") continue;
    index += 1;
    const record = parseLine(line);
    if (record === undefined) continue;
    const input = optionalText(record["input"]);
    if (input === undefined || input.trim() === "") continue;
    /**
     * A pasted attachment is stored beside the prompt that carried it and is
     * elided from `input` as `[Pasted ~64 lines]`, so the prompt only reads
     * back in full with its parts appended to it.
     */
    const parts: string[] = [input];
    const attached = record["parts"];
    if (Array.isArray(attached)) {
      for (const item of attached) {
        const part = asRecord(item);
        if (part === undefined || part["type"] !== "text") continue;
        const text = optionalText(part["text"]);
        if (text !== undefined && text.trim() !== "") parts.push(text);
      }
    }
    const mode = optionalText(record["mode"]);
    turns.push({
      source: "opencode",
      session: "prompt-history",
      actor: "user",
      record: `prompt-history:${index}`,
      occurredAt,
      text: parts.join("\n"),
      properties: { kind: "prompt", ...(mode === undefined ? {} : { mode }) },
    });
  }
  return turns;
}

const COLLECTORS: Readonly<
  Record<string, (storeRoot: string) => Promise<MiscTurn[]>>
> = {
  aside: collectAsideStore,
  "gemini-antigravity": collectAntigravityStore,
  opencode: collectOpencodeStore,
};

/**
 * Episodes are returned in event-time order across every store, not in store
 * order: the store links each arriving Episode to the latest earlier one in
 * its session, so a backdated arrival would start a second chain head and
 * fragment the session spine it belongs to. An unrecognized store directory is
 * left alone rather than guessed at — the snapshot grows a directory whenever
 * a new product is captured, and guessing at its layout would either ingest
 * its diagnostics or claim to have read a transcript it never opened.
 */
export async function collectMiscRaw(root: string): Promise<MiscRawEpisode[]> {
  const turns: MiscTurn[] = [];
  for (const name of await entries(root)) {
    if (SKIPPED_STORES.includes(name)) continue;
    const collect = COLLECTORS[name];
    if (collect === undefined) continue;
    const storeRoot = join(root, name);
    if (!(await isDirectory(storeRoot))) continue;
    turns.push(...(await collect(storeRoot)));
  }
  return turns
    .map(toEpisode)
    .sort((a, b) => {
      const at = a.input.time?.value ?? "";
      const bt = b.input.time?.value ?? "";
      return at === bt
        ? a.input.origin.record.localeCompare(b.input.origin.record)
        : at.localeCompare(bt);
    });
}
