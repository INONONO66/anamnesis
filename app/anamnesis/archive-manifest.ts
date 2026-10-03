import { createHash } from "node:crypto";
import type { AuthoritySnapshot } from "@anamnesis/core";
import { constants, type BigIntStats } from "node:fs";
import { lstat, open, opendir, realpath } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";

/** Read-only directory archive admission, NOT Neo4j dump verification or restore.
 * Callers must hold exclusive custody of the owned archive and its parent for
 * the entire admission/use interval. No portable Node openat/snapshot primitive
 * exists here: inode/change checks detect changes, but are not a writer fence.
 * Nothing returned authorizes activation, proves fsync/publication ordering,
 * authenticates the producer, or verifies the contents of the opaque dump.
 */
/** @public consumed by the .mjs harnesses through dynamic import */
export const ARCHIVE_LIMITS = Object.freeze({
  manifest_bytes: 8 * 1024 ** 2,
  completion_bytes: 4096,
  dump_bytes: 64 * 1024 ** 3,
  dump_metadata_bytes: 4096,
  config_bytes: 1024 ** 2,
  auth_bytes: 16 * 1024,
  object_bytes: 256 * 1024 ** 2,
  sidecar_bytes: 4096,
  total_member_bytes: 128 * 1024 ** 3,
  objects: 10_000,
  members: 20_004,
  embedding_profiles: 64,
  embedding_coverages: 4096,
  receipt_retention_ms: 3650 * 86400000,
  hash_chunk_bytes: 1024 ** 2,
  json_depth: 16,
});
type ArchiveAdmissionCode = "invalid_manifest" | "invalid_completion" | "completion_mismatch" |
  "incompatible_archive" | "archive_layout" | "archive_limit" | "member_mismatch" |
  "invalid_sidecar" | "invalid_dump_metadata" | "archive_changed";
class ArchiveAdmissionError extends Error {
  readonly code: ArchiveAdmissionCode;
  constructor(code: ArchiveAdmissionCode, detail: string, options?: ErrorOptions) {
    super(`${code}: ${detail}`, options); this.name = "ArchiveAdmissionError"; this.code = code;
  }
}
type Role = "database_dump" | "dump_metadata" | "config" | "auth" | "object_data" | "object_sidecar";
interface ArchiveMember { path: string; role: Role; bytes: number; sha256: string }
interface ArchiveObject { hash: string; size: number; media_type: string }
/** Cutoff authority evidence. This is deliberately separate from the opaque
 * dump: a dump without this set cannot establish what was backed up. */
export type { AuthoritySnapshot };
type AuthoritySnapshotRefusal = "authority_members_missing" | "authority_generations_missing" |
  "authority_coverage_missing" | "authority_links_missing" | "authority_invalidation_missing" | "authority_sources_missing";
class AuthoritySnapshotError extends Error {
  readonly code: AuthoritySnapshotRefusal;
  constructor(code: AuthoritySnapshotRefusal, detail: string) { super(`${code}: ${detail}`); this.name = "AuthoritySnapshotError"; this.code = code; }
}
function authoritySorted(values: string[], code: AuthoritySnapshotRefusal): void {
  if (!values.every((v, i) => i === 0 || values[i - 1]! < v)) throw new AuthoritySnapshotError(code, "identities must be sorted and unique");
}
/** Validate adapter evidence before any offline dump is requested. Missing
 * Store APIs are a refusal, never an empty/guessed authority set. */
export function verifyAuthoritySnapshot(value: unknown): AuthoritySnapshot {
  if (!isRecord(value)) throw new AuthoritySnapshotError("authority_members_missing", "snapshot is absent");
  const expectedKeys = ["members", "retained_generations", "coverage", "physical_links", "invalidation_evidence", "source_hashes"];
  if (Object.keys(value).length !== expectedKeys.length || expectedKeys.some(k => !Object.hasOwn(value, k))) throw new AuthoritySnapshotError("authority_members_missing", "snapshot has missing or unknown fields");
  const members = value.members; if (!Array.isArray(members) || members.length === 0 || !members.every(x => typeof x === "string")) throw new AuthoritySnapshotError("authority_members_missing", "Neo4j member enumeration was not supplied");
  authoritySorted(members as string[], "authority_members_missing");
  const generations = value.retained_generations; if (!Array.isArray(generations) || !generations.every((x, i) => integer(x) && (i === 0 || generations[i - 1] < x))) throw new AuthoritySnapshotError("authority_generations_missing", "retained generation coverage is absent or unsorted");
  const coverage = value.coverage;
  if (!isRecord(coverage) || !["ingest_seq", "structure_revision", "policy_revision"].every(k => integer(coverage[k]))) throw new AuthoritySnapshotError("authority_coverage_missing", "cutoff coverage is absent");
  const links = value.physical_links; if (!Array.isArray(links) || !links.every(x => isRecord(x) && typeof x.id === "string" && typeof x.from === "string" && typeof x.to === "string" && (x.role === "DERIVED_FROM" || x.role === "ConductingArc"))) throw new AuthoritySnapshotError("authority_links_missing", "physical DERIVED_FROM/ConductingArc evidence is absent");
  const invalidation = value.invalidation_evidence; if (!Array.isArray(invalidation) || !invalidation.every(x => isRecord(x) && typeof x.id === "string" && text(x.source_hash, HEX) && text(x.outcome_hash, HEX))) throw new AuthoritySnapshotError("authority_invalidation_missing", "invalidation evidence is absent or unhashed");
  const sources = value.source_hashes; if (!Array.isArray(sources) || !sources.every(x => text(x, HEX))) throw new AuthoritySnapshotError("authority_sources_missing", "source hashes are absent");
  authoritySorted(sources as string[], "authority_sources_missing");
  return { members: members as string[], retained_generations: generations as number[], coverage: coverage as AuthoritySnapshot["coverage"], physical_links: links as AuthoritySnapshot["physical_links"], invalidation_evidence: invalidation as AuthoritySnapshot["invalidation_evidence"], source_hashes: sources as string[] };
}
interface ArchiveEmbeddingProfile { embedding_profile_id: string; embedding_model_id: string; vector_index_id: string }
interface ArchiveEmbeddingCoverage {
  embedding_model_id: string; stream: "episode" | "extraction"; generation: number;
  covered_ingest_seq: number; health: "HEALTHY" | "BLOCKED"; resolved_no_vector_count: number; omission_digest: string;
}
export interface ArchiveManifest {
  format: "anamnesis.archive/1";
  operation_id: string;
  cutoff: { ingest_seq: number; structure_revision: number; policy_revision: number };
  compatibility: { schema_version: string; neo4j_version: string; neo4j_image_digest: string; episode_digest_version_ceiling: 1 | 2 };
  configuration: { config_sha256: string; receipt_retention_ms: number; prior_version: string; calibration_version: string; dynamics_version: string };
  models: {
    active_embedding_profile_id: string | null;
    embedding_profiles: ArchiveEmbeddingProfile[];
    embedding_coverages: ArchiveEmbeddingCoverage[];
    extraction: { generation: number; fact_language_policy: string; grouping_version: string; judge_profile_id: string } | null;
  };
  objects: ArchiveObject[];
  members: ArchiveMember[];
  authority?: AuthoritySnapshot;
}
interface ArchiveCompletion {
  format: "anamnesis.archive-complete/1"; operation_id: string; manifest_sha256: string; manifest_bytes: number;
}
/** Explicit supported exact patches/images, never inferred from the archive.
 * Model/config identities are pinned metadata, not model availability or
 * qualification. A future staging verifier must validate those contracts.
 */
export interface ArchiveCompatibility {
  schema_versions: readonly string[]; neo4j_versions: readonly string[];
  neo4j_image_digests: readonly string[]; episode_digest_version_ceiling: 1 | 2;
}
export interface AdmittedArchive {
  status: "admitted"; manifest: ArchiveManifest; manifest_sha256: string;
  verified_members: number; verified_bytes: number;
}
const HEX = /^[0-9a-f]{64}$/;
const UUID7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const VERSION = /^[a-zA-Z0-9][a-zA-Z0-9._/-]{0,127}$/;
const NEO4J = /^5\.26\.(0|[1-9][0-9]{0,5})$/;
const IMAGE = /^sha256:[0-9a-f]{64}$/;
const MEDIA = /^[a-zA-Z0-9!#$&^_.+-]+\/[a-zA-Z0-9!#$&^_.+-]+(?:;[\x20-\x7e]+)?$/;
const FIXED = new Map<string, Role>([
  ["config.jsonc", "config"], ["database/neo4j.dump", "database_dump"],
  ["database/neo4j.dump.metadata.json", "dump_metadata"], ["neo4j.auth", "auth"],
]);
const memberLimit: Record<Role, number> = {
  database_dump: ARCHIVE_LIMITS.dump_bytes, dump_metadata: ARCHIVE_LIMITS.dump_metadata_bytes,
  config: ARCHIVE_LIMITS.config_bytes, auth: ARCHIVE_LIMITS.auth_bytes,
  object_data: ARCHIVE_LIMITS.object_bytes, object_sidecar: ARCHIVE_LIMITS.sidecar_bytes,
};
function need(condition: unknown, code: ArchiveAdmissionCode, detail: string): asserts condition {
  if (!condition) throw new ArchiveAdmissionError(code, detail);
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function record(value: unknown, keys: string[], code: ArchiveAdmissionCode): Record<string, unknown> {
  need(isRecord(value), code, "expected object");
  need(Object.keys(value).length === keys.length && keys.every(k => Object.hasOwn(value, k)), code, "unexpected or missing field");
  return value;
}
function safeInteger(value: unknown): value is number { return Number.isSafeInteger(value); }
function integer(value: unknown, max = Number.MAX_SAFE_INTEGER, min = 0): value is number {
  return safeInteger(value) && value >= min && value <= max;
}
function oneOf<T>(values: readonly T[], value: unknown): value is T { return (values as readonly unknown[]).includes(value); }
const ROLES: readonly Role[] = ["database_dump", "dump_metadata", "config", "auth", "object_data", "object_sidecar"];
function text(value: unknown, pattern: RegExp, max = 256): value is string {
  return typeof value === "string" && value.length <= max && pattern.test(value);
}
function array(value: unknown, max: number, code: ArchiveAdmissionCode): unknown[] {
  need(Array.isArray(value) && value.length <= max, code, "array exceeds admission limit or is absent"); return value;
}
export function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (isRecord(value)) return `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${canonical(value[k])}`).join(",")}}`;
  return JSON.stringify(value);
}
export function sha256(bytes: string | Uint8Array): string { return createHash("sha256").update(bytes).digest("hex"); }
/** Index of the quote closing the string opened at `open`, honouring backslash escapes; past the end when
 * unterminated (charAt answers "" there, so no separate bound is needed). */
function stringEnd(raw: string, open: number): number {
  for (let i = open + 1; ; i++) {
    const c = raw.charAt(i);
    if (c === "" || c === '"') return i;
    if (c === "\\") i++;
  }
}
/** A string is an object key when the next non-space character after it is a colon. */
function isKey(raw: string, end: number): boolean {
  let next = end + 1;
  while (/\s/.test(raw.charAt(next))) next++;
  return raw.charAt(next) === ":";
}
/** Rejects duplicate object keys and over-deep nesting before JSON.parse, which would silently keep the last key.
 * Arrays open a key scope too: a key inside one is malformed JSON, which JSON.parse then reports. */
function scanJson(raw: string, code: ArchiveAdmissionCode): void {
  const scopes: Set<string>[] = [];
  for (let i = 0; i < raw.length; i++) {
    const c = raw[i];
    if (c === '"') {
      const start = i; i = stringEnd(raw, i);
      if (isKey(raw, i)) {
        const key = JSON.parse(raw.slice(start, i + 1)) as string, keys = scopes.at(-1);
        need(keys && !keys.has(key), code, "duplicate JSON key"); keys.add(key);
      }
    } else if (c === "{" || c === "[") {
      scopes.push(new Set()); need(scopes.length <= ARCHIVE_LIMITS.json_depth, code, "JSON depth limit");
    } else if (c === "}" || c === "]") scopes.pop();
  }
}
/** Duplicate keys must reject even for legacy ObjectStore sidecars, whose
 * insertion-ordered JSON is not the canonical manifest byte representation. */
function decode(bytes: string | Uint8Array, max: number, code: ArchiveAdmissionCode, requireCanonical: boolean): unknown {
  need(Buffer.byteLength(bytes) <= max, code, "JSON byte limit");
  let raw: string, value: unknown;
  try {
    raw = typeof bytes === "string" ? bytes : new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
    scanJson(raw, code);
    value = JSON.parse(raw) as unknown;
  } catch (cause) {
    if (cause instanceof ArchiveAdmissionError) throw cause;
    throw new ArchiveAdmissionError(code, "malformed JSON/UTF-8", { cause });
  }
  need(!requireCanonical || canonical(value) === raw, code, "noncanonical JSON bytes");
  return value;
}
function sortedUnique(keys: string[], code: ArchiveAdmissionCode): void {
  need(keys.every((k, i) => i === 0 || keys[i - 1]! < k), code, "identities must be unique and sorted");
}
/** Coverage partition identity whose string order is model, stream, then numeric generation: the
 * generation is zero-padded to the 16 digits of Number.MAX_SAFE_INTEGER, never decimal-string order. */
function partition(model: string, stream: string, generation: number): string {
  return `${model}/${stream}/${String(generation).padStart(16, "0")}`;
}

/** Wire identity: sorted-key compact JSON, no BOM/newline/duplicate keys,
 * canonical JSON numbers/strings, sorted identity arrays; ASCII schema fields.
 * The completion hash binds these exact UTF-8 bytes, not a reserialized input.
 */
function parseCutoff(value: unknown, code: ArchiveAdmissionCode): ArchiveManifest["cutoff"] {
const c0 = record(value, ["ingest_seq", "structure_revision", "policy_revision"], code);
need(integer(c0.ingest_seq) && integer(c0.structure_revision) && integer(c0.policy_revision), code, "invalid cutoff counter");
return { ingest_seq: c0.ingest_seq, structure_revision: c0.structure_revision, policy_revision: c0.policy_revision };
}
function parseCompatibility(value: unknown, code: ArchiveAdmissionCode): ArchiveManifest["compatibility"] {
const k = record(value, ["schema_version", "neo4j_version", "neo4j_image_digest", "episode_digest_version_ceiling"], code);
need(text(k.schema_version, /^anamnesis\.storage\/[1-9][0-9]{0,5}$/) && text(k.neo4j_version, NEO4J) &&
  text(k.neo4j_image_digest, IMAGE) && (k.episode_digest_version_ceiling === 1 || k.episode_digest_version_ceiling === 2), code, "invalid compatibility contract");
return { schema_version: k.schema_version, neo4j_version: k.neo4j_version, neo4j_image_digest: k.neo4j_image_digest, episode_digest_version_ceiling: k.episode_digest_version_ceiling };
}
function parseConfiguration(value: unknown, code: ArchiveAdmissionCode): ArchiveManifest["configuration"] {
const g = record(value, ["config_sha256", "receipt_retention_ms", "prior_version", "calibration_version", "dynamics_version"], code);
need(text(g.config_sha256, HEX) && integer(g.receipt_retention_ms, ARCHIVE_LIMITS.receipt_retention_ms, 1) &&
  text(g.prior_version, VERSION) && text(g.calibration_version, VERSION) && text(g.dynamics_version, VERSION), code, "invalid config pins");
return { config_sha256: g.config_sha256, receipt_retention_ms: g.receipt_retention_ms, prior_version: g.prior_version, calibration_version: g.calibration_version, dynamics_version: g.dynamics_version };
}
/** Embedding profiles in fingerprint order; the active profile, when named, must be one of them. */
function parseProfiles(list: unknown, active: unknown, code: ArchiveAdmissionCode): { profiles: ArchiveEmbeddingProfile[]; activeId: string | null } {
  const profiles = array(list, ARCHIVE_LIMITS.embedding_profiles, code).map((value): ArchiveEmbeddingProfile => {
  const p = record(value, ["embedding_profile_id", "embedding_model_id", "vector_index_id"], code);
  need(text(p.embedding_profile_id, HEX) && text(p.embedding_model_id, HEX) && text(p.vector_index_id, HEX), code, "invalid embedding fingerprint");
  need(p.embedding_profile_id === sha256(canonical({ embedding_model_id: p.embedding_model_id, vector_index_id: p.vector_index_id })), code, "profile fingerprint mismatch");
  return { embedding_profile_id: p.embedding_profile_id, embedding_model_id: p.embedding_model_id, vector_index_id: p.vector_index_id };
  });
  sortedUnique(profiles.map(p => p.embedding_profile_id), code);
  need(active === null || profiles.some(p => p.embedding_profile_id === active), code, "unknown active profile");
  return { profiles, activeId: active as string | null };
}
function parseExtraction(value: unknown, code: ArchiveAdmissionCode): ArchiveManifest["models"]["extraction"] {
  if (value === null) return null;
  const e = record(value, ["generation", "fact_language_policy", "grouping_version", "judge_profile_id"], code);
  need(integer(e.generation, Number.MAX_SAFE_INTEGER, 1) && text(e.fact_language_policy, VERSION) && text(e.grouping_version, VERSION) && text(e.judge_profile_id, HEX), code, "invalid extraction pins");
  return { generation: e.generation, fact_language_policy: e.fact_language_policy, grouping_version: e.grouping_version, judge_profile_id: e.judge_profile_id };
}
/** @public consumed by the .mjs harnesses through dynamic import */
export function parseArchiveManifest(bytes: string | Uint8Array): ArchiveManifest {
  const code = "invalid_manifest";
  const decoded = decode(bytes, ARCHIVE_LIMITS.manifest_bytes, code, true);
  need(isRecord(decoded), code, "expected object");
  const m = record(decoded,
    Object.hasOwn(decoded, "authority") ? ["format", "operation_id", "cutoff", "compatibility", "configuration", "models", "objects", "members", "authority"] : ["format", "operation_id", "cutoff", "compatibility", "configuration", "models", "objects", "members"], code);
  need(m.format === "anamnesis.archive/1" && text(m.operation_id, UUID7), code, "unsupported format or operation identity");
  const cutoff = parseCutoff(m.cutoff, code);
  const compatibility = parseCompatibility(m.compatibility, code);
  const config = parseConfiguration(m.configuration, code);
  const models = record(m.models, ["active_embedding_profile_id", "embedding_profiles", "embedding_coverages", "extraction"], code);
  const { profiles, activeId } = parseProfiles(models.embedding_profiles, models.active_embedding_profile_id, code);
  const extraction = parseExtraction(models.extraction, code);
  const extractionGeneration = extraction?.generation ?? null;
  const modelIds = new Set(profiles.map(p => p.embedding_model_id));
  const coverages = array(models.embedding_coverages, ARCHIVE_LIMITS.embedding_coverages, code).map((value): ArchiveEmbeddingCoverage => {
    const c = record(value, ["embedding_model_id", "stream", "generation", "covered_ingest_seq", "health", "resolved_no_vector_count", "omission_digest"], code);
    need(text(c.embedding_model_id, HEX) && modelIds.has(c.embedding_model_id) && (c.stream === "episode" || c.stream === "extraction") &&
      integer(c.generation, Number.MAX_SAFE_INTEGER, c.stream === "episode" ? 0 : 1) && (c.stream !== "episode" || c.generation === 0) &&
      integer(c.covered_ingest_seq, cutoff.ingest_seq) && (c.health === "HEALTHY" || c.health === "BLOCKED") &&
      integer(c.resolved_no_vector_count) && text(c.omission_digest, HEX), code, "invalid model coverage");
    return { embedding_model_id: c.embedding_model_id, stream: c.stream, generation: c.generation, covered_ingest_seq: c.covered_ingest_seq, health: c.health, resolved_no_vector_count: c.resolved_no_vector_count, omission_digest: c.omission_digest };
  });
  const partitions = coverages.map(c => partition(c.embedding_model_id, c.stream, c.generation));
  sortedUnique(partitions, code);
  const coverageKeys = new Set(partitions);
  for (const id of modelIds) need(coverageKeys.has(partition(id, "episode", 0)), code, "missing model episode coverage");
  const active = profiles.find(p => p.embedding_profile_id === activeId);
  if (active && extractionGeneration !== null) need(coverageKeys.has(partition(active.embedding_model_id, "extraction", extractionGeneration)), code, "missing active extraction coverage");
  const expected = new Map(FIXED), objects = new Map<string, ArchiveObject>();
  for (const value of array(m.objects, ARCHIVE_LIMITS.objects, code)) {
    const obj = record(value, ["hash", "size", "media_type"], code);
    need(text(obj.hash, HEX) && integer(obj.size, ARCHIVE_LIMITS.object_bytes) && text(obj.media_type, MEDIA, 255), code, "invalid object identity");
    const path = `objects/${obj.hash.slice(0, 2)}/${obj.hash}`;
    need(!objects.has(obj.hash), code, "duplicate object");
    objects.set(obj.hash, { hash: obj.hash, size: obj.size, media_type: obj.media_type }); expected.set(path, "object_data"); expected.set(path + ".json", "object_sidecar");
  }
  sortedUnique([...objects.keys()], code);
  const members = array(m.members, ARCHIVE_LIMITS.members, code).map((value): ArchiveMember => {
    const member = record(value, ["path", "role", "bytes", "sha256"], code);
    need(typeof member.path === "string", code, "member path must be a string");
    need(oneOf(ROLES, member.role), code, "unknown member role");
    need(expected.get(member.path) === member.role, code, "noncanonical or wrong-role member path");
    need(integer(member.bytes, memberLimit[member.role], member.role === "object_data" ? 0 : 1) && text(member.sha256, HEX), code, "invalid member size/hash");
    if (member.role === "object_data") {
      const hash = member.path.slice(-64), obj = objects.get(hash)!;
      need(member.sha256 === hash && member.bytes === obj.size, code, "object naming/data disagreement");
    }
    if (member.role === "config") need(member.sha256 === config.config_sha256, code, "config fingerprint mismatch");
    return { path: member.path, role: member.role, bytes: member.bytes, sha256: member.sha256 };
  });
  sortedUnique(members.map(member => member.path), code);
  need(members.length === expected.size && members.reduce((n, member) => n + member.bytes, 0) <= ARCHIVE_LIMITS.total_member_bytes, code, "missing members or total byte limit");
  const manifest: ArchiveManifest = {
    format: "anamnesis.archive/1", operation_id: m.operation_id, cutoff, compatibility, configuration: config,
    models: { active_embedding_profile_id: activeId, embedding_profiles: profiles, embedding_coverages: coverages, extraction },
    objects: [...objects.values()], members,
  };
  if (Object.hasOwn(m, "authority")) manifest.authority = verifyAuthoritySnapshot(m.authority);
  return manifest;
}
/** @public consumed by the .mjs harnesses through dynamic import */
export function parseArchiveCompletion(bytes: string | Uint8Array): ArchiveCompletion {
  const code = "invalid_completion", marker = record(decode(bytes, ARCHIVE_LIMITS.completion_bytes, code, true), ["format", "operation_id", "manifest_sha256", "manifest_bytes"], code);
  need(marker.format === "anamnesis.archive-complete/1" && text(marker.operation_id, UUID7) && text(marker.manifest_sha256, HEX) && integer(marker.manifest_bytes, ARCHIVE_LIMITS.manifest_bytes, 1), code, "invalid completion contract");
  return { format: "anamnesis.archive-complete/1", operation_id: marker.operation_id, manifest_sha256: marker.manifest_sha256, manifest_bytes: marker.manifest_bytes };
}
function checkCompatibility(m: ArchiveManifest, accepted: ArchiveCompatibility): void {
  const code = "incompatible_archive";
  const a = record(accepted, ["schema_versions", "neo4j_versions", "neo4j_image_digests", "episode_digest_version_ceiling"], code);
  for (const [field, pattern] of [["schema_versions", /^anamnesis\.storage\/[1-9][0-9]{0,5}$/], ["neo4j_versions", NEO4J], ["neo4j_image_digests", IMAGE]] as const) {
    const entries = array(a[field], 64, code); need(entries.length > 0 && entries.every(v => text(v, pattern)) && new Set(entries).size === entries.length, code, "invalid explicit compatibility allowlist");
  }
  need(a.episode_digest_version_ceiling === 1 || a.episode_digest_version_ceiling === 2, code, "invalid supported digest ceiling");
  const c = m.compatibility;
  need(accepted.schema_versions.includes(c.schema_version) && accepted.neo4j_versions.includes(c.neo4j_version) && accepted.neo4j_image_digests.includes(c.neo4j_image_digest) && c.episode_digest_version_ceiling <= accepted.episode_digest_version_ceiling, code, "unsupported archive contract");
}
/** @public consumed by the .mjs harnesses through dynamic import */
export function sameStat(a: BigIntStats, b: BigIntStats): boolean {
  return a.dev === b.dev && a.ino === b.ino && a.mode === b.mode && a.uid === b.uid && a.gid === b.gid &&
    a.nlink === b.nlink && a.size === b.size && a.mtimeNs === b.mtimeNs && a.ctimeNs === b.ctimeNs;
}
async function directory(path: string): Promise<BigIntStats> {
  const info = await lstat(path, { bigint: true });
  need(info.isDirectory() && info.uid === BigInt(process.getuid!()) && (info.mode & 0o022n) === 0n, "archive_layout", "archive directories must be owned, non-writable by others, and not links");
  return info;
}
/** Members returned whole for parsing; every other member (and the marker and manifest, which have no
 * declared member) is only hashed, so a dump is never buffered. */
const COLLECTED: readonly Role[] = ["object_sidecar", "dump_metadata"];
/** @public consumed by the .mjs harnesses through dynamic import */
export async function readMember(path: string, max: number, expected?: ArchiveMember): Promise<{ hash: string; bytes: number; content: Buffer; stat: BigIntStats }> {
  need(await realpath(dirname(path)) === dirname(path), "archive_layout", "linked parent directory");
  const before = await lstat(path, { bigint: true });
  need(before.isFile() && before.nlink === 1n, "archive_layout", "member must be a single-link regular file");
  need(before.size <= BigInt(max), "archive_limit", "file exceeds admission byte limit");
  if (expected) need(before.size === BigInt(expected.bytes), "member_mismatch", "member length mismatch");
  const collect = !expected || COLLECTED.includes(expected.role);
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    need(sameStat(before, await file.stat({ bigint: true })), "archive_changed", "file replaced before open");
    const digest = createHash("sha256"), chunks: Buffer[] = [], buffer = Buffer.allocUnsafe(ARCHIVE_LIMITS.hash_chunk_bytes);
    let bytes = 0;
    while (true) {
      const { bytesRead } = await file.read(buffer);
      if (bytesRead === 0) break;
      bytes += bytesRead; need(bytes <= max, "archive_limit", "file grew beyond admission limit");
      const chunk = buffer.subarray(0, bytesRead); digest.update(chunk); if (collect) chunks.push(Buffer.from(chunk));
    }
    // Timestamps are coarse: a same-size rewrite inside one tick leaves the stat unchanged, so the byte count is checked too.
    need(BigInt(bytes) === before.size && sameStat(before, await file.stat({ bigint: true })) && sameStat(before, await lstat(path, { bigint: true })), "archive_changed", "member changed during read");
    const hash = digest.digest("hex");
    if (expected) need(hash === expected.sha256, "member_mismatch", "member SHA-256 mismatch");
    return { hash, bytes, content: Buffer.concat(chunks), stat: before };
  } finally { await file.close(); }
}
async function inventory(root: string, manifest: ArchiveManifest, stamps: Map<string, BigIntStats>): Promise<void> {
  const files = new Set(["manifest.json", "backup.complete", ...manifest.members.map(m => m.path)]), dirs = new Set<string>();
  for (const file of files) {
    let parent = dirname(file);
    while (parent !== ".") { dirs.add(parent); parent = dirname(parent); }
  }
  // Only the manifest-derived root/database/objects/2-hex directories can be
  // visited. Unknown entries reject immediately; opendir never collects an
  // unbounded directory.
  const pending = [""], found = new Set<string>();
  for (let i = 0; i < pending.length; i++) {
    const relative = pending[i]!, path = join(root, relative), before = await directory(path);
    need(await realpath(path) === path, "archive_layout", "linked directory");
    const entries = await opendir(path);
    for await (const entry of entries) {
      const name = relative ? `${relative}/${entry.name}` : entry.name;
      need(!found.has(name) && (files.has(name) || dirs.has(name)), "archive_layout", "extra archive entry"); found.add(name);
      const stat = await lstat(join(root, name), { bigint: true });
      if (dirs.has(name)) { need(stat.isDirectory(), "archive_layout", "linked/non-directory parent"); pending.push(name); }
      else need(stat.isFile() && stat.nlink === 1n, "archive_layout", "non-regular, linked or directory member");
    }
    need(sameStat(before, await lstat(path, { bigint: true })), "archive_changed", "directory changed during listing");
    if (stamps.has(path)) need(sameStat(stamps.get(path)!, before), "archive_changed", "directory replaced");
    stamps.set(path, before);
  }
  need(found.size === files.size + dirs.size, "archive_layout", "missing archive member");
}
function checkSidecar(bytes: Buffer, object: ArchiveObject): void {
  const code = "invalid_sidecar", sidecar = record(decode(bytes, ARCHIVE_LIMITS.sidecar_bytes, code, false), ["hash", "size", "mediaType"], code);
  need(sidecar.hash === object.hash && sidecar.size === object.size && sidecar.mediaType === object.media_type, code, "sidecar/data/manifest disagreement");
}
function checkDumpMetadata(bytes: Buffer, manifest: ArchiveManifest): void {
  const code = "invalid_dump_metadata", metadata = record(decode(bytes, ARCHIVE_LIMITS.dump_metadata_bytes, code, true), ["format", "database", "dump_path", "bytes", "sha256", "neo4j_version", "neo4j_image_digest"], code);
  const dump = manifest.members.find(m => m.role === "database_dump")!;
  need(metadata.format === "anamnesis.archive-dump/1" && metadata.database === "neo4j" && metadata.dump_path === dump.path && metadata.bytes === dump.bytes && metadata.sha256 === dump.sha256 && metadata.neo4j_version === manifest.compatibility.neo4j_version && metadata.neo4j_image_digest === manifest.compatibility.neo4j_image_digest, code, "dump metadata/manifest disagreement");
}
export async function preflightArchive(inputRoot: string, accepted: ArchiveCompatibility): Promise<AdmittedArchive> {
  try {
    const original = resolve(inputRoot), rootStat = await directory(original), root = await realpath(original);
    need(sameStat(rootStat, await directory(root)), "archive_changed", "root replaced");
    const stamps = new Map<string, BigIntStats>([[root, rootStat]]);
    // Marker first: a partial archive is never eligible for member hashing.
    const completion = await readMember(join(root, "backup.complete"), ARCHIVE_LIMITS.completion_bytes);
    const marker = parseArchiveCompletion(completion.content);
    const raw = await readMember(join(root, "manifest.json"), ARCHIVE_LIMITS.manifest_bytes);
    const manifest = parseArchiveManifest(raw.content);
    need(marker.manifest_sha256 === raw.hash && marker.manifest_bytes === raw.bytes && marker.operation_id === manifest.operation_id, "completion_mismatch", "marker does not bind manifest bytes and operation");
    checkCompatibility(manifest, accepted);
    stamps.set(join(root, "manifest.json"), raw.stat); stamps.set(join(root, "backup.complete"), completion.stat);
    await inventory(root, manifest, stamps);
    const objects = new Map(manifest.objects.map(o => [o.hash, o]));
    let verifiedBytes = 0;
    for (const member of manifest.members) {
      const result = await readMember(join(root, member.path), memberLimit[member.role], member);
      stamps.set(join(root, member.path), result.stat); verifiedBytes += result.bytes;
      if (member.role === "object_sidecar") checkSidecar(result.content, objects.get(member.path.slice(-69, -5))!);
      if (member.role === "dump_metadata") checkDumpMetadata(result.content, manifest);
    }
    for (const [path, stamp] of stamps) need(sameStat(stamp, await lstat(path, { bigint: true })), "archive_changed", "archive changed before admission completed");
    need(sameStat(rootStat, await lstat(original, { bigint: true })), "archive_changed", "input root changed");
    return { status: "admitted", manifest, manifest_sha256: raw.hash, verified_members: manifest.members.length, verified_bytes: verifiedBytes };
  } catch (cause) {
    if (isRecord(cause) && ["ENOENT", "ENOTDIR", "ELOOP"].includes(String(cause.code))) {
      throw new ArchiveAdmissionError("archive_layout", "missing or linked archive path", { cause });
    }
    throw cause;
  }
}
