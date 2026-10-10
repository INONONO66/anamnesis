import { notionEpisode } from "@anamnesis/backfill";
import { readdir, stat } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { revisionOf } from "../record.ts";
import type { SourceAdapter } from "./types.ts";

async function* markdownFiles(directory: string): AsyncGenerator<string> {
  for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name.startsWith("._")) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) yield* markdownFiles(path);
    else if (entry.isFile() && entry.name.endsWith(".md")) yield path;
  }
}

let exportRoot: string | undefined;

export const notionAdapter: SourceAdapter = {
  kind: "notion",
  async *files(root) {
    exportRoot = resolve(root);
    yield* markdownFiles(exportRoot);
  },
  async parse(path, bytes) {
    const raw = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    const input = notionEpisode(exportRoot ?? dirname(dirname(path)), path, raw, (await stat(path)).mtime).input;
    const occurred_at = input.time?.value ?? "";
    return [{
      sessionId: input.origin.session,
      records: [{
        origin: input.origin, role: "document", occurred_at,
        time_precision: input.time?.precision === "day" ? "day" : "second",
        text: raw, kind: "document", canonical_kind: "other",
        revision: revisionOf(occurred_at, raw), line: 0,
      }],
    }];
  },
};
