import { parseSlackMessage, slackEpisode } from "@anamnesis/backfill";
import { readdir } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { revisionOf } from "../record.ts";
import type { ParsedSession, SourceAdapter } from "./types.ts";

async function* walk(directory: string): AsyncGenerator<string> {
  for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name.startsWith("._")) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) yield* walk(path);
    else if (entry.isFile() && (entry.name.endsWith(".json") || entry.name.endsWith(".jsonl")) && entry.name !== "index.jsonl") yield path;
  }
}

export const slackAdapter: SourceAdapter = {
  kind: "slack",
  async *files(root) {
    yield* walk(resolve(root));
  },
  async parse(path, bytes) {
    const name = basename(path);
    const parent = basename(dirname(path));
    const channel = name.endsWith(".jsonl")
      ? parent === "threads" ? name.split("-")[0] ?? "" : basename(name, ".jsonl")
      : parent;
    const raw = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    const parsed: unknown = name.endsWith(".jsonl") ? undefined : JSON.parse(raw);
    const messages: { value: string; line: number }[] = name.endsWith(".jsonl")
      ? raw.split(/\r?\n/).flatMap((value, index) => value.trim() === "" ? [] : [{ value, line: index + 1 }])
      : Array.isArray(parsed) ? parsed.map((value: unknown) => ({ value: JSON.stringify(value), line: 0 })) : [];
    const records = messages.map(({ value, line }) => {
      const message = parseSlackMessage(value);
      const input = slackEpisode(message, channel, channel).input;
      const occurred_at = input.time?.value ?? new Date(Number.parseFloat(message.ts) * 1000).toISOString();
      const text = message.text;
      const kind = message.subtype ?? "message";
      return {
        origin: input.origin, role: kind === "bot_message" ? "operator" : "user",
        occurred_at, time_precision: "second", text, kind,
        canonical_kind: kind === "message" ? "message" : "system-event",
        revision: revisionOf(occurred_at, text), line,
      } satisfies ParsedSession["records"][number];
    });
    return [{ sessionId: channel, records }];
  },
};
