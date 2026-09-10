import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, readdir } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { streamAgentLogFile } from "../../packages/backfill/src/agentlog.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { RpcClient } from "./client.ts";
import { sourceRevisionKey, ingestSnapshot, type SourceRecord } from "./source.ts";

// Finite source metadata, not an episode collection. Larger exports must be
// partitioned intentionally; no eviction can invent a missing predecessor.
const MAX_FILES = 1024;
const MAX_REVISIONS = 100_000;
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
  return [info.dev, info.ino, info.size, info.mtimeNs, info.ctimeNs, info.mode].join(":");
}

/** Immutable normalized export only: no watcher, append, rotation, SQLite or
 * inferred live history. A full byte-hashed manifest is the durable replay
 * anchor; parsing and occurrence mapping are replayed in physical file order. */
export async function ingestAgentLog(root: string, checkpointPath: string, client: RpcClient): Promise<void> {
  await ingestSnapshot(checkpointPath, client, async () => {
    const names = await files(root);
    const paths = names.map(name => join(root, name));
    if (paths.some(path => [checkpointPath, checkpointPath + ".pending.json"].some(cp => resolve(path) === resolve(cp)))) throw new Error("source_checkpoint_path_conflict");
    const manifest: { file: string; sha256: string }[] = [];
    const fingerprints = new Map<string, string>();
    for (const path of paths) {
      const before = await fingerprint(path);
      const hash = createHash("sha256");
      for await (const chunk of createReadStream(path, { highWaterMark: 64 * 1024 })) hash.update(chunk);
      if (await fingerprint(path) !== before) throw new Error("source_changed");
      fingerprints.set(path, before);
      manifest.push({ file: basename(path), sha256: hash.digest("hex") });
      // Reject partial/invalid/oversized exports before any graph delivery.
      for await (const _record of streamAgentLogFile(path)) { /* bounded validation */ }
      if (await fingerprint(path) !== before) throw new Error("source_changed");
    }
    const sourceHash = sha(JSON.stringify({ format: "normalized-agentlog-snapshot/1", manifest }));
    const assertUnchanged = async () => {
      if (JSON.stringify(await files(root)) !== JSON.stringify(names)) throw new Error("source_changed");
      for (const path of paths) if (await fingerprint(path) !== fingerprints.get(path)) throw new Error("source_changed");
    };
    async function* records(): AsyncGenerator<SourceRecord> {
      const heads = new Map<string, { native: string; revision: string; key: string; previous: string | null; signature: string }>();
      const seen = new Set<string>();
      let ordinal = 0;
      for (const path of paths) {
        for await (const { line, episode } of streamAgentLogFile(path)) {
          if (!episode) continue;
          ordinal++;
          const input = episode.input;
          const o = input.origin;
          const origin = JSON.stringify([o.source, o.session, o.actor, o.record]);
          const native = input.source_revision!;
          const body = input.payload === undefined ? undefined : Buffer.from(input.payload);
          const base = RpcRememberParams.parse({
            episode: { schema: input.schema, time: input.time, content: input.content, origin: o, properties: input.properties },
            source_revision: native, expected_previous_revision_key: null,
            ...(body === undefined ? {} : { payload_hash: sha(body) }),
          });
          const signature = sha(JSON.stringify(base));
          const head = heads.get(origin);
          const duplicate = head?.native === native;
          if (duplicate && head.signature !== signature) throw new Error(`source_revision_conflict: ${path}:${line}`);
          const seenKey = JSON.stringify([origin, native]);
          const revision = duplicate ? head.revision : seen.has(seenKey) ? `${native}:occurrence:${ordinal}` : native;
          const params = RpcRememberParams.parse({ ...base, source_revision: revision, expected_previous_revision_key: duplicate ? head.previous : head?.key ?? null });
          if (!duplicate) {
            if (seen.size >= MAX_REVISIONS || heads.size >= MAX_REVISIONS) throw new Error("source_revision_limit");
            seen.add(seenKey);
            const key = sourceRevisionKey(params);
            heads.set(origin, { native, revision, key, previous: params.expected_previous_revision_key, signature });
          }
          yield { params, context: { file: basename(path), line, native_source_revision: native },
            ...(body === undefined ? {} : { payload: { bytes_b64: body.toString("base64"), media_type: input.payload_media_type! } }) };
        }
      }
    }
    return { sourceHash, records: records(), assertUnchanged };
  });
  console.log(JSON.stringify({ event: "source_scope", format: "normalized-agentlog", snapshot: "complete", tail: "incomplete", rotation: "incomplete" }));
}
