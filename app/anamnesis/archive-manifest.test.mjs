import { test } from "node:test";
import assert from "node:assert/strict";
import * as fsp from "node:fs/promises";
import { chmod, link, symlink, mkdir, readFile, realpath, rm, writeFile, open } from "node:fs/promises";
import { basename, join } from "node:path";
import { spyOn } from "bun:test";
import { canonical, compatibility, fixture, hash, snapshot } from "./archive-manifest.fixture.mjs";

const api = () => import(process.env.ARCHIVE_MANIFEST_MODULE ?? "./archive-manifest.ts");
const failure = (code, detail) => ({ name: code.startsWith("authority_") ? "AuthoritySnapshotError" : "ArchiveAdmissionError", code, message: `${code}: ${detail}` });
/** Runs `run` while the `nths` calls of fsp[method] on paths ending with `suffix` answer `patch(real result)`: a
 * writer racing the admission, reproduced deterministically. */
async function perturb(method, suffix, nths, run, patch) {
  const real = fsp[method]; let seen = 0;
  const spy = spyOn(fsp, method).mockImplementation(async (...args) => {
    const result = await real(...args);
    return String(args[0]).endsWith(suffix) && nths.includes(++seen) ? patch(result) : result;
  });
  try { return await run(); } finally { spy.mockRestore(); }
}
const foreign = stat => Object.assign(Object.create(stat), { ino: stat.ino + 1n });
async function rejectUnchanged(f, code, detail, accepted = compatibility) {
  const before = await snapshot(f.owner);
  const { preflightArchive } = await api();
  await assert.rejects(preflightArchive(f.root, accepted), failure(code, detail));
  assert.deepEqual(await snapshot(f.owner), before);
}
const template = await (async () => { const f = await fixture({ after() {} }); await rm(f.owner, { recursive: true }); return canonical(f.manifest); })();
const manifest = () => JSON.parse(template);
const marker = () => ({ format: "anamnesis.archive-complete/1", operation_id: manifest().operation_id, manifest_sha256: hash(template), manifest_bytes: Buffer.byteLength(template) });

test("strict machine parsers return the exact declared manifest and completion marker", async t => {
  const f = await fixture(t), { parseArchiveManifest, parseArchiveCompletion } = await api();
  assert.deepEqual(parseArchiveManifest(await readFile(join(f.root, "manifest.json"))), f.manifest);
  assert.deepEqual(parseArchiveCompletion(await readFile(join(f.root, "backup.complete"))), marker());
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

const HEX0 = "0".repeat(64), HEX1 = "1".repeat(64), MODEL2 = "a".repeat(64), MIB = 1024 ** 2, GIB = 1024 ** 3;
const profile2 = { embedding_model_id: MODEL2, vector_index_id: "c".repeat(64) };
profile2.embedding_profile_id = hash(canonical(profile2));
const coverage = (embedding_model_id, stream, generation) => ({ embedding_model_id, stream, generation, covered_ingest_seq: 0, health: "HEALTHY", resolved_no_vector_count: 0, omission_digest: hash("[]") });
const byPath = (a, b) => a.path < b.path ? -1 : 1;
const twoProfiles = m => {
  m.models.embedding_profiles = [m.models.embedding_profiles[0], profile2].sort((a, b) => a.embedding_profile_id < b.embedding_profile_id ? -1 : 1);
  m.models.embedding_coverages.unshift(coverage(MODEL2, "episode", 0));
};
function addObjects(m, count, size) {
  for (let i = 1; i <= count; i++) {
    const h = i.toString(16).padStart(64, "0");
    m.objects.push({ hash: h, size, media_type: "text/plain" });
    m.members.push({ path: `objects/00/${h}`, role: "object_data", bytes: size, sha256: h }, { path: `objects/00/${h}.json`, role: "object_sidecar", bytes: 1, sha256: HEX1 });
  }
  m.objects.sort((a, b) => a.hash < b.hash ? -1 : 1); m.members.sort(byPath);
}
function fillTotal(m, objects) {
  addObjects(m, objects, 256 * MIB);
  const dump = m.members.find(x => x.role === "database_dump");
  dump.bytes = 128 * GIB - m.members.reduce((n, x) => n + (x === dump ? 0 : x.bytes), 0);
}
const authority = {
  members: { count: 2, sha256: HEX0 }, retained_generations: [0, 1], coverage: { ingest_seq: 0, structure_revision: 0, policy_revision: 0 },
  physical_links: { count: 2, sha256: HEX0 }, invalidation_evidence: { count: 2, sha256: HEX1 }, source_hashes: { count: 2, sha256: HEX1 },
};
const withAuthority = (change = () => {}) => m => { m.authority = structuredClone(authority); change(m.authority); };
const sections = { null: null, string: "x", array: [] };

const malformed = [
  ...Object.entries(sections).map(([kind, value]) => [`${kind} section`, m => { m.cutoff = value; }, "expected object"]),
  ["unknown top-level field", m => { m.extra = true; }, "unexpected or missing field"],
  ["unknown nested field", m => { m.cutoff.extra = 1; }, "unexpected or missing field"],
  ["missing cutoff", m => { delete m.cutoff.policy_revision; }, "unexpected or missing field"],
  ["renamed field keeps the count", m => { delete m.cutoff.ingest_seq; m.cutoff.extra = 7; }, "unexpected or missing field"],
  ["fractional cutoff", m => { m.cutoff.ingest_seq = 1.5; }, "invalid cutoff counter"],
  ["negative cutoff", m => { m.cutoff.policy_revision = -1; }, "invalid cutoff counter"],
  ["unsafe cutoff", m => { m.cutoff.ingest_seq = Number.MAX_SAFE_INTEGER + 1; }, "invalid cutoff counter"],
  ["null counter", m => { m.cutoff.ingest_seq = null; }, "invalid cutoff counter"],
  ["string counter", m => { m.cutoff.ingest_seq = "7"; }, "invalid cutoff counter"],
  ["format version", m => { m.format = "anamnesis.archive/2"; }, "unsupported format or operation identity"],
  ["noncanonical operation UUID", m => { m.operation_id = m.operation_id.replace("7000", "4000"); }, "unsupported format or operation identity"],
  ["operation id prefix", m => { m.operation_id = "x" + m.operation_id; }, "unsupported format or operation identity"],
  ["operation id suffix", m => { m.operation_id += "x"; }, "unsupported format or operation identity"],
  ["schema version prefix", m => { m.compatibility.schema_version = "x" + m.compatibility.schema_version; }, "invalid compatibility contract"],
  ["schema version suffix", m => { m.compatibility.schema_version += "x"; }, "invalid compatibility contract"],
  ["schema version letter", m => { m.compatibility.schema_version = "anamnesis.storage/1a"; }, "invalid compatibility contract"],
  ["neo4j minor", m => { m.compatibility.neo4j_version = "5.27.0"; }, "invalid compatibility contract"],
  ["neo4j version prefix", m => { m.compatibility.neo4j_version = "x5.26.12"; }, "invalid compatibility contract"],
  ["neo4j version suffix", m => { m.compatibility.neo4j_version = "5.26.12x"; }, "invalid compatibility contract"],
  ["mutable image tag", m => { m.compatibility.neo4j_image_digest = "neo4j:5.26-community"; }, "invalid compatibility contract"],
  ["image digest prefix", m => { m.compatibility.neo4j_image_digest = "x" + m.compatibility.neo4j_image_digest; }, "invalid compatibility contract"],
  ["image digest suffix", m => { m.compatibility.neo4j_image_digest += "x"; }, "invalid compatibility contract"],
  ["unsupported digest discriminator", m => { m.compatibility.episode_digest_version_ceiling = 3; }, "invalid compatibility contract"],
  ["digest ceiling zero", m => { m.compatibility.episode_digest_version_ceiling = 0; }, "invalid compatibility contract"],
  ["config hash case", m => { m.configuration.config_sha256 = "E".repeat(64); }, "invalid config pins"],
  ["receipt retention unbounded", m => { m.configuration.receipt_retention_ms = Number.MAX_SAFE_INTEGER; }, "invalid config pins"],
  ["receipt retention zero", m => { m.configuration.receipt_retention_ms = 0; }, "invalid config pins"],
  ["version pin prefix", m => { m.configuration.prior_version = "/prior/1"; }, "invalid config pins"],
  ["version pin suffix", m => { m.configuration.calibration_version = "calibration/1 "; }, "invalid config pins"],
  ["version pin length", m => { m.configuration.dynamics_version = "d".repeat(129); }, "invalid config pins"],
  ["array version pin", m => { m.configuration.prior_version = ["prior/1"]; }, "invalid config pins"],
  ["profile identity mismatch", m => { m.models.embedding_profiles[0].embedding_profile_id = "e".repeat(64); }, "profile fingerprint mismatch"],
  ["profile id case", m => { m.models.embedding_profiles[0].embedding_profile_id = "E".repeat(64); }, "invalid embedding fingerprint"],
  ["model id length", m => { m.models.embedding_profiles[0].embedding_model_id = "a".repeat(65); }, "invalid embedding fingerprint"],
  ["index id length", m => { m.models.embedding_profiles[0].vector_index_id = "a".repeat(63); }, "invalid embedding fingerprint"],
  ["unknown active profile", m => { m.models.active_embedding_profile_id = "e".repeat(64); }, "unknown active profile"],
  ["numeric active profile", m => { m.models.active_embedding_profile_id = 5; }, "unknown active profile"],
  ["unsorted profiles", m => { twoProfiles(m); m.models.embedding_profiles.reverse(); }, "identities must be unique and sorted"],
  ["duplicate profiles", m => { m.models.embedding_profiles.push(m.models.embedding_profiles[0]); }, "identities must be unique and sorted"],
  ["profile list is not an array", m => { m.models.embedding_profiles = "x"; }, "array exceeds admission limit or is absent"],
  ["profile list over limit", m => { m.models.embedding_profiles = Array(65).fill(m.models.embedding_profiles[0]); }, "array exceeds admission limit or is absent"],
  ["missing pinned judge profile", m => { delete m.models.extraction.judge_profile_id; }, "unexpected or missing field"],
  ["extraction generation zero", m => { m.models.extraction.generation = 0; }, "invalid extraction pins"],
  ["extraction language pin", m => { m.models.extraction.fact_language_policy = ""; }, "invalid extraction pins"],
  ["extraction grouping pin", m => { m.models.extraction.grouping_version = "grouping 1"; }, "invalid extraction pins"],
  ["extraction judge pin", m => { m.models.extraction.judge_profile_id = "d".repeat(63); }, "invalid extraction pins"],
  ["coverage model id case", m => { m.models.embedding_coverages[0].embedding_model_id = "B".repeat(64); }, "invalid model coverage"],
  ["coverage model unknown", m => { m.models.embedding_coverages[0].embedding_model_id = MODEL2; }, "invalid model coverage"],
  ["coverage stream", m => { m.models.embedding_coverages[0].stream = "other"; }, "invalid model coverage"],
  ["episode partition generation mismatch", m => { m.models.embedding_coverages[0].generation = 1; }, "invalid model coverage"],
  ["episode generation negative", m => { m.models.embedding_coverages[0].generation = -1; }, "invalid model coverage"],
  ["extraction coverage generation zero", m => { m.models.embedding_coverages[1].generation = 0; }, "invalid model coverage"],
  ["coverage beyond cutoff", m => { m.models.embedding_coverages[0].covered_ingest_seq = 8; }, "invalid model coverage"],
  ["coverage health", m => { m.models.embedding_coverages[0].health = "SICK"; }, "invalid model coverage"],
  ["coverage count negative", m => { m.models.embedding_coverages[0].resolved_no_vector_count = -1; }, "invalid model coverage"],
  ["coverage digest", m => { m.models.embedding_coverages[0].omission_digest = "zz"; }, "invalid model coverage"],
  ["duplicate model coverage", m => { m.models.embedding_coverages.push(m.models.embedding_coverages[0]); }, "identities must be unique and sorted"],
  ["streams unsorted", m => { m.models.embedding_coverages.reverse(); }, "identities must be unique and sorted"],
  ["models unsorted", m => { twoProfiles(m); m.models.embedding_coverages.push(m.models.embedding_coverages.shift()); }, "identities must be unique and sorted"],
  ["generations unsorted", m => { m.models.embedding_coverages.splice(1, 0, coverage("b".repeat(64), "extraction", 2)); }, "identities must be unique and sorted"],
  ["generations in decimal-string order", m => { m.models.embedding_coverages.push(coverage("b".repeat(64), "extraction", 10), coverage("b".repeat(64), "extraction", 9)); }, "identities must be unique and sorted"],
  ["second model without episode coverage", m => { twoProfiles(m); m.models.embedding_coverages.shift(); }, "missing model episode coverage"],
  ["missing active extraction coverage", m => { m.models.embedding_coverages.pop(); }, "missing active extraction coverage"],
  ["upper-case hash", m => { m.objects[0].hash = m.objects[0].hash.toUpperCase(); }, "invalid object identity"],
  ["object hash length", m => { m.objects[0].hash = "a".repeat(65); }, "invalid object identity"],
  ["object size negative", m => { m.objects[0].size = -1; }, "invalid object identity"],
  ["object size over limit", m => { m.objects[0].size = 256 * MIB + 1; }, "invalid object identity"],
  ["media type without subtype", m => { m.objects[0].media_type = "text"; }, "invalid object identity"],
  ["media type prefix", m => { m.objects[0].media_type = " text/plain"; }, "invalid object identity"],
  ["media type suffix", m => { m.objects[0].media_type = "text/plain "; }, "invalid object identity"],
  ["media type parameter byte", m => { m.objects[0].media_type = "text/plain;\x7f"; }, "invalid object identity"],
  ["media type length", m => { m.objects[0].media_type = "a/" + "b".repeat(254); }, "invalid object identity"],
  ["duplicate object", m => { m.objects.push(m.objects[0]); }, "duplicate object"],
  ["objects unsorted", m => { addObjects(m, 1, 0); m.objects.reverse(); }, "identities must be unique and sorted"],
  ["object list is not an array", m => { m.objects = "x"; }, "array exceeds admission limit or is absent"],
  ["duplicate member", m => { m.members.push(m.members[0]); }, "identities must be unique and sorted"],
  ["unsorted members", m => { m.members.reverse(); }, "identities must be unique and sorted"],
  ["missing declared member", m => { m.members.pop(); }, "missing members or total byte limit"],
  ["total member bytes over limit", m => { fillTotal(m, 256); m.members.find(x => x.role === "database_dump").bytes++; }, "missing members or total byte limit"],
  ["extra declared member", m => { m.members.push({ path: "spool/x", role: "auth", bytes: 1, sha256: "a".repeat(64) }); }, "noncanonical or wrong-role member path"],
  ["wrong member role", m => { m.members[0].role = "auth"; }, "noncanonical or wrong-role member path"],
  ["unknown member role", m => { m.members[0].role = "other"; }, "unknown member role"],
  ["numeric member path", m => { m.members[0].path = 5; }, "member path must be a string"],
  ...["../escape", "/absolute", "objects/../escape", "database//neo4j.dump", "./config.jsonc", "database\\neo4j.dump", "database/%2e%2e/escape", "database/neo4j.dump/", "database/neo4j.dump\u0000", "C:/escape"]
    .map(path => [`unsafe/noncanonical path ${JSON.stringify(path)}`, m => { m.members[0].path = path; }, "noncanonical or wrong-role member path"]),
  ["member byte limit", m => { m.members.find(x => x.role === "database_dump").bytes = 64 * GIB + 1; }, "invalid member size/hash"],
  ["member hash case", m => { m.members[0].sha256 = m.members[0].sha256.toUpperCase(); }, "invalid member size/hash"],
  ["empty config member", m => { m.members.find(x => x.role === "config").bytes = 0; }, "invalid member size/hash"],
  ["data/object hash mismatch", m => { m.members.find(x => x.role === "object_data").sha256 = "e".repeat(64); }, "object naming/data disagreement"],
  ["object size mismatch", m => { m.objects[0].size++; }, "object naming/data disagreement"],
  ["object member bytes disagree", m => { m.members.find(x => x.role === "object_data").bytes++; }, "object naming/data disagreement"],
  ["config pin mismatch", m => { m.configuration.config_sha256 = "e".repeat(64); }, "config fingerprint mismatch"],
];
for (const [name, mutate, detail] of malformed) test(`strict manifest rejects ${name}`, async () => {
  const { parseArchiveManifest } = await api(), m = manifest(); mutate(m);
  assert.throws(() => parseArchiveManifest(canonical(m)), failure("invalid_manifest", detail));
});
test("strict manifest rejects a malformed manifest behind a valid completion marker without touching the archive", async t => {
  const f = await fixture(t); f.manifest.operation_id = f.manifest.operation_id.replace("7000", "4000");
  // Keep the completion marker syntactically valid so the manifest parser is
  // exercised before the later marker-to-manifest identity comparison.
  await writeFile(join(f.root, "manifest.json"), canonical(f.manifest));
  await rejectUnchanged(f, "invalid_manifest", "unsupported format or operation identity");
});

const authorityRejects = [
  ...Object.entries(sections).map(([kind, value]) => [`${kind} authority`, m => { m.authority = value; }, "authority_members_missing", "snapshot is absent"]),
  ["unknown field", withAuthority(a => { a.extra = 1; }), "authority_members_missing", "snapshot has missing or unknown fields"],
  ["renamed field", withAuthority(a => { delete a.members; a.extra = 1; }), "authority_members_missing", "snapshot has missing or unknown fields"],
  ["members not a digest", withAuthority(a => { a.members = "x"; }), "authority_members_missing", "digest is absent or invalid"],
  ["members missing", withAuthority(a => { delete a.members; }), "authority_members_missing", "digest is absent or invalid"],
  ["members missing count", withAuthority(a => { delete a.members.count; }), "authority_members_missing", "digest is absent or invalid"],
  ["members zero", withAuthority(a => { a.members.count = 0; }), "authority_members_missing", "digest is absent or invalid"],
  ["members fractional count", withAuthority(a => { a.members.count = 0.5; }), "authority_members_missing", "digest is absent or invalid"],
  ["members SHA length", withAuthority(a => { a.members.sha256 = "a".repeat(65); }), "authority_members_missing", "digest is absent or invalid"],
  ["members SHA nonhex", withAuthority(a => { a.members.sha256 = "z".repeat(64); }), "authority_members_missing", "digest is absent or invalid"],
  ["members digest extra field", withAuthority(a => { a.members.extra = 1; }), "authority_members_missing", "digest is absent or invalid"],
  ["generations not an array", withAuthority(a => { a.retained_generations = "x"; }), "authority_generations_missing", "retained generation coverage is absent or unsorted"],
  ["generations missing", withAuthority(a => { delete a.retained_generations; }), "authority_generations_missing", "retained generation coverage is absent or unsorted"],
  ["generation fractional", withAuthority(a => { a.retained_generations = [0, 0.5]; }), "authority_generations_missing", "retained generation coverage is absent or unsorted"],
  ["generation negative", withAuthority(a => { a.retained_generations = [-1, 0]; }), "authority_generations_missing", "retained generation coverage is absent or unsorted"],
  ["generation string", withAuthority(a => { a.retained_generations = [0, "1"]; }), "authority_generations_missing", "retained generation coverage is absent or unsorted"],
  ["generations unsorted", withAuthority(a => { a.retained_generations = [1, 0]; }), "authority_generations_missing", "retained generation coverage is absent or unsorted"],
  ["generations duplicate", withAuthority(a => { a.retained_generations = [1, 1]; }), "authority_generations_missing", "retained generation coverage is absent or unsorted"],
  ["coverage absent", withAuthority(a => { a.coverage = null; }), "authority_coverage_missing", "cutoff coverage is absent"],
  ["coverage missing", withAuthority(a => { delete a.coverage; }), "authority_coverage_missing", "cutoff coverage is absent"],
  ["coverage extra field", withAuthority(a => { a.coverage.extra = 1; }), "authority_coverage_missing", "cutoff coverage is absent"],
  ["coverage counter missing", withAuthority(a => { delete a.coverage.ingest_seq; }), "authority_coverage_missing", "cutoff coverage is absent"],
  ["coverage counter fractional", withAuthority(a => { a.coverage.structure_revision = 0.5; }), "authority_coverage_missing", "cutoff coverage is absent"],
  ["coverage counter negative", withAuthority(a => { a.coverage.policy_revision = -1; }), "authority_coverage_missing", "cutoff coverage is absent"],
  ["coverage counter string", withAuthority(a => { a.coverage.ingest_seq = "0"; }), "authority_coverage_missing", "cutoff coverage is absent"],
  ["links not a digest", withAuthority(a => { a.physical_links = "x"; }), "authority_links_missing", "digest is absent or invalid"],
  ["links missing", withAuthority(a => { delete a.physical_links; }), "authority_links_missing", "digest is absent or invalid"],
  ["links missing SHA", withAuthority(a => { delete a.physical_links.sha256; }), "authority_links_missing", "digest is absent or invalid"],
  ["links extra field", withAuthority(a => { a.physical_links.extra = 1; }), "authority_links_missing", "digest is absent or invalid"],
  ["invalidation not a digest", withAuthority(a => { a.invalidation_evidence = "x"; }), "authority_invalidation_missing", "digest is absent or invalid"],
  ["invalidation missing", withAuthority(a => { delete a.invalidation_evidence; }), "authority_invalidation_missing", "digest is absent or invalid"],
  ["invalidation negative count", withAuthority(a => { a.invalidation_evidence.count = -1; }), "authority_invalidation_missing", "digest is absent or invalid"],
  ["invalidation SHA nonhex", withAuthority(a => { a.invalidation_evidence.sha256 = "zz"; }), "authority_invalidation_missing", "digest is absent or invalid"],
  ["sources not a digest", withAuthority(a => { a.source_hashes = "x"; }), "authority_sources_missing", "digest is absent or invalid"],
  ["sources missing", withAuthority(a => { delete a.source_hashes; }), "authority_sources_missing", "digest is absent or invalid"],
  ["source SHA case", withAuthority(a => { a.source_hashes.sha256 = "A".repeat(64); }), "authority_sources_missing", "digest is absent or invalid"],
  ["source SHA length", withAuthority(a => { a.source_hashes.sha256 = "a".repeat(65); }), "authority_sources_missing", "digest is absent or invalid"],
  ["source digest extra field", withAuthority(a => { a.source_hashes.extra = 1; }), "authority_sources_missing", "digest is absent or invalid"],
];
for (const [name, mutate, code, detail] of authorityRejects) test(`authority snapshot refuses ${name}`, async () => {
  const { parseArchiveManifest } = await api(), m = manifest(); mutate(m);
  assert.throws(() => parseArchiveManifest(canonical(m)), failure(code, detail));
});

test("authority snapshot requires own snapshot and coverage fields", async () => {
  const { verifyAuthoritySnapshot } = await api();
  const m = manifest(); withAuthority()(m);
  const { source_hashes, ...rest } = m.authority;
  assert.throws(() => verifyAuthoritySnapshot(Object.assign(Object.create({ source_hashes }), rest)),
    { code: "authority_sources_missing" });
  m.authority.coverage = Object.assign(Object.create({ ingest_seq: 0 }), { structure_revision: 0, policy_revision: 0, extra: 0 });
  assert.throws(() => verifyAuthoritySnapshot(m.authority), { code: "authority_coverage_missing" });
});

const accepted = [
  ["two profiles with the first active", twoProfiles],
  ["sixty-four profiles", m => {
    m.models.embedding_profiles = []; m.models.embedding_coverages = [];
    for (let i = 0; i < 64; i++) {
      const p = { embedding_model_id: i.toString(16).padStart(64, "0"), vector_index_id: "c".repeat(64) };
      p.embedding_profile_id = hash(canonical(p)); m.models.embedding_profiles.push(p); m.models.embedding_coverages.push(coverage(p.embedding_model_id, "episode", 0));
    }
    m.models.embedding_profiles.sort((a, b) => a.embedding_profile_id < b.embedding_profile_id ? -1 : 1);
    m.models.active_embedding_profile_id = null;
  }],
  ["active profile null with extraction pinned and no extraction coverage", m => { m.models.active_embedding_profile_id = null; m.models.embedding_coverages.pop(); }],
  ["extraction null with only episode coverage", m => { m.models.extraction = null; m.models.embedding_coverages.pop(); }],
  ["extraction generations in numeric, not decimal-string, order", m => { m.models.embedding_coverages.push(coverage("b".repeat(64), "extraction", 9), coverage("b".repeat(64), "extraction", 10)); }],
  ["empty object and media type at the length limit", m => { addObjects(m, 1, 0); m.objects.find(o => o.size === 0).media_type = "a/" + "b".repeat(253); }],
  ["media type parameter", m => { m.objects[0].media_type = "text/plain;charset=utf-8"; }],
  ["total member bytes at the limit", m => { fillTotal(m, 256); }],
  ["schema version with two digits", m => { m.compatibility.schema_version = "anamnesis.storage/12"; }],
  ["neo4j patch with one digit", m => { m.compatibility.neo4j_version = "5.26.1"; }],
  ["version pins at the length limit", m => { m.configuration.prior_version = "p".repeat(128); }],
  ["authority snapshot", withAuthority()],
  ["authority with single identities", withAuthority(a => { a.members = { count: 1, sha256: HEX0 }; a.retained_generations = [0]; a.source_hashes = { count: 1, sha256: HEX0 }; a.invalidation_evidence = { count: 1, sha256: HEX1 }; })],
  ["authority with empty link, invalidation and source collections", withAuthority(a => { a.physical_links = { count: 0, sha256: HEX0 }; a.invalidation_evidence = { count: 0, sha256: HEX1 }; a.source_hashes = { count: 0, sha256: HEX0 }; })],
];
for (const [name, mutate] of accepted) test(`strict manifest accepts ${name}`, async () => {
  const { parseArchiveManifest } = await api(), m = manifest(); mutate(m);
  assert.deepEqual(parseArchiveManifest(canonical(m)), m);
});

const rawCases = [
  ["whitespace", raw => raw + "\n", "noncanonical JSON bytes"],
  ["duplicate JSON key", raw => raw.replace('"ingest_seq":7', '"ingest_seq":6,"ingest_seq":7'), "duplicate JSON key"],
  ["duplicate top-level JSON key", raw => raw.replace(/^\{/, '{"zz":1,"zz":2,'), "duplicate JSON key"],
  ["duplicate JSON key spelled with an escape", () => '{"\\u0061":1,"a":2}', "duplicate JSON key"],
  ["key repeated after a closed nested object", () => '{"a":{"b":1},"b":2}', "unexpected or missing field"],
  ["key inside an array", () => '{"a":["b":1]}', "malformed JSON/UTF-8"],
  ["unterminated string", () => '{"abc', "malformed JSON/UTF-8"],
  ["unterminated escape", () => '{"abc\\', "malformed JSON/UTF-8"],
  ["seventeen sibling objects", () => `[${Array(17).fill("{}").join(",")}]`, "expected object"],
  ["noncanonical numeric spelling", raw => raw.replace('"ingest_seq":7', '"ingest_seq":7.0'), "noncanonical JSON bytes"],
  ["nonfinite number", raw => raw.replace('"ingest_seq":7', '"ingest_seq":1e400'), "noncanonical JSON bytes"],
  ["byte order mark", raw => Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from(raw)]), "malformed JSON/UTF-8"],
  ["invalid UTF-8", () => Buffer.from([0xff]), "malformed JSON/UTF-8"],
  ["invalid UTF-8 inside a string value", raw => { const bytes = Buffer.from(raw.replace('"prior/1"', '"prior/_"')); bytes[bytes.indexOf('"prior/_"') + 7] = 0xff; return bytes; }, "malformed JSON/UTF-8"],
  ["escaped quote before a colon is not a key", raw => raw.replace('"prior/1"', JSON.stringify('x":y')), "invalid config pins"],
  ["sixteen nesting levels", () => "[".repeat(16) + "0" + "]".repeat(16), "expected object"],
  ["seventeen nesting levels", () => "[".repeat(17) + "0" + "]".repeat(17), "JSON depth limit"],
  ["excessive nesting", () => "[".repeat(1000) + "0" + "]".repeat(1000), "JSON depth limit"],
  ...Object.entries(sections).map(([kind, value]) => [`${kind} document`, () => canonical(value), "expected object"]),
  ["bytes over the manifest limit", () => Buffer.alloc(8 * MIB + 1, 0x20), "JSON byte limit"],
];
for (const [name, transform, detail] of rawCases) test(`machine JSON rejects ${name}`, async () => {
  const { parseArchiveManifest } = await api();
  assert.throws(() => parseArchiveManifest(transform(template)), failure("invalid_manifest", detail));
});
test("malformed JSON keeps the parser failure as cause", async () => {
  const { parseArchiveManifest } = await api();
  assert.throws(() => parseArchiveManifest("{"), e => e.message === "invalid_manifest: malformed JSON/UTF-8" && e.cause instanceof SyntaxError);
});

const completionRejects = [
  ["format", c => { c.format = "anamnesis.archive-complete/2"; }, "invalid completion contract"],
  ["operation id", c => { c.operation_id = c.operation_id.replace("7000", "4000"); }, "invalid completion contract"],
  ["manifest hash case", c => { c.manifest_sha256 = c.manifest_sha256.toUpperCase(); }, "invalid completion contract"],
  ["zero manifest bytes", c => { c.manifest_bytes = 0; }, "invalid completion contract"],
  ["manifest bytes over limit", c => { c.manifest_bytes = 8 * MIB + 1; }, "invalid completion contract"],
  ["fractional manifest bytes", c => { c.manifest_bytes = 1.5; }, "invalid completion contract"],
  ["unknown field", c => { c.extra = 1; }, "unexpected or missing field"],
];
for (const [name, mutate, detail] of completionRejects) test(`completion marker rejects ${name}`, async () => {
  const { parseArchiveCompletion } = await api(), c = marker(); mutate(c);
  assert.throws(() => parseArchiveCompletion(canonical(c)), failure("invalid_completion", detail));
});
test("completion marker rejects noncanonical and oversized bytes", async () => {
  const { parseArchiveCompletion } = await api();
  assert.throws(() => parseArchiveCompletion(canonical(marker()) + "\n"), failure("invalid_completion", "noncanonical JSON bytes"));
  assert.throws(() => parseArchiveCompletion(" ".repeat(4097)), failure("invalid_completion", "JSON byte limit"));
});

test("sameStat compares every identity, permission, link, size and time field", async () => {
  const { sameStat } = await api();
  const stat = { dev: 1n, ino: 2n, mode: 3n, uid: 4n, gid: 5n, nlink: 6n, size: 7n, mtimeNs: 8n, ctimeNs: 9n };
  assert.equal(sameStat(stat, { ...stat }), true);
  for (const field of Object.keys(stat)) assert.equal(sameStat(stat, { ...stat, [field]: 10n }), false, field);
});

test("tampered member bytes reject even with unchanged size", async t => {
  const f = await fixture(t), path = join(f.root, "database/neo4j.dump"), file = await open(path, "r+");
  try { await file.write(Buffer.from([0x59]), 0, 1, 1024); } finally { await file.close(); }
  await rejectUnchanged(f, "member_mismatch", "member SHA-256 mismatch");
});
test("tampered member hash rejects", async t => {
  const f = await fixture(t); f.manifest.members.find(m => m.role === "database_dump").sha256 = "e".repeat(64); await f.publish();
  await rejectUnchanged(f, "member_mismatch", "member SHA-256 mismatch");
});
test("declared member length that disagrees with the file rejects before hashing", async t => {
  const f = await fixture(t); f.manifest.members.find(m => m.role === "database_dump").bytes++; await f.publish();
  await rejectUnchanged(f, "member_mismatch", "member length mismatch");
});
for (const [change, detail] of [[s => { s.hash = "e".repeat(64); }, "sidecar/data/manifest disagreement"], [s => { s.size++; }, "sidecar/data/manifest disagreement"], [s => { s.mediaType = "text/plain"; }, "sidecar/data/manifest disagreement"], [s => { s.extra = true; }, "unexpected or missing field"]]) {
  test(`rehashing altered sidecar cannot bypass data/metadata agreement (${detail})`, async t => {
    const f = await fixture(t), path = f.dataPath + ".json", sidecar = JSON.parse(await readFile(join(f.root, path), "utf8"));
    change(sidecar); await f.replace(path, JSON.stringify(sidecar)); await rejectUnchanged(f, "invalid_sidecar", detail);
  });
}
for (const [name, spelling] of [["compact", '"size":0,"size":'], ["spaced", '"size" : 0, "size" :']]) test(`${name} duplicate sidecar JSON keys reject`, async t => {
  const f = await fixture(t), path = f.dataPath + ".json", raw = await readFile(join(f.root, path), "utf8");
  await f.replace(path, raw.replace('"size":', spelling)); await rejectUnchanged(f, "invalid_sidecar", "duplicate JSON key");
});
test("sidecar exactly at the admission byte limit is read whole and admitted", async t => {
  const f = await fixture(t), path = f.dataPath + ".json", raw = await readFile(join(f.root, path), "utf8"), { preflightArchive } = await api();
  await f.replace(path, raw.padEnd(4096, " "));
  assert.equal((await preflightArchive(f.root, compatibility)).verified_bytes, f.manifest.members.reduce((n, m) => n + m.bytes, 0));
});
const metadataRejects = [
  ...["format", "database", "dump_path", "neo4j_version", "neo4j_image_digest"].map(field => [field, d => { d[field] = "other"; }, "dump metadata/manifest disagreement"]),
  ["bytes", d => { d.bytes++; }, "dump metadata/manifest disagreement"],
  ["sha256", d => { d.sha256 = "e".repeat(64); }, "dump metadata/manifest disagreement"],
  ["unknown field", d => { d.extra = 1; }, "unexpected or missing field"],
];
for (const [name, change, detail] of metadataRejects) test(`rehashing false dump metadata (${name}) cannot establish consistency`, async t => {
  const f = await fixture(t), path = "database/neo4j.dump.metadata.json", metadata = JSON.parse(await readFile(join(f.root, path), "utf8"));
  change(metadata); await f.replace(path, canonical(metadata)); await rejectUnchanged(f, "invalid_dump_metadata", detail);
});
test("noncanonical dump metadata bytes reject", async t => {
  const f = await fixture(t), path = "database/neo4j.dump.metadata.json";
  await f.replace(path, await readFile(join(f.root, path), "utf8") + "\n"); await rejectUnchanged(f, "invalid_dump_metadata", "noncanonical JSON bytes");
});
for (const [field, detail] of [["manifest_sha256", "marker does not bind manifest bytes and operation"], ["manifest_bytes", "marker does not bind manifest bytes and operation"], ["operation_id", "marker does not bind manifest bytes and operation"], ["extra", "unexpected or missing field"]]) test(`completion marker rejects tampered ${field}`, async t => {
  const f = await fixture(t), path = join(f.root, "backup.complete"), marker = JSON.parse(await readFile(path, "utf8"));
  marker[field] = field === "manifest_bytes" ? marker[field] + 1 : field === "operation_id" ? "01993000-0000-7000-8000-000000000002" : "e".repeat(64);
  await writeFile(path, canonical(marker)); await rejectUnchanged(f, field === "extra" ? "invalid_completion" : "completion_mismatch", detail);
});
for (const path of ["backup.complete", "manifest.json", "database/neo4j.dump", "config.jsonc", "neo4j.auth", "object_data", "object_sidecar"]) test(`partial archive missing ${path} rejects`, async t => {
  const f = await fixture(t), target = path === "object_data" ? f.dataPath : path === "object_sidecar" ? f.dataPath + ".json" : path;
  await rm(join(f.root, target)); await rejectUnchanged(f, "archive_layout", path.endsWith("complete") || path.endsWith("manifest.json") ? "missing or linked archive path" : "missing archive member");
});
const layoutCauses = [
  ["absent", f => join(f.owner, "absent"), "ENOENT"],
  ["under a regular file", f => join(f.root, "config.jsonc", "archive"), "ENOTDIR"],
  ["through a symlink loop", async f => { const loop = join(f.owner, "loop"); await symlink(loop, loop); return join(loop, "archive"); }, "ELOOP"],
];
for (const [kind, path, code] of layoutCauses) test(`archive root ${kind} is a layout failure keeping the ${code} cause`, async t => {
  const f = await fixture(t), { preflightArchive } = await api();
  await assert.rejects(preflightArchive(await path(f), compatibility), e => {
    assert.deepEqual({ name: e.name, code: e.code, message: e.message }, failure("archive_layout", "missing or linked archive path"));
    assert.equal(e.cause.code, code); return true;
  });
});
// lstat order on the realpath'd root: 1 input root, 2 realpath'd root, 3 inventory entry, 4 after listing, 5 stamp
// check, 6 input root again; on a listed directory: 1 parent entry, 2 inventory entry, 3 after listing, 4 stamp check;
// on a member: marker/manifest 1 before, 2 after read, 3 directory entry, 4 stamp; others 1 entry, 2 before, 3 after, 4 stamp.
const races = [
  ["root replaced", "lstat", "/archive", [2], "archive_changed", "root replaced"],
  ["root replaced by the time it is listed", "lstat", "/archive", [3, 4], "archive_changed", "directory replaced"],
  ["directory changed during listing", "lstat", "/database", [3], "archive_changed", "directory changed during listing"],
  ["directory changed before admission", "lstat", "/database", [4], "archive_changed", "archive changed before admission completed"],
  ["root changed before admission", "lstat", "/archive", [5], "archive_changed", "archive changed before admission completed"],
  ["input root changed", "lstat", "/archive", [6], "archive_changed", "input root changed"],
  ["marker changed during read", "lstat", "/backup.complete", [2], "archive_changed", "member changed during read"],
  ["member changed before admission", "lstat", "/neo4j.auth", [4], "archive_changed", "archive changed before admission completed"],
  ["directory linked after its entry was checked", "realpath", "/database", [1], "archive_layout", "linked directory", path => `${path}-moved`],
];
for (const [name, method, suffix, nths, code, detail, patch = foreign] of races) test(`racing writer: ${name}`, async t => {
  const f = await fixture(t), { preflightArchive } = await api(), root = await realpath(f.root);
  await perturb(method, suffix, nths, () => assert.rejects(preflightArchive(root, compatibility), failure(code, detail)), patch);
});
const handleRaces = [
  ["file replaced before open", "/backup.complete", h => { const stat = h.stat.bind(h); h.stat = async o => foreign(await stat(o)); }, "archive_changed", "file replaced before open"],
  ["open file changed during read", "/backup.complete", h => { const stat = h.stat.bind(h); let n = 0; h.stat = async o => { const s = await stat(o); return ++n === 2 ? foreign(s) : s; }; }, "archive_changed", "member changed during read"],
  ["sidecar at the byte limit grew during read", "sidecar", h => { const read = h.read.bind(h); h.read = async (...a) => { const r = await read(...a); return r.bytesRead ? { ...r, bytesRead: r.bytesRead + 1 } : r; }; }, "archive_limit", "file grew beyond admission limit"],
  ["same-size rewrite read one byte short while every stat stays equal", "/backup.complete", h => { const read = h.read.bind(h); h.read = async (...a) => { const r = await read(...a); return r.bytesRead ? { ...r, bytesRead: r.bytesRead - 1 } : r; }; }, "archive_changed", "member changed during read"],
];
for (const [name, suffix, wrap, code, detail] of handleRaces) test(`racing writer: ${name}`, async t => {
  const f = await fixture(t), sidecar = f.dataPath + ".json";
  await f.replace(sidecar, (await readFile(join(f.root, sidecar), "utf8")).padEnd(4096, " "));
  await perturb("open", suffix === "sidecar" ? basename(sidecar) : suffix, [1], () => rejectUnchanged(f, code, detail), h => { wrap(h); return h; });
});
test("readMember hashes every member and buffers only the parsed roles", async t => {
  const f = await fixture(t), { ARCHIVE_LIMITS, readMember } = await api(), root = await realpath(f.root);
  const dump = f.manifest.members.find(m => m.role === "database_dump"), sidecar = f.manifest.members.find(m => m.role === "object_sidecar");
  const streamed = await readMember(join(root, dump.path), ARCHIVE_LIMITS.dump_bytes, dump);
  assert.deepEqual([streamed.hash, streamed.bytes, streamed.content.length], [dump.sha256, dump.bytes, 0]);
  const parsed = await readMember(join(root, sidecar.path), ARCHIVE_LIMITS.sidecar_bytes, sidecar);
  assert.deepEqual([parsed.hash, parsed.bytes, parsed.content.toString()], [sidecar.sha256, sidecar.bytes, await readFile(join(root, sidecar.path), "utf8")]);
  const marker = await readMember(join(root, "backup.complete"), ARCHIVE_LIMITS.completion_bytes);
  assert.deepEqual([marker.content.length, marker.stat.size], [marker.bytes, BigInt(marker.bytes)]);
});
test("readMember refuses a linked parent, a directory and a hard-linked file before opening", async t => {
  const f = await fixture(t), { readMember } = await api(), root = await realpath(f.root), alias = join(f.owner, "alias");
  await symlink(root, alias); await link(join(root, "config.jsonc"), join(f.owner, "twin"));
  await assert.rejects(readMember(join(alias, "config.jsonc"), MIB), failure("archive_layout", "linked parent directory"));
  await assert.rejects(readMember(join(root, "database"), MIB), failure("archive_layout", "member must be a single-link regular file"));
  await assert.rejects(readMember(join(root, "config.jsonc"), MIB), failure("archive_layout", "member must be a single-link regular file"));
});
test("a non-object failure during preflight is rethrown untouched", async t => {
  const f = await fixture(t), { preflightArchive } = await api();
  await perturb("lstat", "/archive", [1], () => preflightArchive(f.root, compatibility).then(() => assert.fail("admitted"), e => assert.equal(e, null)), () => { throw null; });
});
test("unreadable archive directory propagates the operating system failure", { skip: process.getuid?.() === 0 && "root reads a mode-0 directory" }, async t => {
  const f = await fixture(t), { preflightArchive } = await api(), locked = join(f.root, "database");
  await chmod(locked, 0);
  try { await assert.rejects(preflightArchive(f.root, compatibility), { code: "EACCES" }); } finally { await chmod(locked, 0o700); }
});
for (const path of ["unexpected", "spool/pending", "objects/ff/orphan", "database/extra", "empty-directory/"]) test(`unlisted filesystem member ${path} rejects`, async t => {
  const f = await fixture(t);
  if (path.endsWith("/")) await mkdir(join(f.root, path));
  else { await mkdir(join(f.root, path, ".."), { recursive: true }); await writeFile(join(f.root, path), "extra"); }
  await rejectUnchanged(f, "archive_layout", "extra archive entry");
});
const linkDetails = {
  symlink: "non-regular, linked or directory member", hardlink: "non-regular, linked or directory member", directory: "non-regular, linked or directory member",
  "parent-symlink": "linked/non-directory parent", "root-symlink": "archive directories must be owned, non-writable by others, and not links",
};
for (const [kind, detail] of Object.entries(linkDetails)) test(`filesystem rejects ${kind} without changing owned source or escape target`, async t => {
  const f = await fixture(t), target = join(f.owner, "outside"); await writeFile(target, "outside-owned-fixture");
  if (kind === "root-symlink") { const root = join(f.owner, "alias"); await symlink(f.root, root); f.root = root; }
  else if (kind === "parent-symlink") { await rm(join(f.root, "database"), { recursive: true }); await symlink(f.owner, join(f.root, "database")); }
  else { const path = join(f.root, "config.jsonc"); await rm(path); if (kind === "symlink") await symlink(target, path); else if (kind === "hardlink") await link(target, path); else await mkdir(path); }
  await rejectUnchanged(f, "archive_layout", detail);
});
const compatibilityRejects = [
  [{ schema_versions: ["anamnesis.storage/99"] }, "unsupported archive contract"],
  [{ neo4j_versions: ["5.26.99"] }, "unsupported archive contract"],
  [{ neo4j_image_digests: [`sha256:${"f".repeat(64)}`] }, "unsupported archive contract"],
  [{ episode_digest_version_ceiling: 1 }, "unsupported archive contract"],
  [{ schema_versions: [] }, "invalid explicit compatibility allowlist"],
  [{ neo4j_versions: ["5.26.12", "5.26.12"] }, "invalid explicit compatibility allowlist"],
  [{ neo4j_versions: ["5.26.12", "5.26"] }, "invalid explicit compatibility allowlist"],
  [{ neo4j_image_digests: ["neo4j:5.26-community"] }, "invalid explicit compatibility allowlist"],
  [{ schema_versions: Array(65).fill("anamnesis.storage/1") }, "array exceeds admission limit or is absent"],
  [{ episode_digest_version_ceiling: 3 }, "invalid supported digest ceiling"],
  [{ extra: true }, "unexpected or missing field"],
];
for (const [overrides, detail] of compatibilityRejects) test(`compatibility ${JSON.stringify(overrides).slice(0, 60)} rejects before hashing`, async t => {
  await rejectUnchanged(await fixture(t), "incompatible_archive", detail, { ...compatibility, ...overrides });
});
test("an archive below the supported digest ceiling is admitted", async t => {
  const f = await fixture(t), { preflightArchive } = await api();
  f.manifest.compatibility.episode_digest_version_ceiling = 1; await f.publish();
  assert.equal((await preflightArchive(f.root, compatibility)).status, "admitted");
});
test("metadata admission bounds reject oversized files without reading them whole", async t => {
  const f = await fixture(t), { ARCHIVE_LIMITS } = await api();
  assert.equal(ARCHIVE_LIMITS.hash_chunk_bytes, MIB);
  const file = await open(join(f.root, "backup.complete"), "r+");
  try { await file.truncate(ARCHIVE_LIMITS.completion_bytes + 1); } finally { await file.close(); }
  await rejectUnchanged(f, "archive_limit", "file exceeds admission byte limit");
});
test("empty object authority and disabled model streams are explicit, not missing-field fallback", async t => {
  const f = await fixture(t), { preflightArchive } = await api();
  f.manifest.objects = []; f.manifest.members = f.manifest.members.filter(m => !m.role.startsWith("object_"));
  f.manifest.models = { active_embedding_profile_id: null, embedding_profiles: [], embedding_coverages: [], extraction: null };
  await rm(join(f.root, "objects"), { recursive: true }); await f.publish();
  assert.equal((await preflightArchive(f.root, compatibility)).verified_members, 4);
});
