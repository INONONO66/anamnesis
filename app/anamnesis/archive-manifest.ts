import { createHash } from "node:crypto";
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
export type ArchiveAdmissionCode = "invalid_manifest" | "invalid_completion" | "completion_mismatch" |
  "incompatible_archive" | "archive_layout" | "archive_limit" | "member_mismatch" |
  "invalid_sidecar" | "invalid_dump_metadata" | "archive_changed";
export class ArchiveAdmissionError extends Error {
  readonly code: ArchiveAdmissionCode;
  constructor(code: ArchiveAdmissionCode, detail: string, options?: ErrorOptions) {
    super(`${code}: ${detail}`, options); this.name = "ArchiveAdmissionError"; this.code = code;
  }
}
type Role = "database_dump" | "dump_metadata" | "config" | "auth" | "object_data" | "object_sidecar";
export interface ArchiveMember { path: string; role: Role; bytes: number; sha256: string }
export interface ArchiveObject { hash: string; size: number; media_type: string }
/** Cutoff authority evidence. This is deliberately separate from the opaque
 * dump: a dump without this set cannot establish what was backed up. */
export interface AuthoritySnapshot {
  members: string[];
  retained_generations: number[];
  coverage: { ingest_seq: number; structure_revision: number; policy_revision: number };
  physical_links: { id: string; from: string; to: string; role: "DERIVED_FROM" | "ConductingArc" }[];
  invalidation_evidence: { id: string; source_hash: string; outcome_hash: string }[];
  source_hashes: string[];
}
export type AuthoritySnapshotRefusal = "authority_members_missing" | "authority_generations_missing" |
  "authority_coverage_missing" | "authority_links_missing" | "authority_invalidation_missing" | "authority_sources_missing";
export class AuthoritySnapshotError extends Error {
  readonly code: AuthoritySnapshotRefusal;
  constructor(code: AuthoritySnapshotRefusal, detail: string) { super(`${code}: ${detail}`); this.name = "AuthoritySnapshotError"; this.code = code; }
}
function authoritySorted(values: string[], code: AuthoritySnapshotRefusal): void {
  if (!values.every((v, i) => i === 0 || values[i - 1]! < v)) throw new AuthoritySnapshotError(code, "identities must be sorted and unique");
}
/** Validate adapter evidence before any offline dump is requested. Missing
 * Store APIs are a refusal, never an empty/guessed authority set. */
export function verifyAuthoritySnapshot(value: unknown): AuthoritySnapshot {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new AuthoritySnapshotError("authority_members_missing", "snapshot is absent");
  const v = value as Record<string, unknown>;
  const expectedKeys = ["members", "retained_generations", "coverage", "physical_links", "invalidation_evidence", "source_hashes"];
  if (Object.keys(v).length !== expectedKeys.length || expectedKeys.some(k => !Object.hasOwn(v, k))) throw new AuthoritySnapshotError("authority_members_missing", "snapshot has missing or unknown fields");
  const members = v.members; if (!Array.isArray(members) || members.length === 0 || !members.every(x => typeof x === "string")) throw new AuthoritySnapshotError("authority_members_missing", "Neo4j member enumeration was not supplied");
  authoritySorted(members as string[], "authority_members_missing");
  const generations = v.retained_generations; if (!Array.isArray(generations) || !generations.every(x => typeof x === "number" && Number.isSafeInteger(x) && x >= 0) || generations.some((x, i) => i > 0 && (generations[i - 1] as number) >= x)) throw new AuthoritySnapshotError("authority_generations_missing", "retained generation coverage is absent or unsorted");
  const coverage = v.coverage as Record<string, unknown> | undefined;
  if (!coverage || !["ingest_seq", "structure_revision", "policy_revision"].every(k => typeof coverage[k] === "number" && Number.isSafeInteger(coverage[k]) && (coverage[k] as number) >= 0)) throw new AuthoritySnapshotError("authority_coverage_missing", "cutoff coverage is absent");
  const links = v.physical_links; if (!Array.isArray(links) || !links.every(x => x && typeof x === "object" && typeof (x as Record<string, unknown>).id === "string" && typeof (x as Record<string, unknown>).from === "string" && typeof (x as Record<string, unknown>).to === "string" && ((x as Record<string, unknown>).role === "DERIVED_FROM" || (x as Record<string, unknown>).role === "ConductingArc"))) throw new AuthoritySnapshotError("authority_links_missing", "physical DERIVED_FROM/ConductingArc evidence is absent");
  const invalidation = v.invalidation_evidence; if (!Array.isArray(invalidation) || !invalidation.every(x => x && typeof x === "object" && typeof (x as Record<string, unknown>).id === "string" && text((x as Record<string, unknown>).source_hash, HEX) && text((x as Record<string, unknown>).outcome_hash, HEX))) throw new AuthoritySnapshotError("authority_invalidation_missing", "invalidation evidence is absent or unhashed");
  const sources = v.source_hashes; if (!Array.isArray(sources) || !sources.every(x => text(x, HEX))) throw new AuthoritySnapshotError("authority_sources_missing", "source hashes are absent");
  authoritySorted(sources as string[], "authority_sources_missing");
  return { members: members as string[], retained_generations: generations as number[], coverage: coverage as AuthoritySnapshot["coverage"], physical_links: links as AuthoritySnapshot["physical_links"], invalidation_evidence: invalidation as AuthoritySnapshot["invalidation_evidence"], source_hashes: sources as string[] };
}
export interface ArchiveEmbeddingProfile { embedding_profile_id: string; embedding_model_id: string; vector_index_id: string }
export interface ArchiveEmbeddingCoverage {
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
export interface ArchiveCompletion {
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
function record(value: unknown, keys: string[], code: ArchiveAdmissionCode): Record<string, unknown> {
  need(value !== null && typeof value === "object" && !Array.isArray(value), code, "expected object");
  const obj = value as Record<string, unknown>;
  need(Object.keys(obj).length === keys.length && keys.every(k => Object.hasOwn(obj, k)), code, "unexpected or missing field");
  return obj;
}
function integer(value: unknown, max = Number.MAX_SAFE_INTEGER, min = 0): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= min && value <= max;
}
function text(value: unknown, pattern: RegExp, max = 256): value is string {
  return typeof value === "string" && value.length <= max && pattern.test(value);
}
function array(value: unknown, max: number, code: ArchiveAdmissionCode): unknown[] {
  need(Array.isArray(value) && value.length <= max, code, "array exceeds admission limit or is absent"); return value;
}
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const obj = value as Record<string, unknown>;
    return `{${Object.keys(obj).sort().map(k => `${JSON.stringify(k)}:${canonical(obj[k])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
function sha256(bytes: string | Uint8Array): string { return createHash("sha256").update(bytes).digest("hex"); }
/** Duplicate keys must reject even for legacy ObjectStore sidecars, whose
 * insertion-ordered JSON is not the canonical manifest byte representation. */
function decode(bytes: string | Uint8Array, max: number, code: ArchiveAdmissionCode, requireCanonical: boolean): unknown {
  need((typeof bytes === "string" ? Buffer.byteLength(bytes) : bytes.byteLength) <= max, code, "JSON byte limit");
  let raw: string, value: unknown;
  try {
    raw = typeof bytes === "string" ? bytes : new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
    const stack: Array<Set<string> | null> = [];
    for (let i = 0; i < raw.length; i++) {
      const c = raw[i];
      if (c === '"') {
        const start = i++;
        while (i < raw.length && raw[i] !== '"') { if (raw[i] === "\\") i++; i++; }
        let next = i + 1; while (/\s/.test(raw[next] ?? "x")) next++;
        if (raw[next] === ":") {
          const key = JSON.parse(raw.slice(start, i + 1)) as string, keys = stack.at(-1);
          need(keys && !keys.has(key), code, "duplicate JSON key"); keys.add(key);
        }
      } else if (c === "{" || c === "[") {
        stack.push(c === "{" ? new Set() : null); need(stack.length <= ARCHIVE_LIMITS.json_depth, code, "JSON depth limit");
      } else if (c === "}" || c === "]") stack.pop();
    }
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

/** Wire identity: sorted-key compact JSON, no BOM/newline/duplicate keys,
 * canonical JSON numbers/strings, sorted identity arrays; ASCII schema fields.
 * The completion hash binds these exact UTF-8 bytes, not a reserialized input.
 */
export function parseArchiveManifest(bytes: string | Uint8Array): ArchiveManifest {
  const code = "invalid_manifest";
  const decoded = decode(bytes, ARCHIVE_LIMITS.manifest_bytes, code, true);
  need(decoded !== null && typeof decoded === "object" && !Array.isArray(decoded), code, "expected object");
  const m = record(decoded,
    Object.hasOwn(decoded, "authority") ? ["format", "operation_id", "cutoff", "compatibility", "configuration", "models", "objects", "members", "authority"] : ["format", "operation_id", "cutoff", "compatibility", "configuration", "models", "objects", "members"], code);
  need(m.format === "anamnesis.archive/1" && text(m.operation_id, UUID7), code, "unsupported format or operation identity");
  const cutoff = record(m.cutoff, ["ingest_seq", "structure_revision", "policy_revision"], code);
  need(Object.values(cutoff).every(v => integer(v)), code, "invalid cutoff counter");
  const compat = record(m.compatibility, ["schema_version", "neo4j_version", "neo4j_image_digest", "episode_digest_version_ceiling"], code);
  need(text(compat.schema_version, /^anamnesis\.storage\/[1-9][0-9]{0,5}$/) && text(compat.neo4j_version, NEO4J) &&
    text(compat.neo4j_image_digest, IMAGE) && (compat.episode_digest_version_ceiling === 1 || compat.episode_digest_version_ceiling === 2), code, "invalid compatibility contract");
  const config = record(m.configuration, ["config_sha256", "receipt_retention_ms", "prior_version", "calibration_version", "dynamics_version"], code);
  need(text(config.config_sha256, HEX) && integer(config.receipt_retention_ms, ARCHIVE_LIMITS.receipt_retention_ms, 1) &&
    [config.prior_version, config.calibration_version, config.dynamics_version].every(v => text(v, VERSION)), code, "invalid config pins");
  const models = record(m.models, ["active_embedding_profile_id", "embedding_profiles", "embedding_coverages", "extraction"], code);
  const profiles = array(models.embedding_profiles, ARCHIVE_LIMITS.embedding_profiles, code).map(value => {
    const p = record(value, ["embedding_profile_id", "embedding_model_id", "vector_index_id"], code);
    need(Object.values(p).every(v => text(v, HEX)), code, "invalid embedding fingerprint");
    need(p.embedding_profile_id === sha256(canonical({ embedding_model_id: p.embedding_model_id, vector_index_id: p.vector_index_id })), code, "profile fingerprint mismatch");
    return p as unknown as ArchiveEmbeddingProfile;
  });
  sortedUnique(profiles.map(p => p.embedding_profile_id), code);
  need(models.active_embedding_profile_id === null || profiles.some(p => p.embedding_profile_id === models.active_embedding_profile_id), code, "unknown active profile");
  let extractionGeneration: number | null = null;
  if (models.extraction !== null) {
    const e = record(models.extraction, ["generation", "fact_language_policy", "grouping_version", "judge_profile_id"], code);
    need(integer(e.generation, Number.MAX_SAFE_INTEGER, 1) && text(e.fact_language_policy, VERSION) && text(e.grouping_version, VERSION) && text(e.judge_profile_id, HEX), code, "invalid extraction pins");
    extractionGeneration = e.generation;
  }
  const modelIds = new Set(profiles.map(p => p.embedding_model_id)), coverageKeys = new Set<string>();
  const coverages = array(models.embedding_coverages, ARCHIVE_LIMITS.embedding_coverages, code).map(value => {
    const c = record(value, ["embedding_model_id", "stream", "generation", "covered_ingest_seq", "health", "resolved_no_vector_count", "omission_digest"], code);
    need(text(c.embedding_model_id, HEX) && modelIds.has(c.embedding_model_id) && (c.stream === "episode" || c.stream === "extraction") &&
      integer(c.generation, Number.MAX_SAFE_INTEGER, c.stream === "episode" ? 0 : 1) && (c.stream !== "episode" || c.generation === 0) &&
      integer(c.covered_ingest_seq, cutoff.ingest_seq as number) && (c.health === "HEALTHY" || c.health === "BLOCKED") &&
      integer(c.resolved_no_vector_count) && text(c.omission_digest, HEX), code, "invalid model coverage");
    const key = `${c.embedding_model_id}/${c.stream}/${c.generation}`;
    need(!coverageKeys.has(key), code, "duplicate coverage partition"); coverageKeys.add(key);
    return c as unknown as ArchiveEmbeddingCoverage;
  });
  // Numeric generation ordering, not decimal-string ordering.
  need(coverages.every((c, i) => {
    const p = coverages[i - 1]; return !p || p.embedding_model_id < c.embedding_model_id ||
      (p.embedding_model_id === c.embedding_model_id && (p.stream < c.stream || (p.stream === c.stream && p.generation < c.generation)));
  }), code, "unsorted coverage partitions");
  for (const id of modelIds) need(coverageKeys.has(`${id}/episode/0`), code, "missing model episode coverage");
  const active = profiles.find(p => p.embedding_profile_id === models.active_embedding_profile_id);
  if (active && extractionGeneration !== null) need(coverageKeys.has(`${active.embedding_model_id}/extraction/${extractionGeneration}`), code, "missing active extraction coverage");
  const expected = new Map(FIXED), objects = new Map<string, ArchiveObject>();
  for (const value of array(m.objects, ARCHIVE_LIMITS.objects, code)) {
    const obj = record(value, ["hash", "size", "media_type"], code);
    need(text(obj.hash, HEX) && integer(obj.size, ARCHIVE_LIMITS.object_bytes) && text(obj.media_type, MEDIA, 255), code, "invalid object identity");
    const path = `objects/${obj.hash.slice(0, 2)}/${obj.hash}`;
    need(!objects.has(obj.hash), code, "duplicate object");
    objects.set(obj.hash, obj as unknown as ArchiveObject); expected.set(path, "object_data"); expected.set(path + ".json", "object_sidecar");
  }
  sortedUnique([...objects.keys()], code);
  const members = array(m.members, ARCHIVE_LIMITS.members, code).map(value => {
    const member = record(value, ["path", "role", "bytes", "sha256"], code);
    need(typeof member.path === "string" && expected.get(member.path) === member.role && expected.has(member.path), code, "noncanonical, unknown or wrong-role member path");
    need(integer(member.bytes, memberLimit[member.role as Role], member.role === "object_data" ? 0 : 1) && text(member.sha256, HEX), code, "invalid member size/hash");
    if (member.role === "object_data") {
      const hash = member.path.slice(-64), obj = objects.get(hash)!;
      need(member.sha256 === hash && member.bytes === obj.size, code, "object naming/data disagreement");
    }
    if (member.role === "config") need(member.sha256 === config.config_sha256, code, "config fingerprint mismatch");
    return member as unknown as ArchiveMember;
  });
  sortedUnique(members.map(member => member.path), code);
  if (Object.hasOwn(m, "authority")) m.authority = verifyAuthoritySnapshot(m.authority);
  need(members.length === expected.size && members.reduce((n, member) => n + member.bytes, 0) <= ARCHIVE_LIMITS.total_member_bytes, code, "missing members or total byte limit");
  return m as unknown as ArchiveManifest;
}
export function parseArchiveCompletion(bytes: string | Uint8Array): ArchiveCompletion {
  const code = "invalid_completion", marker = record(decode(bytes, ARCHIVE_LIMITS.completion_bytes, code, true), ["format", "operation_id", "manifest_sha256", "manifest_bytes"], code);
  need(marker.format === "anamnesis.archive-complete/1" && text(marker.operation_id, UUID7) && text(marker.manifest_sha256, HEX) && integer(marker.manifest_bytes, ARCHIVE_LIMITS.manifest_bytes, 1), code, "invalid completion contract");
  return marker as unknown as ArchiveCompletion;
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
function sameStat(a: BigIntStats, b: BigIntStats): boolean {
  return a.dev === b.dev && a.ino === b.ino && a.mode === b.mode && a.uid === b.uid && a.gid === b.gid &&
    a.nlink === b.nlink && a.size === b.size && a.mtimeNs === b.mtimeNs && a.ctimeNs === b.ctimeNs;
}
async function directory(path: string): Promise<BigIntStats> {
  const info = await lstat(path, { bigint: true });
  need(info.isDirectory() && !info.isSymbolicLink() && info.uid === BigInt(process.getuid!()) && (info.mode & 0o022n) === 0n, "archive_layout", "archive directories must be owned, non-writable by others, and not links");
  return info;
}
async function readMember(path: string, max: number, collect: boolean, expected?: ArchiveMember): Promise<{ hash: string; bytes: number; content: Buffer; stat: BigIntStats }> {
  need(await realpath(dirname(path)) === dirname(path), "archive_layout", "linked parent directory");
  const before = await lstat(path, { bigint: true });
  need(before.isFile() && before.nlink === 1n, "archive_layout", "member must be a single-link regular file");
  need(before.size <= BigInt(max), "archive_limit", "file exceeds admission byte limit");
  if (expected) need(before.size === BigInt(expected.bytes), "member_mismatch", "member length mismatch");
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    need(sameStat(before, await file.stat({ bigint: true })), "archive_changed", "file replaced before open");
    const digest = createHash("sha256"), chunks: Buffer[] = [];
    const buffer = Buffer.alloc(Math.min(ARCHIVE_LIMITS.hash_chunk_bytes, max + 1));
    let bytes = 0;
    while (true) {
      const result = await file.read(buffer, 0, Math.min(buffer.length, max - bytes + 1), null);
      if (result.bytesRead === 0) break;
      bytes += result.bytesRead; need(bytes <= max, "archive_limit", "file grew beyond admission limit");
      const chunk = buffer.subarray(0, result.bytesRead); digest.update(chunk); if (collect) chunks.push(Buffer.from(chunk));
    }
    need(BigInt(bytes) === before.size && sameStat(before, await file.stat({ bigint: true })) && sameStat(before, await lstat(path, { bigint: true })), "archive_changed", "member changed during read");
    const hash = digest.digest("hex");
    if (expected) need(hash === expected.sha256, "member_mismatch", "member SHA-256 mismatch");
    return { hash, bytes, content: Buffer.concat(chunks), stat: before };
  } finally { await file.close(); }
}
async function inventory(root: string, manifest: ArchiveManifest, stamps: Map<string, BigIntStats>): Promise<void> {
  const files = new Set(["manifest.json", "backup.complete", ...manifest.members.map(m => m.path)]), dirs = new Set([""]);
  for (const file of files) {
    let parent = dirname(file);
    while (parent !== ".") { dirs.add(parent); parent = dirname(parent); }
  }
  // Only the manifest-derived root/database/objects/2-hex directories can be
  // visited. Unknown entries reject immediately; opendir never collects an
  // unbounded directory. The fixed grammar permits at most 259 directories.
  need(dirs.size <= 259, "archive_limit", "directory limit");
  const pending = [""], found = new Set<string>();
  for (let i = 0; i < pending.length; i++) {
    const relative = pending[i]!, path = join(root, relative), before = await directory(path);
    need(await realpath(path) === path, "archive_layout", "linked directory");
    const entries = await opendir(path);
    for await (const entry of entries) {
      const name = relative ? `${relative}/${entry.name}` : entry.name;
      need(!found.has(name) && (files.has(name) || dirs.has(name)), "archive_layout", "extra archive entry"); found.add(name);
      const stat = await lstat(join(root, name), { bigint: true });
      if (dirs.has(name)) { need(stat.isDirectory() && !stat.isSymbolicLink(), "archive_layout", "linked/non-directory parent"); pending.push(name); }
      else need(stat.isFile() && stat.nlink === 1n, "archive_layout", "non-regular, linked or directory member");
    }
    need(sameStat(before, await lstat(path, { bigint: true })), "archive_changed", "directory changed during listing");
    if (stamps.has(path)) need(sameStat(stamps.get(path)!, before), "archive_changed", "directory replaced");
    stamps.set(path, before);
  }
  need(found.size === files.size + dirs.size - 1, "archive_layout", "missing archive member");
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
    const completion = await readMember(join(root, "backup.complete"), ARCHIVE_LIMITS.completion_bytes, true);
    const marker = parseArchiveCompletion(completion.content);
    const raw = await readMember(join(root, "manifest.json"), ARCHIVE_LIMITS.manifest_bytes, true);
    const manifest = parseArchiveManifest(raw.content);
    need(marker.manifest_sha256 === raw.hash && marker.manifest_bytes === raw.bytes && marker.operation_id === manifest.operation_id, "completion_mismatch", "marker does not bind manifest bytes and operation");
    checkCompatibility(manifest, accepted);
    stamps.set(join(root, "manifest.json"), raw.stat); stamps.set(join(root, "backup.complete"), completion.stat);
    await inventory(root, manifest, stamps);
    const objects = new Map(manifest.objects.map(o => [o.hash, o]));
    let verifiedBytes = 0;
    for (const member of manifest.members) {
      const collect = member.role === "object_sidecar" || member.role === "dump_metadata";
      const result = await readMember(join(root, member.path), memberLimit[member.role], collect, member);
      stamps.set(join(root, member.path), result.stat); verifiedBytes += result.bytes;
      if (member.role === "object_sidecar") checkSidecar(result.content, objects.get(member.path.slice(-69, -5))!);
      if (member.role === "dump_metadata") checkDumpMetadata(result.content, manifest);
    }
    for (const [path, stamp] of stamps) need(sameStat(stamp, await lstat(path, { bigint: true })), "archive_changed", "archive changed before admission completed");
    need(sameStat(rootStat, await lstat(original, { bigint: true })), "archive_changed", "input root changed");
    return { status: "admitted", manifest, manifest_sha256: raw.hash, verified_members: manifest.members.length, verified_bytes: verifiedBytes };
  } catch (cause) {
    if (cause instanceof Error && "code" in cause && ["ENOENT", "ENOTDIR", "ELOOP"].includes(String(cause.code))) {
      throw new ArchiveAdmissionError("archive_layout", "missing or linked archive path", { cause });
    }
    throw cause;
  }
}
