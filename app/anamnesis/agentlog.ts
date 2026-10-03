import { logEvent } from "./log.ts";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, readdir } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { streamAgentLogFile } from "@anamnesis/backfill";
import { RpcRememberParams } from "@anamnesis/protocol";
import { RpcClient } from "./client.ts";
import { Revisions } from "./raw-lane.ts";
import { ingestSnapshot, type SourceRecord } from "./source.ts";

// Finite source metadata, not an episode collection. Larger exports must be
// partitioned intentionally; no eviction can invent a missing predecessor.
const MAX_FILES = 1024;
const sha = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
async function files(root: string): Promise<string[]> {
  const entries = await readdir(root);
  const names = entries.filter(name => name.endsWith(".jsonl") && !name.startsWith("._")).sort();
  if (!names.length) throw new Error("source_no_export_files");
  if (names.length > MAX_FILES) throw new Error("source_file_limit");
  return names;
}
async function fingerprint(path: string): Promise<string> {
  const info = await lstat(path, { bigint: true });
  if (!info.isFile()) throw new Error(`source_not_regular_file: ${path}`);
  return `${info.dev}:${info.ino}:${info.size}:${info.mtimeNs}:${info.ctimeNs}:${info.mode}`;
}

/** Immutable normalized export only: no watcher, append, rotation, SQLite or
 * inferred live history. A full byte-hashed manifest is the durable replay
 * anchor; parsing and occurrence mapping are replayed in physical file order. */
export async function ingestAgentLog(root: string, checkpointPath: string, client: RpcClient): Promise<void> {
  await ingestSnapshot(checkpointPath, client, async () => {
    const names = await files(root);
    const paths = names.map(name => join(root, name));
    // The .pending.json sidecar needs no check here: files() admits only *.jsonl names.
    if (paths.some(path => resolve(path) === resolve(checkpointPath))) throw new Error("source_checkpoint_path_conflict");
    const manifest: { file: string; sha256: string }[] = [];
    const fingerprints = new Map<string, string>();
    for (const path of paths) {
      const before = await fingerprint(path);
      const hash = createHash("sha256");
      for await (const chunk of createReadStream(path)) hash.update(chunk);
      fingerprints.set(path, before);
      manifest.push({ file: basename(path), sha256: hash.digest("hex") });
      // Reject partial/invalid/oversized exports before any graph delivery; a file that changes after `before`
      // fails assertUnchanged ahead of the first delivery.
      for await (const _record of streamAgentLogFile(path)) { /* bounded validation */ }
    }
    const sourceHash = sha(JSON.stringify({ format: "normalized-agentlog-snapshot/1", manifest }));
    const assertUnchanged = async () => {
      if (JSON.stringify(await files(root)) !== JSON.stringify(names)) throw new Error("source_changed");
      for (const path of paths) if (await fingerprint(path) !== fingerprints.get(path)) throw new Error("source_changed");
    };
    async function* records(): AsyncGenerator<SourceRecord> {
      const revisions = new Revisions();
      for (const path of paths) {
        for await (const { line, episode } of streamAgentLogFile(path)) {
          if (!episode) continue;
          const input = episode.input;
          const o = input.origin;
          const native = input.source_revision!;
          const body = input.payload === undefined ? undefined : Buffer.from(input.payload);
          const base = RpcRememberParams.parse({
            episode: { schema: input.schema, time: input.time, content: input.content, origin: o, properties: input.properties },
            source_revision: native, expected_previous_revision_key: null,
            ...(body === undefined ? {} : { payload_hash: sha(body) }),
          });
          const { params } = revisions.admit(base, false, `${path}:${line}`);
          yield { params, context: { file: basename(path), line, native_source_revision: native },
            ...(body === undefined ? {} : { payload: { bytes_b64: body.toString("base64"), media_type: input.payload_media_type! } }) };
        }
      }
    }
    return { sourceHash, records: records(), assertUnchanged };
  });
  logEvent("info", "source_scope", { format: "normalized-agentlog", snapshot: "complete", tail: "incomplete", rotation: "incomplete" });
}
