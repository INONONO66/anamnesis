import { createHash } from "node:crypto";
import { readdir, readFile, stat } from "node:fs/promises";
import { basename, join, relative, sep } from "node:path";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** The document body is the payload; the graph node keeps a short excerpt. */
const EXCERPT_LIMIT = 512;

export interface NotionEpisode {
  input: RememberInput;
  redactions: number;
}

async function markdownFiles(root: string): Promise<string[]> {
  const found: string[] = [];
  const walk = async (directory: string): Promise<void> => {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.name.endsWith(".md")) found.push(path);
    }
  };
  await walk(root);
  return found;
}

function excerpt(title: string, body: string): string {
  const head = body.replace(/\s+/g, " ").trim().slice(0, EXCERPT_LIMIT);
  return head === "" ? title : `${title}\n\n${head}`;
}

/**
 * A document revision is its masked content hash: re-running the backfill over
 * an unchanged export is a no-op, while an edited page opens a new revision.
 */
export async function collectNotion(root: string): Promise<NotionEpisode[]> {
  const episodes: NotionEpisode[] = [];
  for (const path of await markdownFiles(root)) {
    const raw = await readFile(path, "utf8");
    const { text, redactions } = maskSecrets(raw);
    const relpath = relative(root, path);
    const [workspace = "notion"] = relpath.split(sep);
    const hash = createHash("sha256").update(text, "utf8").digest("hex");
    const info = await stat(path);
    const title = basename(path, ".md");
    episodes.push({
      redactions,
      input: {
        schema: "anamnesis.original-document/1",
        content: excerpt(title, text),
        origin: {
          source: "notion",
          session: workspace,
          actor: "export",
          record: relpath,
        },
        source_revision: hash,
        time: { value: info.mtime.toISOString(), precision: "day" },
        payload: new TextEncoder().encode(text),
        payload_media_type: "text/markdown",
        properties: { title, path: relpath },
      },
    });
  }
  return episodes;
}
