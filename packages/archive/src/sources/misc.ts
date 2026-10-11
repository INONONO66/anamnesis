import { asideSessionId, isRecord, messageText, optionalText } from "@anamnesis/backfill";
import { stat } from "node:fs/promises";
import { basename, join, resolve, sep } from "node:path";
import { revisionOf } from "../record.ts";
import type { ArchiveRecord } from "../record.ts";
import type { ParsedSession, SourceAdapter } from "./types.ts";
import { isFileOrAbsent, readdirOrEmpty } from "./fs.ts";

type Entry = ParsedSession["records"][number];
type Context =
  | { source: "aside"; session: string }
  | { source: "gemini-antigravity"; session: string }
  | { source: "opencode"; session: "prompt-history"; occurredAt: number };

function contextFor(path: string, occurredAt: number): Context | undefined {
  const parts = resolve(path).split(sep);
  if (basename(path) === "messages.jsonl" && parts.includes(".aside") && parts.includes("sessions")) {
    return { source: "aside", session: asideSessionId(parts.at(-2) ?? "") };
  }
  if (basename(path) === "transcript.jsonl" && parts.includes("antigravity-cli") && parts.includes("brain")) {
    return { source: "gemini-antigravity", session: parts.at(-4) ?? "" };
  }
  if (basename(path) === "prompt-history.jsonl" && parts.includes("opencode")) {
    return { source: "opencode", session: "prompt-history", occurredAt };
  }
  return undefined;
}

function contentText(value: unknown): string | undefined {
  if (typeof value === "string") return value.trim() === "" ? undefined : value;
  return messageText(value);
}

function roleOf(actor: string): ArchiveRecord["role"] {
  switch (actor) {
    case "user": case "assistant": case "system": case "developer": case "tool":
    case "document": case "operator": return actor;
    case "toolResult": return "tool";
    default: return "operator";
  }
}

function add(records: Entry[], context: Context, actor: string, id: string, occurred: number | string, kind: string, text: string, line: number, reasoning?: string): void {
  const occurred_at = new Date(occurred).toISOString();
  records.push({
    origin: { source: context.source, session: context.session, actor, record: id },
    role: roleOf(actor), occurred_at, time_precision: "second", text, kind,
    canonical_kind: kind === "message" || kind === "prompt" || kind === "USER_INPUT" || kind === "PLANNER_RESPONSE"
      ? "message" : kind === "compaction" ? "compaction" : kind === "summary" ? "summary"
        : kind === "SYSTEM_MESSAGE" || kind === "EPHEMERAL_MESSAGE" || kind === "custom" ? "system-event" : "other",
    revision: revisionOf(occurred_at, text), line,
    ...(reasoning ? { reasoning } : {}),
  });
}

function asideRecord(records: Entry[], context: Extract<Context, { source: "aside" }>, value: Record<string, unknown>, ordinal: number, line: number): void {
  const actor = optionalText(value["role"]) ?? "unknown";
  const timestamp = value["timestamp"];
  if (typeof timestamp !== "number" || !Number.isFinite(timestamp)) return;
  const kind = optionalText(value["type"]) ?? "message";
  const content = value["content"];
  const text = kind === "compaction" ? contentText(value["summary"]) ?? contentText(content)
    : contentText(content) ?? (kind === "message" ? undefined : JSON.stringify(value));
  if (text === undefined) return;
  const reasoning = Array.isArray(content) ? content.filter((part) => isRecord(part) && (part["type"] === "thinking" || part["type"] === "reasoning"))
    .map((part) => isRecord(part) ? optionalText(part["text"]) : undefined).filter((part): part is string => part !== undefined).join("\n") : "";
  add(records, context, actor, `${context.session}:${ordinal}`, timestamp, kind, text, line, reasoning);
}

function antigravityRecord(records: Entry[], context: Extract<Context, { source: "gemini-antigravity" }>, value: Record<string, unknown>, line: number): void {
  const kind = optionalText(value["type"]), step = value["step_index"], created = optionalText(value["created_at"]);
  if (!kind || typeof step !== "number" || !created || Number.isNaN(Date.parse(created))) return;
  const actor = kind === "USER_INPUT" ? "user" : kind === "PLANNER_RESPONSE" ? "assistant"
    : kind === "SYSTEM_MESSAGE" || kind === "EPHEMERAL_MESSAGE" ? "system" : kind.includes("TOOL") || kind === "RUN_COMMAND" ? "tool" : "operator";
  const content = contentText(value["content"]);
  const text = kind === "USER_INPUT" && content !== undefined
    ? /<USER_REQUEST>\n?([\s\S]*?)\n?<\/USER_REQUEST>/.exec(content)?.[1] ?? content
    : content ?? JSON.stringify(value);
  add(records, context, actor, `${context.session}:${step}`, created, kind, text, line);
}

function opencodeRecord(records: Entry[], context: Extract<Context, { source: "opencode" }>, value: Record<string, unknown>, ordinal: number, line: number): void {
  const input = contentText(value["input"]);
  const parts = messageText(value["parts"]);
  const text = [input, parts].filter((part): part is string => part !== undefined).join("\n");
  if (text !== "") add(records, context, "user", `prompt-history:${ordinal}`, context.occurredAt, "prompt", text, line);
}

async function* asideFiles(root: string): AsyncGenerator<string> {
  const users = await readdirOrEmpty(join(root, "aside", "home", ".aside", "u"));
  for (const user of users.sort((a, b) => a.name.localeCompare(b.name))) {
    if (!user.isDirectory() || user.name.startsWith("._")) continue;
    const sessions = await readdirOrEmpty(join(root, "aside", "home", ".aside", "u", user.name, "sessions"));
    for (const session of sessions.sort((a, b) => a.name.localeCompare(b.name))) {
      if (session.isDirectory() && !session.name.startsWith("._")) {
        const path = join(root, "aside", "home", ".aside", "u", user.name, "sessions", session.name, "messages.jsonl");
        if (await isFileOrAbsent(path)) yield path;
      }
    }
  }
}

export const miscAdapter: SourceAdapter = {
  kind: "misc",
  async *files(root) {
    const paths: string[] = [];
    for await (const path of asideFiles(root)) paths.push(path);
    const brain = join(root, "gemini-antigravity", "home", ".gemini", "antigravity-cli", "brain");
    const conversations = await readdirOrEmpty(brain);
    for (const conversation of conversations) {
      if (!conversation.isDirectory() || conversation.name.startsWith("._")) continue;
      const path = join(brain, conversation.name, ".system_generated", "logs", "transcript.jsonl");
      if (await isFileOrAbsent(path)) paths.push(path);
    }
    const opencode = join(root, "opencode", "home", ".local", "state", "opencode", "prompt-history.jsonl");
    if (await isFileOrAbsent(opencode)) paths.push(opencode);
    for (const path of paths.sort((a, b) => a.localeCompare(b))) yield resolve(path);
  },
  async parse(path, bytes) {
    const info = await stat(path);
    const context = contextFor(path, info.mtime.getTime());
    if (!context) return [];
    const records: Entry[] = [];
    let ordinal = -1;
    for (const [lineNumber, line] of new TextDecoder().decode(bytes).split(/\r?\n/).entries()) {
      if (line.trim() === "") continue;
      ordinal++;
      let value: unknown;
      try { value = JSON.parse(line); }
      catch (error) { if (error instanceof SyntaxError) continue; throw error; }
      if (!isRecord(value)) continue;
      switch (context.source) {
        case "aside": asideRecord(records, context, value, ordinal, lineNumber + 1); break;
        case "gemini-antigravity": antigravityRecord(records, context, value, lineNumber + 1); break;
        case "opencode": opencodeRecord(records, context, value, ordinal, lineNumber + 1); break;
      }
    }
    return [{ sessionId: context.session, records }];
  },
};
