import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { Manifest } from "./manifest.ts";
import { copyRaw } from "./raw.ts";
import { ArchiveRecord } from "./record.ts";
import type { SourceAdapter } from "./sources/types.ts";

export async function collect(options: {
  readonly adapter: SourceAdapter;
  readonly root: string;
  readonly archiveDir: string;
}): Promise<{ files: number; sessions: number; records: number; skipped: number }> {
  const { adapter, root, archiveDir } = options;
  const manifest = Manifest.open(archiveDir);
  let files = 0;
  let sessions = 0;
  let records = 0;
  let skipped = 0;
  try {
    // The adapter's files() excludes re-encoded copies (for example omo runtime/transcripts).
    for await (const path of adapter.files(root)) {
      files++;
      const bytes = await readFile(path);
      const sha256 = createHash("sha256").update(bytes).digest("hex");
      if (manifest.hasFile(adapter.kind, sha256)) {
        manifest.addPath(adapter.kind, sha256, path);
        skipped++;
      } else {
        const parsed = await adapter.parse(path, bytes);
        const output: ArchiveRecord[] = [];
        for (const session of parsed) {
          for (const record of session.records) {
            const { line, ...fields } = record;
            output.push(ArchiveRecord.parse({
              ...fields,
              seq: output.length,
              file: { sha256, path, line },
            }));
          }
        }
        await copyRaw(path, archiveDir, bytes);
        manifest.insertSession({
          sha256, source: adapter.kind, sessionId: parsed[0]?.sessionId ?? "",
          sessionIds: parsed.map((session) => session.sessionId),
          bytes: bytes.length, path, collectedAt: new Date().toISOString(),
        }, output);
        sessions += parsed.length;
        records += output.length;
      }
      const directory = join(archiveDir, "records", adapter.kind);
      await mkdir(directory, { recursive: true });
      for (const sessionId of manifest.sessionIdsForFile(adapter.kind, sha256)) {
        const destination = join(directory, `${encodeURIComponent(sessionId)}.jsonl`);
        const lines = manifest.recordsFor(adapter.kind, sessionId).map((record) => JSON.stringify(record));
        await writeFile(destination, lines.length === 0 ? "" : `${lines.join("\n")}\n`);
      }
    }
    return { files, sessions, records, skipped };
  } finally {
    manifest.close();
  }
}
