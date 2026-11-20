import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { appendFile, mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { OriginRole, RPC_LIMITS, RpcErrorCode, RpcRememberParams, SCHEMA_ID } from "@anamnesis/protocol";
import type { RpcClient } from "./client.ts";
import { maskSecrets } from "./secrets.ts";

type Role = (typeof OriginRole.options)[number];
/** One cleaned message: a line of `<clean>/<source>/<room file>.jsonl`. */
export interface CleanRecord {
  source: string; room: string; thread: string | null; message: string; speaker: string;
  role: Role; time: string; reply: string | null; text: string; evidence: Record<string, unknown>;
}
interface Room { file: string; first: string; my_share?: number }
interface Cursor { next: number; created: number; duplicates: number; refused: number }
type Outcome = "created" | "duplicate" | { refused: string };
type Built = { params: RpcRememberParams; payload?: Buffer } | { refused: string };
export interface LoadOptions {
  clean: string; source: string; state: string;
  concurrency: number; limit: number; rooms: readonly string[]; dryRun: boolean;
}
interface Run {
  options: LoadOptions; directory: string; stateDirectory: string; queue: Room[];
  total: { rooms: number; rooms_done: number; records: number; created: number; duplicates: number; refused: number };
  remaining: number; stop: string | undefined;
}

const ROLES: ReadonlySet<string> = new Set(OriginRole.options);
const CONTENT_LIMIT = 4000;
const CURSOR_EVERY = 200;
const PROGRESS_MS = 10_000;
const MINE_SHARE = 0.15;
const sha256 = (value: string | Buffer) => createHash("sha256").update(value).digest("hex");

export function parseRecord(line: string, at: string): CleanRecord {
  const value: unknown = JSON.parse(line);
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`clean_record_invalid: ${at}`);
  const fields = value as Record<string, unknown>;
  const text = (name: string): string => {
    const member = fields[name];
    if (typeof member !== "string" || member === "") throw new Error(`clean_record_invalid: ${at}: ${name}`);
    return member;
  };
  const optional = (name: string): string | null => {
    const member = fields[name];
    if (member !== null && (typeof member !== "string" || member === "")) throw new Error(`clean_record_invalid: ${at}: ${name}`);
    return member;
  };
  const role = text("role");
  if (!ROLES.has(role)) throw new Error(`clean_record_invalid: ${at}: role`);
  if (typeof fields["text"] !== "string") throw new Error(`clean_record_invalid: ${at}: text`);
  const evidence = fields["evidence"];
  if (evidence === null || typeof evidence !== "object" || Array.isArray(evidence)) throw new Error(`clean_record_invalid: ${at}: evidence`);
  return {
    source: text("source"), room: text("room"), thread: optional("thread"), message: text("message"), speaker: text("speaker"),
    role: role as Role, time: text("time"), reply: optional("reply"), text: fields["text"], evidence: evidence as Record<string, unknown>,
  };
}

/** One record as one Episode: identity (source, room[/thread], speaker, message), the event time, the evidence as
 * properties, and direct lineage under the record's real role. The revision is keyed on the event time and the raw
 * text, not the masked text, so a masking-rule change never opens a false revision of every message it touches. Text
 * over 4,000 characters keeps its head as content and travels whole as the payload. */
export function toRemember(record: CleanRecord): Built {
  if (record.text.trim() === "") return { refused: "empty_text" };
  const time = new Date(record.time).toISOString();
  const { text } = maskSecrets(record.text);
  const payload = text.length > CONTENT_LIMIT ? Buffer.from(text, "utf8") : undefined;
  // The cut can split a surrogate pair; the whole text is in the payload.
  const content = payload === undefined ? text : text.slice(0, CONTENT_LIMIT).replace(/[\uD800-\uDBFF]$/, "");
  const session = record.thread === null ? record.room : `${record.room}/${record.thread}`;
  const params = RpcRememberParams.safeParse({
    episode: {
      schema: SCHEMA_ID.ORIGINAL_MESSAGE, time: { value: time, precision: "second" }, content,
      origin: { source: record.source, session, actor: record.speaker, record: record.message },
      properties: {
        ...record.evidence, room: record.room,
        ...(record.thread === null ? {} : { thread: record.thread }),
        ...(record.reply === null ? {} : { reply: record.reply }),
      },
    },
    source_revision: sha256(`${time}\n${record.text}`),
    expected_previous_revision_key: null,
    ...(payload ? { payload_hash: sha256(payload) } : {}),
    origin_role: record.role, lineage_mode: "direct", parent_recall_ids: [],
  });
  if (!params.success) {
    const issue = params.error.issues[0];
    return { refused: `invalid_params:${issue?.path.join(".") ?? ""}:${issue?.message ?? ""}` };
  }
  return payload ? { params: params.data, payload } : { params: params.data };
}

async function uploadPayload(client: RpcClient, params: RpcRememberParams, bytes: Buffer): Promise<void> {
  const hash = sha256(bytes), media_type = "text/plain";
  const begin = await client.request("object.begin", { sha256: hash, size: bytes.length, media_type });
  let object;
  if (begin.state === "committed") object = begin.object;
  else {
    for (let offset = 0, seq = 0; offset < bytes.length; offset += RPC_LIMITS.chunk_bytes, seq++) {
      const result = await client.request("object.chunk", { upload_id: begin.upload_id, seq, bytes_b64: bytes.subarray(offset, offset + RPC_LIMITS.chunk_bytes).toString("base64") });
      if (result.next_seq !== seq + 1) throw new Error("payload_upload_sequence");
    }
    object = await client.request("object.commit", { upload_id: begin.upload_id });
  }
  if (object.hash !== params.payload_hash || object.size !== bytes.length || object.media_type !== media_type) throw new Error("payload_mismatch");
}

/** A daemon refusal is the record's outcome; anything else (transport, retryable, local) stops the run. */
async function deliver(client: RpcClient | null, record: CleanRecord): Promise<Outcome> {
  const built = toRemember(record);
  if ("refused" in built) return built;
  if (client === null) return "created";
  try {
    if (built.payload !== undefined) await uploadPayload(client, built.params, built.payload);
    const result = await client.request("remember", built.params);
    if (result.state !== "committed") throw new Error(`remember_${result.state}`);
    return result.created ? "created" : "duplicate";
  } catch (error) {
    if (!(error instanceof Error) || !("retryable" in error) || error.retryable !== false || !("code" in error)) throw error;
    const code = RpcErrorCode.safeParse(error.code);
    if (!code.success) throw error;
    return { refused: code.data };
  }
}

/** Splits on LF only. readline also breaks on U+2028 / U+2029, which JSON strings may hold unescaped. */
async function* linesOf(path: string): AsyncGenerator<string> {
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let carry = "";
  for await (const chunk of createReadStream(path)) {
    const parts = (carry + decoder.decode(chunk as Buffer, { stream: true })).split("\n");
    carry = parts.pop()!;
    yield* parts;
  }
  carry += decoder.decode();
  if (carry !== "") yield carry;
}

async function readCursor(path: string): Promise<Cursor> {
  let raw: string;
  try { raw = await readFile(path, "utf8"); }
  catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") return { next: 0, created: 0, duplicates: 0, refused: 0 };
    throw error;
  }
  const cursor = JSON.parse(raw) as Cursor;
  for (const field of ["next", "created", "duplicates", "refused"] as const) {
    if (!Number.isSafeInteger(cursor[field]) || cursor[field] < 0) throw new Error(`cursor_invalid: ${path}`);
  }
  return cursor;
}

async function writeCursor(path: string, cursor: Cursor): Promise<void> {
  const temporary = `${path}.${process.pid}.tmp`;
  await writeFile(temporary, `${JSON.stringify({ ...cursor, updated_at: new Date().toISOString() })}\n`);
  await rename(temporary, path);
}

/** Rooms where the owner writes come first, then by first message; this is the order extraction follows later. */
async function roomsOf(directory: string, only: readonly string[]): Promise<Room[]> {
  const rooms = (await readFile(join(directory, "_rooms.jsonl"), "utf8")).split("\n").filter(line => line !== "").map(line => JSON.parse(line) as Room);
  const group = (room: Room): number => room.my_share === undefined || room.my_share >= MINE_SHARE ? 0 : 1;
  rooms.sort((a, b) => group(a) - group(b) || (a.first < b.first ? -1 : a.first > b.first ? 1 : 0) || (a.file < b.file ? -1 : 1));
  if (only.length === 0) return rooms;
  const selected = rooms.filter(room => only.includes(room.file));
  if (selected.length !== only.length) throw new Error(`unknown --room: ${only.filter(file => !rooms.some(room => room.file === file)).join(", ")}`);
  return selected;
}

/** A room is sent strictly in file order: same-second messages keep their order only through ingest order. The cursor
 * is the line index, so a re-run sends only lines after it and a room file that only grew resumes where it stopped. */
async function loadRoom(run: Run, client: RpcClient | null, room: Room): Promise<void> {
  const { options, total } = run;
  const cursorPath = join(run.stateDirectory, `${room.file}.cursor.json`);
  const cursor = options.dryRun ? { next: 0, created: 0, duplicates: 0, refused: 0 } : await readCursor(cursorPath);
  let index = 0, sinceWrite = 0;
  try {
    for await (const line of linesOf(join(run.directory, room.file))) {
      if (index++ < cursor.next || line === "") continue;
      if (run.stop !== undefined) return;
      if (run.remaining <= 0) { run.stop ??= "limit"; return; }
      run.remaining--;
      const at = `${room.file}:${index}`;
      const record = parseRecord(line, at);
      if (record.source !== options.source) throw new Error(`clean_record_invalid: ${at}: source`);
      const outcome = await deliver(client, record);
      total.records++;
      if (outcome === "created") { cursor.created++; total.created++; }
      else if (outcome === "duplicate") { cursor.duplicates++; total.duplicates++; }
      else {
        cursor.refused++; total.refused++;
        await appendFile(join(run.stateDirectory, "_refused.jsonl"), `${JSON.stringify({ at, room: record.room, thread: record.thread, message: record.message, reason: outcome.refused })}\n`);
      }
      cursor.next = index;
      if (!options.dryRun && ++sinceWrite >= CURSOR_EVERY) { await writeCursor(cursorPath, cursor); sinceWrite = 0; }
    }
    total.rooms_done++;
  } finally {
    if (!options.dryRun && sinceWrite > 0) await writeCursor(cursorPath, cursor);
  }
}

async function worker(run: Run, connect: () => Promise<RpcClient>): Promise<void> {
  const client = run.options.dryRun ? null : await connect();
  try {
    for (let room = run.queue.shift(); room !== undefined && run.stop === undefined; room = run.queue.shift()) await loadRoom(run, client, room);
  } catch (error) {
    run.stop ??= "error";
    throw error;
  } finally {
    await client?.close();
  }
}

/** Loads one source of a cleaned corpus, rooms in parallel over `concurrency` connections. Progress and the outcome are
 * printed as `LOAD_PROGRESS` / `LOAD_DONE` / `LOAD_STOPPED` / `LOAD_FAILED` lines; refusals go to `<state>/<source>/_refused.jsonl`. */
export async function loadClean(options: LoadOptions, connect: () => Promise<RpcClient>): Promise<void> {
  const directory = join(options.clean, options.source), stateDirectory = join(options.state, options.source);
  await mkdir(stateDirectory, { recursive: true });
  const queue = await roomsOf(directory, options.rooms);
  const run: Run = { options, directory, stateDirectory, queue, remaining: options.limit, stop: undefined,
    total: { rooms: queue.length, rooms_done: 0, records: 0, created: 0, duplicates: 0, refused: 0 } };
  let signal: number | undefined;
  const onInterrupt = () => { signal ??= 130; run.stop ??= "signal"; }, onTerminate = () => { signal ??= 143; run.stop ??= "signal"; };
  const started = performance.now();
  const report = (label: string, extra: Record<string, unknown> = {}): void => {
    const seconds = (performance.now() - started) / 1000;
    console.log(`${label} ${JSON.stringify({ source: options.source, ...run.total, seconds: Math.round(seconds), per_second: Math.round(run.total.records / Math.max(seconds, 0.001)), ...extra })}`);
  };
  process.on("SIGINT", onInterrupt); process.on("SIGTERM", onTerminate);
  const progress = setInterval(() => report("LOAD_PROGRESS"), PROGRESS_MS);
  try {
    const results = await Promise.allSettled(Array.from({ length: Math.min(options.concurrency, Math.max(queue.length, 1)) }, () => worker(run, connect)));
    const failure = results.find((result): result is PromiseRejectedResult => result.status === "rejected");
    if (failure !== undefined) {
      report("LOAD_FAILED", { error: failure.reason instanceof Error ? failure.reason.message : String(failure.reason) });
      throw failure.reason;
    }
    if (run.stop !== undefined) {
      report("LOAD_STOPPED", { reason: run.stop });
      if (signal !== undefined) process.exitCode = signal;
      return;
    }
    report("LOAD_DONE", { dry_run: options.dryRun });
  } finally {
    clearInterval(progress);
    process.off("SIGINT", onInterrupt); process.off("SIGTERM", onTerminate);
  }
}
