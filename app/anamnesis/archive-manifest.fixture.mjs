import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, readdir, lstat, readlink, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import assert from "node:assert/strict";

export const hash = bytes => createHash("sha256").update(bytes).digest("hex");
export function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}
export const compatibility = {
  schema_versions: ["anamnesis.storage/1"], neo4j_versions: ["5.26.12"],
  neo4j_image_digests: [`sha256:${"a".repeat(64)}`], episode_digest_version_ceiling: 2,
};
export async function fixture(t) {
  const owner = await mkdtemp(join(tmpdir(), "g005-archive-manifest-"));
  t.after(async () => { await rm(owner, { recursive: true }); await assert.rejects(lstat(owner), { code: "ENOENT" }); });
  const root = join(owner, "archive");
  await mkdir(root, { mode: 0o700 });
  const payload = Buffer.from("owned payload bytes\n");
  const objectHash = hash(payload), dataPath = `objects/${objectHash.slice(0, 2)}/${objectHash}`;
  // Synthetic opaque bytes deliberately do NOT assert a valid Neo4j dump.
  const dump = Buffer.alloc(3 * 1024 * 1024 + 17, 0x5a);
  const config = Buffer.from('{"fixture":true}\n'), auth = Buffer.from("neo4j/fixture-not-a-live-secret\n");
  const files = new Map([
    ["database/neo4j.dump", dump],
    ["database/neo4j.dump.metadata.json", Buffer.from(canonical({ format: "anamnesis.archive-dump/1", database: "neo4j", dump_path: "database/neo4j.dump", bytes: dump.length, sha256: hash(dump), neo4j_version: "5.26.12", neo4j_image_digest: compatibility.neo4j_image_digests[0] }))],
    ["config.jsonc", config], ["neo4j.auth", auth],
    [dataPath, payload], [`${dataPath}.json`, Buffer.from(JSON.stringify({ hash: objectHash, size: payload.length, mediaType: "application/octet-stream" }))],
  ]);
  const roles = ["database_dump", "dump_metadata", "config", "auth", "object_data", "object_sidecar"];
  const model = "b".repeat(64), index = "c".repeat(64);
  const profile = hash(canonical({ embedding_model_id: model, vector_index_id: index }));
  const manifest = {
    format: "anamnesis.archive/1", operation_id: "01993000-0000-7000-8000-000000000001",
    cutoff: { ingest_seq: 7, structure_revision: 11, policy_revision: 3 },
    compatibility: { schema_version: "anamnesis.storage/1", neo4j_version: "5.26.12", neo4j_image_digest: compatibility.neo4j_image_digests[0], episode_digest_version_ceiling: 2 },
    configuration: { config_sha256: hash(config), receipt_retention_ms: 86400000, prior_version: "prior/1", calibration_version: "calibration/1", dynamics_version: "dynamics/1" },
    models: {
      active_embedding_profile_id: profile,
      embedding_profiles: [{ embedding_profile_id: profile, embedding_model_id: model, vector_index_id: index }],
      embedding_coverages: [{ embedding_model_id: model, stream: "episode", generation: 0, covered_ingest_seq: 7, health: "HEALTHY", resolved_no_vector_count: 0, omission_digest: hash("[]") }, { embedding_model_id: model, stream: "extraction", generation: 1, covered_ingest_seq: 6, health: "BLOCKED", resolved_no_vector_count: 0, omission_digest: hash("[]") }],
      extraction: { generation: 1, fact_language_policy: "source-language/1", grouping_version: "grouping/1", judge_profile_id: "d".repeat(64) },
    },
    objects: [{ hash: objectHash, size: payload.length, media_type: "application/octet-stream" }],
    members: [...files].map(([path, bytes], i) => ({ path, role: roles[i], bytes: bytes.length, sha256: hash(bytes) })).sort((a, b) => a.path < b.path ? -1 : 1),
  };
  for (const [path, bytes] of files) { await mkdir(dirname(join(root, path)), { recursive: true, mode: 0o700 }); await writeFile(join(root, path), bytes, { mode: 0o600 }); }
  async function publish() {
    const bytes = canonical(manifest);
    await writeFile(join(root, "manifest.json"), bytes, { mode: 0o600 });
    await writeFile(join(root, "backup.complete"), canonical({ format: "anamnesis.archive-complete/1", operation_id: manifest.operation_id, manifest_sha256: hash(bytes), manifest_bytes: Buffer.byteLength(bytes) }), { mode: 0o600 });
  }
  async function replace(path, bytes) {
    await writeFile(join(root, path), bytes);
    const member = manifest.members.find(m => m.path === path);
    member.bytes = Buffer.byteLength(bytes); member.sha256 = hash(bytes);
    await publish();
  }
  await publish();
  return { owner, root, manifest, publish, replace, dataPath };
}
export async function snapshot(root) {
  const result = [];
  async function visit(path, relative) {
    const info = await lstat(path);
    const row = { path: relative, mode: info.mode, ino: info.ino, size: info.size, mtime: info.mtimeMs, nlink: info.nlink };
    if (info.isSymbolicLink()) row.target = await readlink(path);
    else if (info.isDirectory()) { for (const name of (await readdir(path)).sort()) await visit(join(path, name), `${relative}/${name}`); }
    else row.sha256 = hash(await readFile(path));
    result.push(row);
  }
  await visit(root, ""); return result;
}
