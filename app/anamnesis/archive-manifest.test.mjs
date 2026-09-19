import { test } from "node:test";
import assert from "node:assert/strict";
import { link, symlink, mkdir, readFile, rm, writeFile, open } from "node:fs/promises";
import { join } from "node:path";
import { canonical, compatibility, fixture, hash, snapshot } from "./archive-manifest.fixture.mjs";

const api = () => import(process.env.ARCHIVE_MANIFEST_MODULE ?? "./archive-manifest.ts");
async function rejectUnchanged(f, code) {
  const before = await snapshot(f.owner);
  const { preflightArchive } = await api();
  await assert.rejects(preflightArchive(f.root, compatibility), { code });
  assert.deepEqual(await snapshot(f.owner), before);
}

test("missing-parser RED contract: strict machine parser and completion parser exist", async t => {
  const f = await fixture(t), { parseArchiveManifest, parseArchiveCompletion } = await api();
  assert.deepEqual(parseArchiveManifest(await readFile(join(f.root, "manifest.json"))), f.manifest);
  assert.equal(parseArchiveCompletion(await readFile(join(f.root, "backup.complete"))).manifest_sha256, hash(canonical(f.manifest)));
});

test("complete filesystem archive: streams opaque dump, validates all pairs, leaves source unchanged", async t => {
  const f = await fixture(t), before = await snapshot(f.owner), { preflightArchive } = await api();
  const admitted = await preflightArchive(f.root, compatibility);
  assert.equal(admitted.status, "admitted");
  assert.equal(admitted.manifest_sha256, hash(canonical(f.manifest)));
  assert.deepEqual(admitted.manifest, f.manifest);
  assert.equal(admitted.verified_members, 6);
  assert.equal(admitted.verified_bytes, f.manifest.members.reduce((n, m) => n + m.bytes, 0));
  assert.deepEqual(await snapshot(f.owner), before);
});

const malformed = [
  ["unknown top-level field", m => { m.extra = true; }],
  ["unknown nested field", m => { m.cutoff.extra = 1; }],
  ["missing cutoff", m => { delete m.cutoff.policy_revision; }],
  ["fractional cutoff", m => { m.cutoff.ingest_seq = 1.5; }],
  ["negative cutoff", m => { m.cutoff.policy_revision = -1; }],
  ["unsafe cutoff", m => { m.cutoff.ingest_seq = Number.MAX_SAFE_INTEGER + 1; }],
  ["null counter", m => { m.cutoff.ingest_seq = null; }],
  ["mutable image tag", m => { m.compatibility.neo4j_image_digest = "neo4j:5.26-community"; }],
  ["unsupported digest discriminator", m => { m.compatibility.episode_digest_version_ceiling = 3; }],
  ["duplicate member", m => { m.members.push(m.members[0]); }],
  ["missing declared member", m => { m.members.pop(); }],
  ["extra declared member", m => { m.members.push({ path: "spool/x", role: "auth", bytes: 1, sha256: "a".repeat(64) }); }],
  ["wrong member role", m => { m.members[0].role = "auth"; }],
  ["unsorted members", m => { m.members.reverse(); }],
  ["upper-case hash", m => { m.objects[0].hash = m.objects[0].hash.toUpperCase(); }],
  ["duplicate object", m => { m.objects.push(m.objects[0]); }],
  ["data/object hash mismatch", m => { m.members.find(x => x.role === "object_data").sha256 = "e".repeat(64); }],
  ["object size mismatch", m => { m.objects[0].size++; }],
  ["config pin mismatch", m => { m.configuration.config_sha256 = "e".repeat(64); }],
  ["receipt retention unbounded", m => { m.configuration.receipt_retention_ms = Number.MAX_SAFE_INTEGER; }],
  ["profile identity mismatch", m => { m.models.embedding_profiles[0].embedding_profile_id = "e".repeat(64); }],
  ["unknown active profile", m => { m.models.active_embedding_profile_id = "e".repeat(64); }],
  ["duplicate model coverage", m => { m.models.embedding_coverages.push(m.models.embedding_coverages[0]); }],
  ["coverage beyond cutoff", m => { m.models.embedding_coverages[0].covered_ingest_seq = 8; }],
  ["episode partition generation mismatch", m => { m.models.embedding_coverages[0].generation = 1; }],
  ["missing active extraction coverage", m => { m.models.embedding_coverages.pop(); }],
  ["missing pinned judge profile", m => { delete m.models.extraction.judge_profile_id; }],
  ["member byte limit", m => { m.members.find(x => x.role === "database_dump").bytes = 64 * 1024 ** 3 + 1; }],
];
for (const [name, mutate] of malformed) test(`strict manifest rejects ${name}`, async t => {
  const f = await fixture(t); mutate(f.manifest); await f.publish(); await rejectUnchanged(f, "invalid_manifest");
});
test("strict manifest rejects noncanonical operation UUID behind a valid completion marker", async t => {
  const f = await fixture(t), { parseArchiveManifest } = await api();
  f.manifest.operation_id = f.manifest.operation_id.replace("7000", "4000");
  const malformed = canonical(f.manifest);
  assert.throws(() => parseArchiveManifest(malformed), { code: "invalid_manifest" });
  // Keep the completion marker syntactically valid so the manifest parser is
  // exercised before the later marker-to-manifest identity comparison.
  await writeFile(join(f.root, "manifest.json"), malformed);
  await rejectUnchanged(f, "invalid_manifest");
});
for (const path of ["../escape", "/absolute", "objects/../escape", "database//neo4j.dump", "./config.jsonc", "database\\neo4j.dump", "database/%2e%2e/escape", "database/neo4j.dump/", "database/neo4j.dump\u0000", "C:/escape"]) {
  test(`manifest rejects unsafe/noncanonical path ${JSON.stringify(path)}`, async t => {
    const f = await fixture(t); f.manifest.members[0].path = path; await f.publish(); await rejectUnchanged(f, "invalid_manifest");
  });
}
for (const [name, transform] of [
  ["whitespace", raw => raw + "\n"],
  ["duplicate JSON key", raw => raw.replace('"ingest_seq":7', '"ingest_seq":6,"ingest_seq":7')],
  ["noncanonical numeric spelling", raw => raw.replace('"ingest_seq":7', '"ingest_seq":7.0')],
  ["nonfinite number", raw => raw.replace('"ingest_seq":7', '"ingest_seq":1e400')],
  ["excessive nesting", () => "[".repeat(1000) + "0" + "]".repeat(1000)],
  ["invalid UTF-8", () => Buffer.from([0xff])],
]) test(`machine JSON rejects ${name}`, async t => {
  const f = await fixture(t), { parseArchiveManifest } = await api();
  assert.throws(() => parseArchiveManifest(transform(canonical(f.manifest))), { code: "invalid_manifest" });
});

test("tampered member bytes reject even with unchanged size", async t => {
  const f = await fixture(t), path = join(f.root, "database/neo4j.dump"), file = await open(path, "r+");
  try { await file.write(Buffer.from([0x59]), 0, 1, 1024); } finally { await file.close(); }
  await rejectUnchanged(f, "member_mismatch");
});
test("tampered member hash rejects", async t => {
  const f = await fixture(t); f.manifest.members.find(m => m.role === "database_dump").sha256 = "e".repeat(64); await f.publish();
  await rejectUnchanged(f, "member_mismatch");
});
for (const change of [s => { s.hash = "e".repeat(64); }, s => { s.size++; }, s => { s.mediaType = "text/plain"; }, s => { s.extra = true; }]) {
  test("rehashing altered sidecar cannot bypass data/metadata agreement", async t => {
    const f = await fixture(t), path = f.dataPath + ".json", sidecar = JSON.parse(await readFile(join(f.root, path), "utf8"));
    change(sidecar); await f.replace(path, JSON.stringify(sidecar)); await rejectUnchanged(f, "invalid_sidecar");
  });
}
test("duplicate sidecar JSON keys reject", async t => {
  const f = await fixture(t), path = f.dataPath + ".json", raw = await readFile(join(f.root, path), "utf8");
  await f.replace(path, raw.replace('"size":', '"size":0,"size":')); await rejectUnchanged(f, "invalid_sidecar");
});
test("rehashing false dump metadata cannot establish consistency", async t => {
  const f = await fixture(t), path = "database/neo4j.dump.metadata.json", metadata = JSON.parse(await readFile(join(f.root, path), "utf8"));
  metadata.neo4j_version = "5.26.99"; await f.replace(path, canonical(metadata)); await rejectUnchanged(f, "invalid_dump_metadata");
});
for (const field of ["manifest_sha256", "manifest_bytes", "operation_id", "extra"]) test(`completion marker rejects tampered ${field}`, async t => {
  const f = await fixture(t), path = join(f.root, "backup.complete"), marker = JSON.parse(await readFile(path, "utf8"));
  marker[field] = field === "manifest_bytes" ? marker[field] + 1 : field === "operation_id" ? "01993000-0000-7000-8000-000000000002" : "e".repeat(64);
  await writeFile(path, canonical(marker)); await rejectUnchanged(f, field === "extra" ? "invalid_completion" : "completion_mismatch");
});
for (const path of ["backup.complete", "manifest.json", "database/neo4j.dump", "config.jsonc", "neo4j.auth", "object_data", "object_sidecar"]) test(`partial archive missing ${path} rejects`, async t => {
  const f = await fixture(t), target = path === "object_data" ? f.dataPath : path === "object_sidecar" ? f.dataPath + ".json" : path;
  await rm(join(f.root, target)); await rejectUnchanged(f, "archive_layout");
});
for (const path of ["unexpected", "spool/pending", "objects/ff/orphan", "database/extra", "empty-directory/"]) test(`unlisted filesystem member ${path} rejects`, async t => {
  const f = await fixture(t);
  if (path.endsWith("/")) await mkdir(join(f.root, path));
  else { await mkdir(join(f.root, path, ".."), { recursive: true }); await writeFile(join(f.root, path), "extra"); }
  await rejectUnchanged(f, "archive_layout");
});
for (const kind of ["symlink", "hardlink", "directory", "parent-symlink", "root-symlink"]) test(`filesystem rejects ${kind} without changing owned source or escape target`, async t => {
  const f = await fixture(t), target = join(f.owner, "outside"); await writeFile(target, "outside-owned-fixture");
  if (kind === "root-symlink") { const root = join(f.owner, "alias"); await symlink(f.root, root); f.root = root; }
  else if (kind === "parent-symlink") { await rm(join(f.root, "database"), { recursive: true }); await symlink(f.owner, join(f.root, "database")); }
  else { const path = join(f.root, "config.jsonc"); await rm(path); if (kind === "symlink") await symlink(target, path); else if (kind === "hardlink") await link(target, path); else await mkdir(path); }
  await rejectUnchanged(f, "archive_layout");
});
test("incompatible schema, Neo4j patch/image and digest ceiling reject before hashing", async t => {
  const f = await fixture(t), { preflightArchive } = await api(), before = await snapshot(f.owner);
  for (const overrides of [{ schema_versions: ["anamnesis.storage/99"] }, { neo4j_versions: ["5.26.99"] }, { neo4j_image_digests: [`sha256:${"f".repeat(64)}`] }, { episode_digest_version_ceiling: 1 }]) {
    await assert.rejects(preflightArchive(f.root, { ...compatibility, ...overrides }), { code: "incompatible_archive" });
  }
  assert.deepEqual(await snapshot(f.owner), before);
});
test("metadata admission bounds reject oversized files without reading them whole", async t => {
  const f = await fixture(t), { ARCHIVE_LIMITS, parseArchiveManifest } = await api();
  assert.equal(ARCHIVE_LIMITS.hash_chunk_bytes, 1024 * 1024);
  assert.throws(() => parseArchiveManifest(Buffer.alloc(ARCHIVE_LIMITS.manifest_bytes + 1, 0x20)), { code: "invalid_manifest" });
  const file = await open(join(f.root, "backup.complete"), "r+");
  try { await file.truncate(ARCHIVE_LIMITS.completion_bytes + 1); } finally { await file.close(); }
  await rejectUnchanged(f, "archive_limit");
});
test("empty object authority and disabled model streams are explicit, not missing-field fallback", async t => {
  const f = await fixture(t), { preflightArchive } = await api();
  f.manifest.objects = []; f.manifest.members = f.manifest.members.filter(m => !m.role.startsWith("object_"));
  f.manifest.models = { active_embedding_profile_id: null, embedding_profiles: [], embedding_coverages: [], extraction: null };
  await rm(join(f.root, "objects"), { recursive: true }); await f.publish();
  assert.equal((await preflightArchive(f.root, compatibility)).verified_members, 4);
});
