import neo4j, {
  Driver,
  type ManagedTransaction,
  type Node,
  type Record as Neo4jRecord,
  type RecordShape,
  type Relationship,
} from "neo4j-driver";
import { createHash } from "node:crypto";
import { homedir } from "node:os";
import { join } from "node:path";
import { v7 as uuidv7 } from "uuid";
import {
  LINK_LATTICE,
  MemoryElement,
  MemoryLink,
  RpcCommitParams,
  SCHEMA_LABELS,
  TIME_BEARING,
  ExtractionAttempt, Generation, ModelTask, Coverage,
  type MemoryElementInput,
  type MemoryLinkInput,
  type Celestial,
  type KnownSchema,
  type LinkRole,
  type Origin,
  type TimePoint,
} from "@anamnesis/protocol";
import { RpcPolicySetParams, RpcPolicyRevokeParams, type RpcPolicyResult } from "../../protocol/src/rpc.ts";
import { ObjectStore } from "./objects.ts";
import { EchoLineage, EpisodeLineageError, RecallLineageSelection, parseEpisodeLineage, type EpisodeLineageInput } from "../../protocol/src/episode-lineage.ts";
import { SemanticClaimValidationError, SemanticResolvedTime, validateSemanticClaim, type SemanticSourceContext, type ValidatedSemanticClaim } from "../../protocol/src/semantic-claim.ts";
import { CreateModelTask, ModelTaskCAS, LeaseModelTask, SettleModelTask, CompleteExtractionAttempt, AdvanceExtractionCoverage, SelectExtractionGeneration, ExtractionSelection, ReadExtractionCoverage, ExtractionCoverageRead, canonicalExtractionBody, extractionBodyDigest } from "../../protocol/src/extraction.ts";
import { validateModelOutput, validateSourceSpans } from "./extraction.ts";
import { ExtractionClaimContext, ExtractionJudgeInput, ExtractionPipeline, ExtractionDisposition, ExtractionAuditError } from '../../protocol/src/extraction-audit.ts';
import { ProposeRetainedClaim, MaterializeRetainedClaim, ReviewRetainedClaim, MaterializationResult, SemanticReviewPremises, SemanticResolution, SemanticReviewOutput, RetainedSemanticProposal, semanticReviewClaimBody } from "../../protocol/src/materialization.ts";
import { ExtractionModelOutput } from '../../protocol/src/extraction.ts';
import { HistoricalElement, historicalEligibility, type EligibilityReason } from "./legacy-format.ts";
import { z } from "zod";
import { replayDynamics, type DynamicsEvent } from "./dynamics/state.ts";
import { solvePpr } from "./dynamics/ppr.ts";

import { ADOPTION_NUMERIC_VERSION } from "./dynamics/adoption-numeric.ts";
import { attributeOutcome, normalizedRrf } from "./dynamics/ranking.ts";
import { initialStability, retention } from "./dynamics/retention.ts";
import { RpcEmbeddingRecoverParams, RpcEmbeddingAttempt, RpcRecallParams, RpcRecallResult, type RpcRecallItem, RpcDreamAdmitParams, RpcDreamJob, RpcDreamLeaseParams, RpcDreamExpireParams, RpcDreamExecuteParams } from "../../protocol/src/rpc.ts";
import { EmbeddingError, embeddingProfileId, validateVector, type EmbeddingProvider } from "./embedding.ts";
import { admittedBudget, packRecall, canonicalContext, RecallError, type Tokenizers, type RecallBundle } from "./recall.ts";
import { receiptBodyDigestInput, canonicalReceiptJson } from "./receipt-digest.ts";
import { DreamAdapterError, type DreamLeidenAdapter } from "./dream-leiden-adapter.ts";

export type ConductingArcRow = {
  source_id: string; link_id: string; peer_id: string; role: string;
  generation: number | null; source_extraction_generation: number | null;
};
export type ConductingArcProbe = { source_id: string; count: number; saturated: boolean; coverage: "complete" };
export type AuthoritySnapshot = {
  members: string[];
  retained_generations: number[];
  coverage: { ingest_seq: number; structure_revision: number; policy_revision: number };
  physical_links: { id: string; from: string; to: string; role: "DERIVED_FROM" | "ConductingArc" }[];
  invalidation_evidence: { id: string; source_hash: string; outcome_hash: string }[];
  source_hashes: string[];
};
export class AuthoritySnapshotError extends Error {
  constructor(readonly code: "authority_snapshot_unavailable" | "authority_snapshot_limit_exceeded", detail: string = code) { super(`${code}: ${detail}`); }
}
export type GraphEnvelope = { nodes: string[]; arcs: ConductingArcRow[]; truncated: boolean; probes: ConductingArcProbe[];
  pin: { policy_revision: number; generation_id: string; coverage_revision: number; covered_ingest_seq: number; T: number };
  overflow: { nodes: number; arcs: number; saturated_sources: number } };
export class GraphAccessError extends Error {
  readonly code: "degree_probe_unavailable" | "ordered_probe_unavailable";
  constructor(readonly reason: "degree_probe_unavailable" | "ordered_probe_unavailable") { super(reason); this.code = reason; this.name = "GraphAccessError"; }
}

export class GenerationReadinessError extends Error {
  constructor(readonly code: "coverage_unavailable" | "coverage_incomplete" | "generation_work_in_flight" | "generation_watermark_unavailable"
    | "selector_unavailable" | "selector_conflict" | "selector_version_conflict" | "selector_version_exhausted"
    | "generation_not_active" | "activation_prerequisite_unavailable", readonly prerequisites: readonly string[] = []) {
    super(`${code}${prerequisites.length ? `: ${prerequisites.join(",")}` : ""}`);
    this.name = "GenerationReadinessError";
  }
}

export interface StoreOptions {
  uri: string;
  user: string;
  password: string;
  database?: string;
  objectsRoot?: string;
  /** Server clock only, never accepted from a feedback request. */
  clock?: () => number;
  embeddingProvider?: EmbeddingProvider;
  tokenizers?: Tokenizers;
  recallDefaultBytes?: number;
  /** Trusted runtime injection only; never loaded from an RPC or arbitrary command. */
  dreamLeidenAdapter?: DreamLeidenAdapter;
}

export interface PutResult {
  id: string;

  created: boolean;

  diverged?: boolean;

  invalidated?: string;
}

export interface SearchHit {
  element: MemoryElement;

  score: number;
}

export interface IntegrityIssue {
  elementId: string;
  kind: "digest-mismatch" | "missing-payload" | "payload-hash-mismatch"
    | "unsupported-digest-format" | "topology-mismatch" | "unsupported-topology-format"
    | "malformed-element" | "semantic-ineligibility";
  reasons?: EligibilityReason[];
}

type ElementProperties = Record<string, string | number | null>;
type LinkProperties = Record<string, string | number>;
type ElementNode = Node<number, ElementProperties>;
type LinkRelationship = Relationship<number, LinkProperties>;
type QueryParameter =
  | string
  | number
  | boolean
  | null
  | Buffer
  | string[]
  | ReturnType<typeof neo4j.int>;
type QueryParameters = Record<string, QueryParameter>;

interface ElementWriteOptions {
  payload?: Uint8Array;
  payloadMediaType?: string;
  sourceRevision?: string;
  expectedPreviousRevisionKey?: string | null;
  enqueue?: boolean;
  previous?: string;
  admission?: { metadata: unknown; context: InstallationContext };
}

const LINK_ROLES = Object.keys(LINK_LATTICE) as LinkRole[];
const CONDUCTING_ROLES = ["NEXT_EPISODE", "MENTIONS", "RELATES_TO", "HAS_MEMBER", "DERIVED_FROM"] as const;
type ConductingPartition = { stream: string; generation: number | string; state?: string };
type PhysicalConductor = ConductingArcRow & { from: string; to: string; registry_source: number | null };
export type ConductingArcVerification = {
  ready: boolean; revision: number | null; truncated: boolean; physical_links: number; endpoint_rows: number;
  partitions: ConductingPartition[]; issues: string[];
};
const ConductingMaintenanceOptions = z.strictObject({ maxItems: z.number().int().min(1).max(50000).default(10000) });
function conductingPartition(role: string, generation: number | null): ConductingPartition {
  return { stream: role === "NEXT_EPISODE" ? "cache" : role === "HAS_MEMBER" ? "community" : "extraction", generation: generation ?? 0 };
}
function arcIdentity(row: ConductingArcRow): string { return JSON.stringify([row.source_id, row.link_id]); }
function arcTuple(row: ConductingArcRow): string {
  return JSON.stringify([row.source_id, row.link_id, row.peer_id, row.role, row.generation, row.source_extraction_generation]);
}

const SCHEMA_STATEMENTS = [
  `CREATE CONSTRAINT echo_lineage_episode IF NOT EXISTS FOR (l:EchoLineage) REQUIRE l.episode_id IS UNIQUE`,
  `CREATE CONSTRAINT embedding_attempt_id IF NOT EXISTS FOR (a:EmbeddingAttempt) REQUIRE a.operation_id IS UNIQUE`,
  `CREATE CONSTRAINT embedding_vector_key IF NOT EXISTS FOR (v:EmbeddingVector) REQUIRE v.key IS UNIQUE`,
  `CREATE CONSTRAINT embedding_profile_id IF NOT EXISTS FOR (p:EmbeddingProfile) REQUIRE p.id IS UNIQUE`,
  `CREATE CONSTRAINT policy_authority_key IF NOT EXISTS FOR (p:PolicyAuthority) REQUIRE p.key IS UNIQUE`,
  `CREATE CONSTRAINT policy_event_revision IF NOT EXISTS FOR (p:PolicyEvent) REQUIRE p.revision IS UNIQUE`,
  `CREATE CONSTRAINT policy_event_key IF NOT EXISTS FOR (p:PolicyEvent) REQUIRE p.key IS UNIQUE`,
  `CREATE CONSTRAINT recall_receipt_id IF NOT EXISTS FOR (r:RecallReceipt) REQUIRE r.recall_id IS UNIQUE`,
  `CREATE CONSTRAINT recall_transport_id IF NOT EXISTS FOR (r:RecallTransport) REQUIRE r.recall_id IS UNIQUE`,
  `CREATE CONSTRAINT receipt_operation_id IF NOT EXISTS FOR (r:RecallFeedback) REQUIRE r.operation_id IS UNIQUE`,
  `CREATE CONSTRAINT recall_outcome_id IF NOT EXISTS FOR (r:RecallOutcome) REQUIRE r.recall_id IS UNIQUE`,
  `CREATE CONSTRAINT hit_id IF NOT EXISTS FOR (h:Hit) REQUIRE h.id IS UNIQUE`,
  `CREATE CONSTRAINT hit_idem_key IF NOT EXISTS FOR (h:Hit) REQUIRE h.idem_key IS UNIQUE`,
  `CREATE CONSTRAINT hit_cache_episode IF NOT EXISTS FOR (c:HitCache) REQUIRE c.episode_id IS UNIQUE`,
  `CREATE INDEX receipt_expiry IF NOT EXISTS FOR (r:RecallReceipt) ON (r.expires_at)`,
  `CREATE INDEX hit_replay IF NOT EXISTS FOR (h:Hit) ON (h.episode_id, h.t, h.id)`,
  `CREATE CONSTRAINT conducting_arc_source_link IF NOT EXISTS FOR (a:ConductingArc) REQUIRE (a.source_id, a.link_id) IS UNIQUE`,
  `CREATE CONSTRAINT graph_next_episode_id IF NOT EXISTS FOR ()-[l:NEXT_EPISODE]-() REQUIRE l.id IS UNIQUE`,
  ...CONDUCTING_ROLES.filter(role => role !== "NEXT_EPISODE").map(role =>
    `CREATE CONSTRAINT graph_${role.toLowerCase()}_id IF NOT EXISTS FOR ()-[l:${role}]-() REQUIRE l.id IS UNIQUE`),
  `CREATE CONSTRAINT conducting_arc_coverage IF NOT EXISTS FOR (c:ConductingArcCoverage) REQUIRE (c.stream,c.generation) IS UNIQUE`,
  `CREATE INDEX conducting_arc_link IF NOT EXISTS FOR (a:ConductingArc) ON (a.link_id)`,
  `CREATE INDEX hub_arc_link IF NOT EXISTS FOR (a:HubArc) ON (a.link_id)`,
  `CREATE CONSTRAINT conducting_generation_partition IF NOT EXISTS FOR (g:Generation) REQUIRE (g.stream,g.generation) IS UNIQUE`,

  `CREATE CONSTRAINT element_id IF NOT EXISTS
   FOR (e:Element) REQUIRE e.id IS UNIQUE`,

  ...LINK_ROLES.map(
    (role) => `CREATE CONSTRAINT link_idem_${role.toLowerCase()} IF NOT EXISTS
   FOR ()-[l:${role}]-() REQUIRE l.idem_key IS UNIQUE`,
  ),
  `CREATE CONSTRAINT episode_revision IF NOT EXISTS
   FOR (e:Episode) REQUIRE e.revision_key IS UNIQUE`,
  `CREATE CONSTRAINT episode_ingest_seq IF NOT EXISTS
   FOR (e:Episode) REQUIRE e.ingest_seq IS UNIQUE`,
  `CREATE CONSTRAINT origin_head_key IF NOT EXISTS
   FOR (h:OriginHead) REQUIRE h.origin_key IS UNIQUE`,
  `CREATE CONSTRAINT payload_hash IF NOT EXISTS
   FOR (p:Payload) REQUIRE p.hash IS UNIQUE`,
  // Without it, remembers racing on a cold database each MERGE their own Meta
  // node and hand out the same ingest_seq.
  `CREATE CONSTRAINT meta_key IF NOT EXISTS
   FOR (m:Meta) REQUIRE m.key IS UNIQUE`,
  `CREATE INDEX episode_origin IF NOT EXISTS
   FOR (e:Episode) ON (e.origin_key)`,
  `CREATE INDEX episode_session_order IF NOT EXISTS
   FOR (e:Episode) ON (e.session_key, e.time_utc, e.ingest_seq)`,
  `CREATE INDEX element_time IF NOT EXISTS
   FOR (e:Element) ON (e.time_utc)`,
  `CREATE INDEX element_schema IF NOT EXISTS
   FOR (e:Element) ON (e.schema)`,
  `CREATE INDEX outbox_pending IF NOT EXISTS
   FOR (o:Outbox) ON (o.processed_at)`,
  // valid(T) seeks invalidators by target instead of expanding adjacency.
  `CREATE CONSTRAINT extraction_generation_id IF NOT EXISTS FOR (g:ExtractionGeneration) REQUIRE g.id IS UNIQUE`,
  `CREATE CONSTRAINT extraction_attempt_id IF NOT EXISTS FOR (a:ExtractionAttempt) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT model_task_id IF NOT EXISTS FOR (t:ModelTask) REQUIRE t.id IS UNIQUE`,
  `CREATE CONSTRAINT model_task_work_key IF NOT EXISTS FOR (t:ModelTask) REQUIRE t.work_key IS UNIQUE`,
  `CREATE CONSTRAINT extraction_pipeline_id IF NOT EXISTS FOR (p:ExtractionPipeline) REQUIRE p.id IS UNIQUE`,
  `CREATE CONSTRAINT extraction_pipeline_judge IF NOT EXISTS FOR (p:ExtractionPipeline) REQUIRE p.judge_task_id IS UNIQUE`,
  `CREATE CONSTRAINT extraction_judge_input_id IF NOT EXISTS FOR (p:ExtractionJudgeInput) REQUIRE p.id IS UNIQUE`,
  `CREATE CONSTRAINT extraction_disposition_key IF NOT EXISTS FOR (d:ExtractionDisposition) REQUIRE (d.judge_attempt_id,d.claim_index) IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_input_id IF NOT EXISTS FOR (a:AdjudicationInput) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_attempt_id IF NOT EXISTS FOR (a:AdjudicationAttempt) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_proposal_id IF NOT EXISTS FOR (a:AdjudicationProposal) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_review_id IF NOT EXISTS FOR (a:AdjudicationReview) REQUIRE a.review_id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_review_proposal IF NOT EXISTS FOR (a:AdjudicationReview) REQUIRE a.proposal_id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_consumption_id IF NOT EXISTS FOR (a:AdjudicationConsumption) REQUIRE a.proposal_id IS UNIQUE`,
  `CREATE CONSTRAINT materialization_operation_id IF NOT EXISTS FOR (a:MaterializationOperation) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT materialization_occurrence IF NOT EXISTS FOR (a:MaterializationOperation) REQUIRE a.occurrence_key IS UNIQUE`,
  `CREATE INDEX fact_generation_id IF NOT EXISTS FOR (f:Fact) ON (f.generation,f.id)`,
  `CREATE INDEX entity_generation_key IF NOT EXISTS FOR (e:Entity) ON (e.generation,e.entity_key)`,
  `CREATE CONSTRAINT extraction_coverage_key IF NOT EXISTS FOR (c:ExtractionCoverage) REQUIRE c.key IS UNIQUE`,
  `CREATE INDEX invalidates_seek IF NOT EXISTS
   FOR ()-[l:INVALIDATES]-() ON (l.target_id, l.effective_time_utc, l.id)`,

  `CREATE FULLTEXT INDEX element_content IF NOT EXISTS
   FOR (e:Element) ON EACH [e.content]
   OPTIONS { indexConfig: { \`fulltext.analyzer\`: 'cjk' } }`,
];

/** Model-stated claim time becomes an explicit resolved time. Coarse precisions
 * are truncated to the UTC interval start; second/minute stay instants. */
function semanticClaimTime(time: { value: string; precision: "second" | "minute" | "day" | "month" | "year" }) {
  const d = new Date(time.value);
  const precision = time.precision === "second" || time.precision === "minute" ? "instant" as const : time.precision;
  if (precision !== "instant") {
    d.setUTCHours(0, 0, 0, 0);
    if (precision !== "day") d.setUTCDate(1);
    if (precision === "year") d.setUTCMonth(0);
  }
  return { time_value: time.value, time_utc: d.getTime(), time_precision: precision, resolution: "explicit" as const, anchor_time_utc: null };
}

function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

/** Used when no snapshot cutoff is given, so every invalidator applies. */
const END_OF_TIME = "9999-12-31T23:59:59.999Z";

const CANONICAL_DIGEST = "rfc8785-v1";

interface TopologyRow {
  id: string;
  sessionKey: string;
  record: string;
  previousRecord: string | null;
  timeUtc: string;
  ingestSeq: number;
  version: number | null;
  actual: ({ from: string; key: string | null } | null)[];
}

const TOPOLOGY_QUERY = `MATCH (e:Element:Episode)
  OPTIONAL MATCH (p)-[l:NEXT_EPISODE]->(e)
  RETURN e.id AS id, e.session_key AS sessionKey, e.origin_record AS record,
    e.topology_previous_record AS previousRecord, e.ingest_seq AS ingestSeq,
    e.topology_version AS version, e.time_utc AS timeUtc,
    collect(CASE WHEN l IS NULL THEN null ELSE {from: p.id, key: l.idem_key} END) AS actual
  ORDER BY sessionKey, timeUtc, ingestSeq`;

/** Derive expectations independently of the cache, retaining explicit-parent
 * semantics as observed at admission (later source revisions are not parents).
 */
function topologyExpectations(rows: TopologyRow[]): (TopologyRow & { parents: string[] })[] {
  const records = new Map<string, TopologyRow[]>();
  for (const row of rows) {
    const key = JSON.stringify([row.sessionKey, row.record]);
    const bucket = records.get(key) ?? [];
    bucket.push(row);
    records.set(key, bucket);
  }
  const previousBySession = new Map<string, string>();
  return rows.map((row) => {
    const previous = previousBySession.get(row.sessionKey);
    const parents = row.previousRecord === null
      ? previous === undefined ? [] : [previous]
      : (records.get(JSON.stringify([row.sessionKey, row.previousRecord])) ?? [])
        .filter((parent) => parent.ingestSeq < row.ingestSeq).map((parent) => parent.id);
    previousBySession.set(row.sessionKey, row.id);
    return { ...row, parents };
  });
}

class StorageContractError extends Error {
  constructor(readonly code: "revision_conflict" | "stale_revision"
    | "unsupported_digest_format" | "invalid_canonical_json" | "unsupported_topology_format",
    readonly detail: string) {
    super(`${code}: ${detail}`);
  }
}

/** RFC 8785: UTF-16 key order, ECMAScript primitives, no lone surrogates.
 * Serialize members directly: JSON.stringify(object) reorders integer keys.
 * Values have already crossed the protocol's JSON-only boundary.
 */
function canonicalJson(value: MemoryElement["properties"][string]): string {
  if (typeof value === "string" && /[\uD800-\uDFFF]/u.test(value)) {
    throw new StorageContractError("invalid_canonical_json", "lone surrogate");
  }
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
    .map(([key, member]) => `${canonicalJson(key)}:${canonicalJson(member)}`).join(",")}}`;
}

interface DigestContext {
  payloadHash?: string | null;
  previousRevisionKey?: string | null;
  format?: string | number | null;
  episodeDigestVersion?: number | null;
  originRole?: string | null;
  lineageDigest?: string | null;
}

export function elementDigest(
  e: {
    schema: string;
    time?: TimePoint | undefined;
    content: string;
    properties?: MemoryElement["properties"] | undefined;
  },
  context: DigestContext = {},
): string {
  const body = {
      schema: e.schema,
      content: e.content,
      properties: Object.fromEntries(
        Object.entries(e.properties ?? {}).filter(
          ([key]) => key !== "payload_hash",
        ),
      ),
      time: carriesTime(e.schema) ? e.time ?? null : null,
      payload_hash: context.payloadHash ?? null,
      previous_revision_key: context.previousRevisionKey ?? null,
    };
  if (context.episodeDigestVersion != null) {
    if (context.episodeDigestVersion !== 2 || context.format !== "episode-rfc8785-v2")
      throw new EpisodeLineageError("unsupported_digest_version");
    return sha256(canonicalJson({ episode_digest_version: 2, ...body,
      origin_role: context.originRole ?? null, lineage_digest: context.lineageDigest ?? null }));
  }
  // Absent stored markers mean frozen insertion-ordered legacy bytes, never
  // an invitation to migrate or sort a previously admitted original.
  switch (context.format) {
    case null: return sha256(JSON.stringify(body));
    case undefined:
    case CANONICAL_DIGEST: return sha256(canonicalJson(body));
    default: throw new StorageContractError("unsupported_digest_format", String(context.format));
  }
}

export function verifyLineageRetry(input: unknown, role: string | null, lineage: EchoLineage): void {
  const metadata = parseEpisodeLineage(input);
  if (metadata.origin_role !== role || metadata.lineage_mode !== lineage.lineage_mode
    || canonicalExtractionBody(metadata.parent_recall_ids) !== canonicalExtractionBody(lineage.parent_recall_ids))
    throw new StorageContractError("revision_conflict", lineage.episode_id);
}

function tupleHash(parts: readonly string[]): string {
  return sha256(JSON.stringify(parts));
}

function originKey(o: Origin): string {
  return tupleHash([o.source, o.session, o.actor, o.record]);
}

function sessionKey(o: Origin): string {
  return tupleHash([o.source, o.session]);
}

/** Originals-layer links are content-free keys; derived links bind content. */
function linkIdemKey(
  l: {
    from: string;
    to: string;
    role: string;
    content: string;
  },
  originals: boolean,
): string {
  return sha256(
    JSON.stringify(
      originals
        ? [l.from, l.to, l.role]
        : [l.from, l.to, l.role, l.content],
    ),
  );
}

function celestialOf(schema: string): Celestial | null {
  return SCHEMA_LABELS[schema as KnownSchema] ?? null;
}

/** Only Episode and Fact carry an event time (docs/03 §1). */
function carriesTime(schema: string): boolean {
  const c = celestialOf(schema);
  return c !== null && TIME_BEARING[c];
}

function labelClause(schema: string): string {
  const c = celestialOf(schema);
  return c ? `Element:${c}` : "Element";
}

function toUtc(isoWithOffset: string): string {
  return new Date(isoWithOffset).toISOString();
}

export function luceneQuery(raw: string): string {
  return raw
    .replace(/[+\-&|!(){}\[\]^"~*?:\\\/]/g, " ")
    .split(/\s/)
    .filter(Boolean)
    .join(" ");
}

const receiptTime = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const receiptHash = z.string().regex(/^[0-9a-f]{64}$/);
const receiptIds = z.array(z.uuidv7()).max(64).refine((ids) => new Set(ids).size === ids.length, "IDs must be distinct");
export const IssueReceiptInput = z.strictObject({
  recall_id: z.uuidv7(),
  /** Ordered, already selected Episodes. No ranking or client source claims. */
  primary_ids: receiptIds,
  receipt_ttl_ms: receiptTime.positive().default(3_600_000),
});
export type IssueReceiptInput = z.input<typeof IssueReceiptInput>;
const receiptPrimary = z.strictObject({ id: z.uuidv7(), rank: receiptTime, sources: z.array(z.uuidv7()).min(1).max(16) });
export const RecallReceipt = z.strictObject({
  recall_id: z.uuidv7(), format: z.literal("episode-selection-v1"),
  principal: z.literal("installation"),
  // Legacy receipts predate auto exposure and cannot authorize it.
  commit_mode: z.enum(["auto", "receipt"]).default("receipt"),
  primary_ids: receiptIds, primaries: z.array(receiptPrimary).max(64),
  receipt_ttl_ms: receiptTime.positive(), created_at: receiptTime, expires_at: receiptTime,
  structure_revision: receiptTime.nullable(), policy_revision: receiptTime.nullable(),
  config_version: z.literal("g003-dynamics-v1"),
  body_digest: receiptHash, selection_digest: receiptHash,
  client_binding: z.uuid().optional(),
  lineage_selection: RecallLineageSelection.optional(),
  serving: z.strictObject({ response: RpcRecallResult, context_digest: receiptHash, result_digest: receiptHash, query: z.string().max(8192),
    query_vector: z.array(z.number().finite()).max(4096).nullable(),
    candidates: z.array(z.strictObject({ id: z.uuidv7(), score: z.number(), relevance: z.number(), mass: z.number(), utility: z.number() })).max(177),
  }).optional(),
});
export type RecallReceipt = z.infer<typeof RecallReceipt>;
/** Server-only observations, never client feedback or proof of consumption.
 * A missing record (including a crash before its append) remains unknown. */
export const RecallTransportInput = z.strictObject({
  recall_id: z.uuidv7(), state: z.enum(["local_complete", "delivery_unknown"]),
});
export type RecallTransportInput = z.infer<typeof RecallTransportInput>;
const RecallTransport = RecallTransportInput.extend({
  created_at: receiptTime, principal: z.literal("installation"),
  commit_mode: z.enum(["auto", "receipt"]), boundary: z.literal("node-write-callback-v1"),
});
export const CommitReceiptInput = RpcCommitParams;
export type CommitReceiptInput = RpcCommitParams;
export interface CommitReceiptResult {
  operation_id: string; recall_id: string; adopted: string[]; reward: number | null; applied: boolean;
}
export type ReceiptStatus = { state: "unknown"; operation_id: string } | {
  state: "committed"; operation_id: string; body_digest: string; created_at: number; result: CommitReceiptResult;
};
export interface HitCacheIssue { code: "hit_cache_mismatch" | "invalid_hit_evidence"; id: string }
export interface HitCacheVerification { state: "verified"; hits: number; issues: HitCacheIssue[] }
export interface HitCacheRebuild { state: "rebuilt"; hits: number; created: number; removed: number }
export interface HitCache {
  episode_id: string; s: number; t_last_hit: number; hit_count: number;
  utility_reward_sum: number; utility_weight: number; utility: number;
  event_ids: string[]; config_version: "g003-dynamics-v1";
  /** Missing on legacy native-transcendental caches; verify/rebuild detects it. */
  numeric_version?: typeof ADOPTION_NUMERIC_VERSION;
}
const hitBase = {
  id: z.uuidv7(), episode_id: z.uuidv7(), operation_id: z.uuidv7(), namespace: z.uuidv7(),
  idem_key: receiptHash, t: receiptTime,
  attribution: z.array(receiptPrimary).min(1).max(64), config_version: z.literal("g003-dynamics-v1"),
};
export const ReceiptHit = z.discriminatedUnion("kind", [
  z.strictObject({ ...hitBase, kind: z.literal("exposure"), kappa_eff: z.literal(0) }),
  z.strictObject({ ...hitBase, kind: z.literal("recall_hit"), kappa_eff: z.number().positive().max(1) }),
  z.strictObject({ ...hitBase, kind: z.literal("outcome"), kappa_eff: z.literal(0), reward: z.number().min(-1).max(1), weight: z.number().positive().max(1) }),
]);
export type ReceiptHit = z.infer<typeof ReceiptHit>;
export class ReceiptError extends Error {
  constructor(readonly code: "unknown_recall" | "receipt_expired" | "idempotency_conflict" | "invalid_selection" | "invalid_hit_evidence" | "unsupported_policy"
    | "policy_denied" | "policy_unavailable" | "unknown_policy" | "resource_exhausted" | "unauthenticated" | "commit_mode_mismatch", detail = code) {
    super(`${code}: ${detail}`);
  }
}
/** Internal transport context, never parsed from request params. The daemon sets
 * this only after validating the installation token; client labels confer nothing. */
export interface InstallationContext { readonly principal: "installation"; readonly commit_mode: "auto" | "receipt"; readonly client_binding?: string }
const PolicyEvent = RpcPolicySetParams.extend({
  action: z.enum(["deny", "revoke"]), principal: z.literal("installation"),
  revision: receiptTime.positive(), created_at: receiptTime,
});
type PolicyEvent = z.infer<typeof PolicyEvent>;
function policySelector(selector: RpcPolicySetParams["selector"]): Record<string, string> {
  return { ...(selector.episode_id === undefined ? {} : { episode_id: selector.episode_id }),
    ...(selector.source === undefined ? {} : { source: selector.source }) };
}
function policyBody(value: RpcPolicySetParams | PolicyEvent): string {
  return canonicalJson({ ...value, selector: policySelector(value.selector) });
}
type PolicyState = { structure_revision: number | null; policy_revision: number; denies: Map<string, PolicyEvent>; revoked: Set<string> };
function requireInstallation(context: InstallationContext): void {
  if (context?.principal !== "installation") throw new ReceiptError("unauthenticated");
}
type CacheEvidence = {
  episodes: { id: string; mass: number; ingested_at: number }[];
  hits: { props: Record<string, unknown>; targets: string[] }[];
  caches: { props: HitCache; targets: string[] }[];
};
/** One statement observes ledger, originals and cache together. No write locks or
 * repairs in verification. The writer path already holds the Meta fence lock. */
const CACHE_EVIDENCE = `
  CALL () { MATCH (e:Element:Episode) WHERE $ids IS NULL OR e.id IN $ids
    RETURN collect({id:e.id, mass:e.mass, ingested_at:e.ingested_at}) AS episodes }
  CALL () { MATCH (h:Hit) WHERE $ids IS NULL OR h.episode_id IN $ids
    OPTIONAL MATCH (h)-[:HIT_OF]->(target)
    WITH h, collect(coalesce(target.id, 'invalid-target')) AS targets
    RETURN collect({props:properties(h), targets:targets}) AS hits }
  CALL () { MATCH (c:HitCache) WHERE $ids IS NULL OR c.episode_id IN $ids
    OPTIONAL MATCH (c)-[:CACHE_OF]->(target)
    WITH c, collect(coalesce(target.id, 'invalid-target')) AS targets
    RETURN collect({props:properties(c), targets:targets}) AS caches }
  RETURN episodes, hits, caches`;

function cacheExpectations(evidence: CacheEvidence): { expected: HitCache[]; issues: HitCacheIssue[] } {
  const issues: HitCacheIssue[] = [];
  const events = new Map<string, DynamicsEvent[]>();
  const ids = new Set<string>();
  const keys = new Set<string>();
  for (const row of evidence.hits) {
    let decoded: unknown = null;
    try { decoded = typeof row.props["body"] === "string" ? JSON.parse(row.props["body"]) : null; }
    catch (error) {
      if (!(error instanceof SyntaxError)) throw error;
      issues.push({ code: "invalid_hit_evidence", id: String(row.props["id"]) });
      continue;
    }
    const parsed = ReceiptHit.safeParse(decoded);
    const hit = parsed.success ? parsed.data : null;
    const body = hit ? canonicalJson(hit) : "";
    if (!hit || row.props["body"] !== body || row.props["body_digest"] !== sha256(body)
      || Object.entries(hit).some(([key, value]) => key !== "attribution" && row.props[key] !== value)
      || row.props["attribution"] !== canonicalJson(hit.attribution)
      || row.targets.length !== 1 || row.targets[0] !== hit.episode_id
      || !evidence.episodes.some((e) => e.id === hit.episode_id)
      || hit.idem_key !== tupleHash([hit.namespace, hit.episode_id, hit.kind])
      || ids.has(hit.id) || keys.has(hit.idem_key)) {
      issues.push({ code: "invalid_hit_evidence", id: String(row.props["id"]) });
      continue;
    }
    ids.add(hit.id); keys.add(hit.idem_key);
    const bucket = events.get(hit.episode_id) ?? [];
    bucket.push(hit.kind === "recall_hit"
      ? { id: hit.id, at: hit.t, kind: hit.kind, kappa: hit.kappa_eff }
      : hit.kind === "outcome" ? { id: hit.id, at: hit.t, kind: hit.kind, reward: hit.reward, weight: hit.weight }
      : { id: hit.id, at: hit.t, kind: hit.kind });
    events.set(hit.episode_id, bucket);
  }
  const expected: HitCache[] = [];
  for (const episode of evidence.episodes) {
    if (!events.has(episode.id) && !evidence.caches.some((row) => row.props.episode_id === episode.id)) continue;
    const history = events.get(episode.id) ?? [];
    const state = replayDynamics({ initialMass: episode.mass, ingestedAt: episode.ingested_at, priorRewardSum: 0, priorWeight: 0 }, history);
    const ordered = [...history].sort((a, b) => a.at - b.at || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const sum = ordered.reduce((total, event) => total + (event.kind === "outcome" ? event.weight * event.reward : 0), 0);
    expected.push({ episode_id: episode.id, s: state.stability, t_last_hit: state.lastHit,
      hit_count: state.hitCount, utility_reward_sum: sum, utility_weight: state.weight,
      utility: state.utility, event_ids: state.eventIds, config_version: "g003-dynamics-v1", numeric_version: ADOPTION_NUMERIC_VERSION });
  }
  return { expected, issues };
}
function cacheMatches(row: CacheEvidence["caches"][number] | undefined, expected: HitCache): boolean {
  return !!row && row.targets.length === 1 && row.targets[0] === expected.episode_id
    && canonicalJson({ ...row.props }) === canonicalJson({ ...expected });
}

export class Store {
  private readonly driver: Driver;
  private readonly database: string;
  private readonly objects: ObjectStore;
  private writerEpoch: number | undefined;
  private readonly clock: () => number;
  private readonly embeddingProvider: EmbeddingProvider | undefined;
  private readonly tokenizers: Tokenizers;
  private readonly recallDefaultBytes: number;
  private readonly dreamLeidenAdapter: DreamLeidenAdapter | undefined;

  constructor(opts: StoreOptions, driver?: Driver) {
    this.clock = opts.clock ?? Date.now;
    this.embeddingProvider = opts.embeddingProvider;
    this.tokenizers = opts.tokenizers ?? new Map();
    this.recallDefaultBytes = z.number().int().min(0).max(1024 * 1024).parse(opts.recallDefaultBytes ?? 65536);
    this.dreamLeidenAdapter = opts.dreamLeidenAdapter;
    this.driver =
      driver ??
      neo4j.driver(opts.uri, neo4j.auth.basic(opts.user, opts.password), {
        disableLosslessIntegers: true,
      });
    this.database = opts.database ?? "neo4j";
    this.objects = new ObjectStore(
      opts.objectsRoot ?? join(homedir(), ".anamnesis", "objects"),
    );
  }

  get databaseName(): string {
    return this.database;
  }

  async claimWriterEpoch(): Promise<number> {
    const session = this.driver.session({ database: this.database });
    try {
      const epoch = await session.executeWrite(async (tx) => {
        const result = await tx.run<{ epoch: number }>(
          `MERGE (m:Meta {key: 'meta'})
           ON CREATE SET m.ingest_seq = 0, m.writer_epoch = 0
           SET m.writer_epoch = coalesce(m.writer_epoch, 0) + 1
           RETURN m.writer_epoch AS epoch`,
        );
        const record = result.records[0];
        if (!record) throw new Error("writer epoch claim returned no epoch");
        return record.get("epoch");
      });
      this.writerEpoch = epoch;
      return epoch;
    } finally {
      await session.close();
    }
  }

  async init(): Promise<void> {
    for (const stmt of SCHEMA_STATEMENTS) await this.run(stmt);

    if (this.embeddingProvider) {
      const profile = this.embeddingProvider.profile, id = embeddingProfileId(profile);
      await this.run(`MERGE (p:EmbeddingProfile {id:$id}) ON CREATE SET p.body=$body`, { id, body: canonicalJson(profile) });
      // Digest-derived identifiers only, never caller/model-name interpolation.
      await this.run(`CREATE VECTOR INDEX vec_episode_${id} IF NOT EXISTS FOR (v:Embedding_${id}) ON (v.vector)
        OPTIONS {indexConfig: {\`vector.dimensions\`: ${profile.dimensions}, \`vector.similarity_function\`: 'cosine'}}`);
    }
    await this.run(`CALL db.awaitIndexes(60)`);
    await this.withWriteTx(async (tx) => {
      const state = await tx.run<{ revision: number | null; format: string | null; events: number; legacy: number; elements: number }>(
        `MERGE (m:Meta {key:'meta'}) ON CREATE SET m.ingest_seq=0, m.conducting_arc_ready=false
         SET m.ingest_seq=m.ingest_seq
         WITH m OPTIONAL MATCH (p:PolicyAuthority {key:'installation'})
         CALL () { MATCH (e:PolicyEvent) RETURN count(e) AS events }
         CALL () { MATCH (e:Element {schema:'anamnesis.memory-policy/1'}) RETURN count(e) AS legacy }
         CALL () { MATCH (e:Element) RETURN count(e) AS elements }
         RETURN m.policy_revision AS revision,p.format AS format,events,legacy,elements`);
      const row = state.records[0]!;
      // Only a genuinely policy-empty database may bootstrap revision zero.
      // Missing/incompatible authority never overwrites an existing revision.
      const preserveLegacy = row.get("elements") > 0 && row.get("revision") === null && row.get("format") === null && row.get("events") === 0 && row.get("legacy") === 0;
      if (!preserveLegacy && row.get("revision") === null && row.get("format") === null && row.get("events") === 0 && row.get("legacy") === 0) {
        await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.policy_revision=0
          CREATE (:PolicyAuthority {key:'installation', format:'episode-source-v1'})`);
      }
      if (!preserveLegacy) {
        // Upgrade legacy selectors once. Never reset an established epoch on init.
        await tx.run(`MERGE (s:Meta {key:'extraction_selector'})
          SET s.selector_version=coalesce(s.selector_version,0)`);
        // Startup verifies a capped retained-graph snapshot; it never reconstructs
        // endpoint rows. Existing incomplete/oversized stores need explicit repair.
        const snapshot = await this.conductingSnapshotTx(tx, 10000);
        if (!snapshot.report.truncated && snapshot.dataIssues.length === 0) {
          await this.publishConductingTx(tx, snapshot.partitions);
        } else {
          await this.invalidateConductingTx(tx);
        }
      }
    });
  }

  /** Privileged server issuer, Episodes only; not semantic recall or a wire API. */
  async issueReceipt(input: IssueReceiptInput, context: InstallationContext): Promise<RecallReceipt> {
    requireInstallation(context);
    const request = IssueReceiptInput.parse(input);
    return this.withWriteTx(async (tx) => this.issueReceiptTx(tx, request, context, await this.receiptLockTx(tx)));
  }

  private async issueReceiptTx(tx: ManagedTransaction, request: z.output<typeof IssueReceiptInput>, context: InstallationContext,
    policy: PolicyState, serving?: RecallReceipt["serving"]): Promise<RecallReceipt> {
      const bodyDigest = sha256(canonicalReceiptJson(receiptBodyDigestInput({ ...request, ...(serving ? { serving } : {}) })));
      const old = await this.receiptTx(tx, request.recall_id);
      if (old) {
        await this.authorizeReceiptTx(tx, old, policy, context);
        if (receiptTime.parse(this.clock()) >= old.expires_at) throw new ReceiptError("receipt_expired");
        if (old.body_digest !== bodyDigest || old.commit_mode !== context.commit_mode) throw new ReceiptError("idempotency_conflict");
        return old;
      }
      const sourceRows = await tx.run(`MATCH (e:Element) WHERE e.id IN $ids
        OPTIONAL MATCH (e:Fact)-[:DERIVED_FROM]->(source:Episode)
        RETURN e.id AS id,coalesce(source.id,e.id) AS source`, { ids: request.primary_ids });
      if (sourceRows.records.length !== request.primary_ids.length) throw new ReceiptError("invalid_selection");
      await this.authorizeEpisodesTx(tx, [...new Set(sourceRows.records.map(row => row.get("source")))], policy);
      const createdAt = serving?.response.diagnostics.now ?? receiptTime.parse(this.clock());
      const sourceById = new Map(sourceRows.records.map(row => [row.get("id"), row.get("source")]));
      const primaries = request.primary_ids.map((id, rank) => ({ id, rank, sources: [sourceById.get(id)!] }));
      // Internal compatibility receipts do not acquire lineage authority.
      const selection = context.client_binding ? await this.lineageSelectionTx(tx, request.primary_ids) : undefined;
      const receipt = RecallReceipt.parse({ ...request, primaries, ...(serving ? { serving } : {}), format: "episode-selection-v1",
        ...(selection ? { client_binding: context.client_binding, lineage_selection: selection } : {}),
        principal: "installation", commit_mode: context.commit_mode, created_at: createdAt, expires_at: createdAt + request.receipt_ttl_ms,
        structure_revision: policy.structure_revision, policy_revision: policy.policy_revision,
        config_version: "g003-dynamics-v1", body_digest: bodyDigest,
        selection_digest: sha256(canonicalJson(selection ?? primaries.map((p) => ({ element_id: p.id, root_episode_ids: p.sources, echo_depth: 0, complete: true })))),
      });
      await tx.run(`CREATE (r:Receipt:RecallReceipt {recall_id:$id, body:$body, body_digest:$digest,
        created_at:$createdAt, expires_at:$expiresAt})`, {
        id: receipt.recall_id, body: canonicalContext(receipt), digest: bodyDigest,
        createdAt: receipt.created_at, expiresAt: receipt.expires_at,
      });
      await tx.run(`MATCH (r:RecallReceipt {recall_id:$id})
        UNWIND $primaries AS p MATCH (e:Element {id:p.id})
        CREATE (r)-[:PRIMARY {rank:p.rank}]->(e)`, { id: receipt.recall_id, primaries });
      return receipt;
  }

  private async lineageTx(tx: ManagedTransaction, episodeId: string, digest: string | null): Promise<EchoLineage> {
    const rows = await tx.run<{ body: string; props: Record<string, unknown> }>(
      `MATCH (l:EchoLineage {episode_id:$id}) RETURN l.body AS body,properties(l) AS props`, { id: episodeId });
    const row = rows.records[0];
    if (!row) throw new EpisodeLineageError("lineage_unavailable");
    const body = row.get("body"), { body: ignored, digest: retainedDigest, ...props } = row.get("props");
    const lineage = EchoLineage.parse(JSON.parse(body));
    if (lineage.episode_id !== episodeId || extractionBodyDigest(lineage) !== digest || retainedDigest !== digest
      || canonicalExtractionBody(props) !== body || canonicalExtractionBody(lineage) !== body)
      throw new EpisodeLineageError("lineage_mismatch");
    return lineage;
  }

  private async lineageSelectionTx(tx: ManagedTransaction, ids: string[]): Promise<z.infer<typeof RecallLineageSelection>> {
    const items: z.infer<typeof RecallLineageSelection> = [];
    for (const id of ids) {
      const rows = await tx.run<{ version: number | null; digest: string | null; source: string | null }>(
        `MATCH (e:Element {id:$id}) OPTIONAL MATCH (e:Fact)-[:DERIVED_FROM]->(source:Episode)
         RETURN coalesce(e.episode_digest_version,source.episode_digest_version) AS version,
           coalesce(e.lineage_digest,source.lineage_digest) AS digest,source.id AS source`, { id });
      const row = rows.records[0];
      if (!row) throw new ReceiptError("invalid_selection");
      const source = row.get("source") ?? id, version = row.get("version"), digest = row.get("digest");
      if (version === null) items.push({ element_id: id, root_episode_ids: [], echo_depth: 0, complete: false });
      else {
        if (version !== 2) throw new EpisodeLineageError("unsupported_digest_version");
        const lineage = await this.lineageTx(tx, source, digest);
        items.push({ element_id: id, root_episode_ids: lineage.root_episode_ids, echo_depth: lineage.echo_depth, complete: lineage.complete });
      }
    }
    return RecallLineageSelection.parse(items);
  }

  private async admitLineageTx(tx: ManagedTransaction, episodeId: string, input: EpisodeLineageInput,
    context: InstallationContext, now: number): Promise<EchoLineage> {
    if (!context.client_binding) throw new EpisodeLineageError("lineage_binding_mismatch");
    if (input.lineage_mode === "direct") return EchoLineage.parse({ episode_id: episodeId, lineage_mode: "direct",
      parent_recall_ids: [], context_digests: [], root_episode_ids: [episodeId], echo_depth: 0, complete: true });
    const policy = await this.receiptLockTx(tx), roots = new Set<string>(), contextDigests: string[] = [];
    let depth = 0, complete = true, count = 0;
    for (const id of input.parent_recall_ids) {
      const parent = await this.receiptTx(tx, id);
      if (!parent) throw new ReceiptError("unknown_recall");
      if (parent.client_binding !== context.client_binding) throw new EpisodeLineageError("lineage_binding_mismatch");
      if (parent.created_at >= now) throw new EpisodeLineageError("lineage_unavailable");
      // TTL governs feedback, not provenance. Retained expired receipts remain
      // usable; deletion makes new admission unknown, never changes a child.
      await this.authorizeReceiptTx(tx, parent, policy, context);
      const selection = parent.lineage_selection;
      if (!selection) throw new EpisodeLineageError("lineage_unavailable");
      if (extractionBodyDigest(selection) !== parent.selection_digest
        || canonicalExtractionBody(selection.map(item => item.element_id)) !== canonicalExtractionBody(parent.primary_ids))
        throw new EpisodeLineageError("lineage_mismatch");
      contextDigests.push(parent.selection_digest);
      for (const item of selection) {
        count++; complete &&= item.complete;
        depth = Math.max(depth, item.echo_depth);
        for (const root of item.root_episode_ids) roots.add(root);
        await this.authorizeEpisodesTx(tx, item.root_episode_ids, policy);
      }
    }
    const orderedRoots = [...roots].sort();
    return EchoLineage.parse({ episode_id: episodeId, lineage_mode: "receipts", parent_recall_ids: input.parent_recall_ids,
      context_digests: contextDigests, root_episode_ids: orderedRoots.slice(0, 16), echo_depth: Math.min(8, depth + 1),
      complete: complete && count > 0 && orderedRoots.length > 0 && orderedRoots.length <= 16 && depth < 8 });
  }

  /** Retained provenance reader for lifecycle semantic adapters. No caller role,
   * lineage, source text or time replacement crosses this custody boundary. */
  async semanticEpisode(sourceId: string, context: InstallationContext): Promise<Omit<SemanticSourceContext["episode"], "content_language">> {
    return this.extractionTx(context, (tx, policy) => this.semanticEpisodeTx(tx, sourceId, policy));
  }

  private async semanticEpisodeTx(tx: ManagedTransaction, sourceId: string, policy: PolicyState): Promise<Omit<SemanticSourceContext["episode"], "content_language">> {
    await this.authorizeEpisodesTx(tx, [z.uuidv7().parse(sourceId)], policy);
    const rows = await tx.run<{ e: ElementNode }>(`MATCH (e:Episode {id:$id}) RETURN e`, { id: sourceId });
    const p = nodeProps(rows.records[0]!.get("e")), element = p["digest_format"] == null ? decodeHistoricalElement(p) : toElement(p);
    const version = p["episode_digest_version"] ?? null;
    if (version !== null && version !== 2) throw new EpisodeLineageError("unsupported_digest_version");
    const lineage = version === 2 ? await this.lineageTx(tx, sourceId, String(p["lineage_digest"])) : null;
    if (elementDigest(element, { payloadHash: StoredHash.parse(p["payload_hash"] ?? null),
      previousRevisionKey: StoredHash.parse(p["previous_revision_key"] ?? null), format: p["digest_format"] ?? null,
      episodeDigestVersion: version, originRole: z.string().nullable().parse(p["origin_role"] ?? null),
      lineageDigest: StoredHash.parse(p["lineage_digest"] ?? null) }) !== p["digest"])
      throw new StorageContractError("revision_conflict", sourceId);
    if (lineage) await this.authorizeEpisodesTx(tx, lineage.root_episode_ids, policy);
    // Legacy seconds are instants; minutes have no equivalent and fail
    // schema validation rather than gaining invented precision. Never cast into the
    // unrelated resolved-time ABI. Coarse UTC alignment is validated explicitly.
    const precision = element.time!.precision;
    const time = SemanticResolvedTime.parse({ time_value: element.time!.value, time_utc: Date.parse(element.time!.value),
      time_precision: precision === "second" ? "instant" : precision });
    const provenance: SemanticSourceContext["episode"]["provenance"] = lineage
      ? { episode_digest_version: 2, origin_role: parseEpisodeLineage({ origin_role: p["origin_role"], lineage_mode: lineage.lineage_mode, parent_recall_ids: lineage.parent_recall_ids }).origin_role,
        lineage, lineage_digest: String(p["lineage_digest"]) }
      : { episode_digest_version: 1, origin_role: null, lineage: null, lineage_digest: null };
    return { id: sourceId, schema: z.enum(["anamnesis.original-message/1", "anamnesis.original-document/1"]).parse(element.schema),
      revision_key: receiptHash.parse(p["revision_key"]), content_digest: sha256(element.content), content: element.content,
      ingest_seq: receiptTime.positive().parse(p["ingest_seq"]), time,
      speaker: { origin_source: element.origin.source, origin_actor: element.origin.actor }, provenance };
  }

  /** Meta's write lock is held through validation, feedback/cache writes and
   * commit. Policy commands use the same fence, so policy cannot race effects. */
  private async receiptLockTx(tx: ManagedTransaction): Promise<PolicyState> {
    const rows = await tx.run<{ structure: number | null; policy: number | null; format: string | null; legacy: number }>(
      `MATCH (m:Meta {key:'meta'}) SET m.ingest_seq=m.ingest_seq
       WITH m OPTIONAL MATCH (p:PolicyAuthority {key:'installation'})
       CALL () { MATCH (e:Element {schema:'anamnesis.memory-policy/1'}) RETURN count(e) AS legacy }
       RETURN m.structure_revision AS structure,m.policy_revision AS policy,p.format AS format,legacy`);
    const row = rows.records[0];
    const revision = receiptTime.safeParse(row?.get("policy"));
    if (!row || !revision.success || row.get("format") !== "episode-source-v1" || row.get("legacy") !== 0) throw new ReceiptError("policy_unavailable");
    // Fold immutable controls, not a caller policy callback or an optional cache.
    // Validate the complete contiguous history even at an unchanged revision.
    const events = await tx.run<{ key: string; revision: number; body: string; digest: string }>(
      `MATCH (p:PolicyEvent) RETURN p.key AS key,p.revision AS revision,p.body AS body,p.body_digest AS digest ORDER BY p.revision`);
    if (events.records.length !== revision.data) throw new ReceiptError("policy_unavailable");
    const denies = new Map<string, PolicyEvent>(), revoked = new Set<string>();
    for (const [index, record] of events.records.entries()) {
      let decoded: unknown;
      try { decoded = JSON.parse(record.get("body")); }
      catch (error) { if (!(error instanceof SyntaxError)) throw error; throw new ReceiptError("policy_unavailable"); }
      const parsed = PolicyEvent.safeParse(decoded);
      if (!parsed.success) throw new ReceiptError("policy_unavailable");
      const event = parsed.data, body = policyBody(event);
      if (event.revision !== index + 1 || record.get("revision") !== event.revision
        || record.get("key") !== `${event.policy_id}:${event.action}`
        || record.get("body") !== body || record.get("digest") !== sha256(body)) throw new ReceiptError("policy_unavailable");
      const deny = denies.get(event.policy_id);
      if (event.action === "deny") {
        if (deny) throw new ReceiptError("policy_unavailable");
        denies.set(event.policy_id, event);
      } else {
        if (!deny || revoked.has(event.policy_id) || canonicalJson(policySelector(deny.selector)) !== canonicalJson(policySelector(event.selector)) || deny.scope !== event.scope) throw new ReceiptError("policy_unavailable");
        revoked.add(event.policy_id);
      }
      if (denies.size - revoked.size > 256) throw new ReceiptError("policy_unavailable");
    }
    return { structure_revision: row.get("structure"), policy_revision: revision.data, denies, revoked };
  }

  private async authorizeEpisodesTx(tx: ManagedTransaction, ids: string[], policy: PolicyState): Promise<void> {
    const rows = await tx.run<{ id: string; source: string; schema: string }>(
      `MATCH (e:Element:Episode) WHERE e.id IN $ids RETURN e.id AS id,e.origin_source AS source,e.schema AS schema`, { ids });
    if (rows.records.length !== ids.length) throw new ReceiptError("invalid_selection");
    for (const row of rows.records) {
      if (!["anamnesis.original-message/1", "anamnesis.original-document/1"].includes(row.get("schema"))) throw new ReceiptError("invalid_selection");
      for (const deny of policy.denies.values()) {
        if (policy.revoked.has(deny.policy_id)) continue;
        if ((deny.selector.episode_id === undefined || deny.selector.episode_id === row.get("id"))
          && (deny.selector.source === undefined || deny.selector.source === row.get("source"))) throw new ReceiptError("policy_denied");
      }
    }
  }

  private async authorizeReceiptTx(tx: ManagedTransaction, receipt: RecallReceipt, policy: PolicyState, context: InstallationContext): Promise<void> {
    if (receipt.principal !== context.principal) throw new ReceiptError("unauthenticated");
    // Null identifies an old, unauthorised bootstrap receipt, not revision zero.
    if (receipt.policy_revision === null || receipt.policy_revision > policy.policy_revision) throw new ReceiptError("policy_unavailable");
    await this.authorizeEpisodesTx(tx, [...new Set([...receipt.primary_ids,
      ...receipt.primaries.flatMap(item => [item.id, ...item.sources]),
      ...(receipt.serving?.response.results.flatMap(item => item.provenance.supersedes.map(prior => prior.id)) ?? [])])], policy);
  }

  async setPolicy(input: RpcPolicySetParams, context: InstallationContext): Promise<RpcPolicyResult> {
    requireInstallation(context);
    const request = RpcPolicySetParams.parse(input);
    return this.changePolicy(request.policy_id, request, context);
  }

  async revokePolicy(input: RpcPolicyRevokeParams, context: InstallationContext): Promise<RpcPolicyResult> {
    requireInstallation(context);
    return this.changePolicy(RpcPolicyRevokeParams.parse(input).policy_id, null, context);
  }

  private async changePolicy(id: string, request: RpcPolicySetParams | null, context: InstallationContext): Promise<RpcPolicyResult> {
    return this.withWriteTx(async tx => {
      const state = await this.receiptLockTx(tx);
      const old = state.denies.get(id);
      const action = request ? "deny" : "revoke";
      if (request && old && policyBody({ policy_id: old.policy_id, selector: old.selector, scope: old.scope }) !== policyBody(request)) throw new ReceiptError("idempotency_conflict");
      if (!request && !old) throw new ReceiptError("unknown_policy");
      const applied = request ? !old : !state.revoked.has(id);
      const selection = request ?? { policy_id: old!.policy_id, selector: old!.selector, scope: old!.scope };
      if (applied) {
        if (request && state.denies.size - state.revoked.size >= 256) throw new ReceiptError("resource_exhausted");
        const event = PolicyEvent.parse({ ...selection, action, principal: context.principal,
          revision: state.policy_revision + 1, created_at: this.clock() });
        const body = policyBody(event);
        await tx.run(`CREATE (:PolicyEvent {key:$key,revision:$revision,body:$body,body_digest:$digest})
          WITH 1 AS ignored MATCH (m:Meta {key:'meta'}) SET m.policy_revision=$revision`,
          { key: `${id}:${action}`, revision: event.revision, body, digest: sha256(body) });
      }
      return { ...selection, action, applied, policy_revision: state.policy_revision + (applied ? 1 : 0), evaluator: "episode-source-v1" };
    });
  }

  private async receiptTx(tx: ManagedTransaction, id: string): Promise<RecallReceipt | null> {
    const rows = await tx.run<{ body: string }>(`MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body`, { id });
    return rows.records[0] ? RecallReceipt.parse(JSON.parse(rows.records[0].get("body"))) : null;
  }

  async getReceipt(recallId: string): Promise<RecallReceipt | null> {
    const id = z.uuidv7().parse(recallId);
    const session = this.driver.session({ database: this.database });
    try { return await session.executeRead((tx) => this.receiptTx(tx, id)); }
    finally { await session.close(); }
  }

  /** The transport append deliberately does not consult current policy: even a
   * cancelled/denied publication must retain its truthful, content-free audit. */
  async recordRecallTransport(input: RecallTransportInput, context: InstallationContext) {
    requireInstallation(context);
    const request = RecallTransportInput.parse(input);
    return this.withWriteTx(async tx => {
      await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.ingest_seq=m.ingest_seq`);
      const receipt = await this.receiptTx(tx, request.recall_id);
      if (!receipt) throw new ReceiptError("unknown_recall");
      if (receipt.principal !== context.principal) throw new ReceiptError("unauthenticated");
      if (receipt.commit_mode !== context.commit_mode) throw new ReceiptError("commit_mode_mismatch");
      const prior = await tx.run<{ body: string }>(`MATCH (a:RecallTransport {recall_id:$id}) RETURN a.body AS body`, { id: request.recall_id });
      if (prior.records[0]) {
        const audit = RecallTransport.parse(JSON.parse(prior.records[0].get("body")));
        if (audit.state !== request.state) throw new ReceiptError("idempotency_conflict");
        return audit;
      }
      const audit = RecallTransport.parse({ ...request, principal: context.principal, commit_mode: context.commit_mode, created_at: this.clock(), boundary: "node-write-callback-v1" });
      const body = canonicalJson(audit);
      await tx.run(`MATCH (r:RecallReceipt {recall_id:$id})
        CREATE (a:Receipt:RecallTransport $props)-[:TRANSPORT_OF]->(r)`, {
        id: request.recall_id, props: { ...audit, operation_id: request.recall_id, body, body_digest: sha256(body) },
      });
      return audit;
    });
  }

  /** Audit only, after persisted local completion. Replays are explicit and
   * policy-checked; startup never infers delivery from receipt existence. */
  async exposeRecall(recallId: string, context: InstallationContext): Promise<{ applied: number }> {
    requireInstallation(context);
    if (context.commit_mode !== "auto") throw new ReceiptError("commit_mode_mismatch");
    const id = z.uuidv7().parse(recallId);
    return this.withWriteTx(async tx => {
      const policy = await this.receiptLockTx(tx);
      const receipt = await this.receiptTx(tx, id);
      if (!receipt) throw new ReceiptError("unknown_recall");
      if (receipt.commit_mode !== "auto") throw new ReceiptError("commit_mode_mismatch");
      await this.authorizeReceiptTx(tx, receipt, policy, context);
      const rows = await tx.run<{ body: string }>(`MATCH (a:RecallTransport {recall_id:$id}) RETURN a.body AS body`, { id });
      if (!rows.records[0]) throw new ReceiptError("invalid_selection");
      const audit = RecallTransport.parse(JSON.parse(rows.records[0].get("body")));
      if (audit.state !== "local_complete") throw new ReceiptError("invalid_selection");
      const existing = await tx.run<{ key: string }>(`MATCH (h:Hit {namespace:$id,kind:'exposure'}) RETURN h.idem_key AS key`, { id });
      const keys = new Set(existing.records.map(row => row.get("key")));
      const selected = receipt.primaries.slice(0, 3), hits: ReceiptHit[] = [];
      for (const source of new Set(selected.flatMap(item => item.sources))) {
        const key = tupleHash([id, source, "exposure"]);
        if (keys.has(key)) continue;
        hits.push(ReceiptHit.parse({ id: uuidv7(), episode_id: source, operation_id: id, namespace: id,
          idem_key: key, t: audit.created_at, kind: "exposure", kappa_eff: 0, config_version: receipt.config_version,
          attribution: selected.filter(item => item.sources.includes(source)) }));
      }
      await this.appendReceiptHitsTx(tx, hits, "RecallTransport");
      return { applied: hits.length };
    });
  }

  async getReceiptStatus(operationId: string): Promise<ReceiptStatus> {
    const id = z.uuidv7().parse(operationId);
    const rows = await this.run<{ bodyDigest: string; createdAt: number; result: string }>(
      `MATCH (r:RecallFeedback {operation_id:$id}) RETURN r.body_digest AS bodyDigest,
       r.created_at AS createdAt, r.result AS result`, { id });
    const row = rows[0];
    return row ? { state: "committed", operation_id: id, body_digest: row.bodyDigest,
      created_at: row.createdAt, result: JSON.parse(row.result) } : { state: "unknown", operation_id: id };
  }

  async commitReceipt(input: CommitReceiptInput, context: InstallationContext): Promise<CommitReceiptResult> {
    requireInstallation(context);
    if (context.commit_mode !== "receipt") throw new ReceiptError("commit_mode_mismatch");
    const parsed = CommitReceiptInput.parse(input);
    const request = { ...parsed, ...(parsed.adopted === undefined ? {} : { adopted: [...parsed.adopted].sort() }) };
    const bodyFields: Record<string, z.core.util.JSONType> = { operation_id: request.operation_id, recall_id: request.recall_id };
    if (request.adopted !== undefined) bodyFields["adopted"] = request.adopted;
    if (request.reward !== undefined) bodyFields["reward"] = request.reward;
    const body = canonicalJson(bodyFields);
    const digest = sha256(body);
    return this.withWriteTx(async (tx) => {
      const policy = await this.receiptLockTx(tx);
      const receipt = await this.receiptTx(tx, request.recall_id);
      if (!receipt) throw new ReceiptError("unknown_recall");
      const now = receiptTime.parse(this.clock());
      if (now >= receipt.expires_at) throw new ReceiptError("receipt_expired");
      await this.authorizeReceiptTx(tx, receipt, policy, context);
      if (receipt.commit_mode !== "receipt") throw new ReceiptError("commit_mode_mismatch");
      const prior = await tx.run<{ digest: string; result: string }>(
        `MATCH (r:RecallFeedback {operation_id:$id}) RETURN r.body_digest AS digest,r.result AS result`, { id: request.operation_id });
      if (prior.records[0]) {
        if (prior.records[0].get("digest") !== digest) throw new ReceiptError("idempotency_conflict");
        return { ...JSON.parse(prior.records[0].get("result")), applied: false };
      }
      const adopted = request.adopted ?? [];
      if (adopted.some((id) => !receipt.primary_ids.includes(id))) throw new ReceiptError("invalid_selection");
      const selected = [...(request.adopted ?? receipt.primary_ids)].sort();
      const outcomeDigest = request.reward === undefined ? null : sha256(canonicalJson({ reward: request.reward, selected }));
      const previousOutcome = await tx.run<{ digest: string }>(
        `MATCH (r:RecallOutcome {recall_id:$id}) RETURN r.outcome_digest AS digest`, { id: receipt.recall_id });
      const outcome = previousOutcome.records[0];
      if (outcome && outcomeDigest !== null && outcome.get("digest") !== outcomeDigest) throw new ReceiptError("idempotency_conflict");
      const existingHits = await tx.run<{ key: string }>(`MATCH (h:Hit {namespace:$id}) RETURN h.idem_key AS key`, { id: receipt.recall_id });
      const keys = new Set(existingHits.records.map((row) => row.get("key")));
      const hits: ReceiptHit[] = [];
      const coefficients = new Map<string, number>();
      for (const item of receipt.primaries.filter((item) => adopted.includes(item.id))) {
        for (const source of item.sources) coefficients.set(source, Math.min(1, (coefficients.get(source) ?? 0) + 1 / item.sources.length));
      }
      const append = (episodeId: string, kind: "recall_hit" | "outcome", value: number): void => {
        const key = tupleHash([receipt.recall_id, episodeId, kind]);
        if (keys.has(key)) return;
        const contributing = kind === "recall_hit" ? adopted : selected;
        const base = { id: uuidv7(), episode_id: episodeId, operation_id: request.operation_id,
          namespace: receipt.recall_id, idem_key: key, t: now, config_version: receipt.config_version,
          attribution: receipt.primaries.filter((item) => contributing.includes(item.id) && item.sources.includes(episodeId)) };
        hits.push(ReceiptHit.parse(kind === "recall_hit"
          ? { ...base, kind, kappa_eff: value }
          : { ...base, kind, kappa_eff: 0, reward: request.reward, weight: value }));
      };
      for (const [id, coefficient] of coefficients) append(id, "recall_hit", coefficient);
      if (request.reward !== undefined && !outcome) {
        for (const [id, weight] of attributeOutcome(receipt.primaries, selected)) append(id, "outcome", weight);
      }
      const result: CommitReceiptResult = { operation_id: request.operation_id, recall_id: receipt.recall_id,
        adopted, reward: request.reward ?? null, applied: hits.length > 0 || (outcomeDigest !== null && !outcome) };
      await tx.run(`MATCH (r:RecallReceipt {recall_id:$recallId})
        CREATE (f:Receipt:RecallFeedback {operation_id:$operationId, recall_id:$recallId,
          body:$body, body_digest:$digest, created_at:$now, result:$result})-[:FEEDBACK_OF]->(r)`,
        { recallId: receipt.recall_id, operationId: request.operation_id, body, digest, now, result: canonicalJson({ ...result }) });
      if (outcomeDigest !== null && !outcome) {
        await tx.run(`MATCH (f:RecallFeedback {operation_id:$operationId})
          CREATE (o:RecallOutcome {recall_id:$recallId, outcome_digest:$digest, reward:$reward,
            selected:$selected, created_at:$now})-[:ACCEPTED_BY]->(f)`,
          { operationId: request.operation_id, recallId: receipt.recall_id, digest: outcomeDigest, reward: request.reward, selected, now });
      }
      await this.appendReceiptHitsTx(tx, hits, "RecallFeedback");
      return result;
    });
  }

  private async appendReceiptHitsTx(tx: ManagedTransaction, hits: ReceiptHit[], producer: "RecallFeedback" | "RecallTransport"): Promise<void> {
    for (const hit of hits) {
      const body = canonicalJson(hit);
      await tx.run(`MATCH (e:Element:Episode {id:$episodeId}),(f:${producer} {operation_id:$operationId})
        CREATE (h:Hit $props)-[:HIT_OF]->(e) CREATE (h)-[:RECORDED_BY]->(f)`, {
        episodeId: hit.episode_id, operationId: hit.operation_id,
        props: { ...hit, attribution: canonicalJson(hit.attribution), body, body_digest: sha256(body) },
      });
    }
    if (hits.length) await this.rebuildHitCacheTx(tx, [...new Set(hits.map(hit => hit.episode_id))]);
  }

  async getHitCache(episodeId: string): Promise<HitCache | null> {
    const rows = await this.run<{ props: HitCache }>(`MATCH (c:HitCache {episode_id:$id}) RETURN properties(c) AS props`, { id: z.uuidv7().parse(episodeId) });
    return rows[0]?.props ?? null;
  }

  async verifyHitCache(): Promise<HitCacheVerification> {
    const rows = await this.run<CacheEvidence>(CACHE_EVIDENCE, { ids: null });
    const evidence = rows[0]!;
    const { expected, issues } = cacheExpectations(evidence);
    for (const cache of expected) {
      if (!cacheMatches(evidence.caches.find((row) => row.props.episode_id === cache.episode_id), cache)) {
        issues.push({ code: "hit_cache_mismatch", id: cache.episode_id });
      }
    }
    for (const row of evidence.caches) {
      if (!expected.some((cache) => cache.episode_id === row.props.episode_id)) issues.push({ code: "hit_cache_mismatch", id: row.props.episode_id });
    }
    return { state: "verified", hits: evidence.hits.length, issues };
  }

  async rebuildHitCache(): Promise<HitCacheRebuild> {
    return this.withWriteTx(async (tx) => {
      await this.receiptLockTx(tx);
      return this.rebuildHitCacheTx(tx, null);
    });
  }

  private async rebuildHitCacheTx(tx: ManagedTransaction, ids: string[] | null): Promise<HitCacheRebuild> {
    const rows = await tx.run<CacheEvidence>(CACHE_EVIDENCE, { ids });
    const evidence = rows.records[0]!.toObject();
    const { expected, issues } = cacheExpectations(evidence);
    if (issues.length) throw new ReceiptError("invalid_hit_evidence");
    let created = 0;
    let removed = 0;
    for (const row of evidence.caches) {
      const cache = expected.find((cache) => cache.episode_id === row.props.episode_id);
      if (!cache || !cacheMatches(row, cache)) {
        await tx.run(`MATCH (c:HitCache {episode_id:$id}) DETACH DELETE c`, { id: row.props.episode_id });
        removed++;
      }
    }
    for (const cache of expected) {
      if (cacheMatches(evidence.caches.find((row) => row.props.episode_id === cache.episode_id), cache)) continue;
      await tx.run(`MATCH (e:Element:Episode {id:$id}) CREATE (c:HitCache $props)-[:CACHE_OF]->(e)`, { id: cache.episode_id, props: cache });
      created++;
    }
    return { state: "rebuilt", hits: evidence.hits.length, created, removed };
  }

  async putElement(
    input: MemoryElementInput,
    opts: ElementWriteOptions = {},
  ): Promise<PutResult> {
    return this.putParsedElement(MemoryElement.parse(input), opts);
  }

  /** Engine ingestion uses this after its derived input schema has parsed once. */
  async putParsedElement(
    el: MemoryElement,
    opts: ElementWriteOptions = {},
  ): Promise<PutResult> {
    const payload = opts.payload
      ? await this.objects.put(
          opts.payload,
          opts.payloadMediaType ?? "application/octet-stream",
        )
      : null;
    const payloadHash = payload?.hash ?? null;
    const now = new Date().toISOString();
    const celestial = celestialOf(el.schema);
    const sourceRevision = opts.sourceRevision ?? el.origin.record;
    if (celestial === "Episode") {
      return this.withWriteTx(async (tx) => {
        const key = originKey(el.origin);
        const revisionKey = tupleHash([key, sourceRevision]);
        const retry = async (): Promise<PutResult | null> => {
          const existing = await tx.run<{
            id: string;
            digest: string;
            format: string | number | null;
            previousRevisionKey: string | null;
            version: number | null; role: string | null; lineageDigest: string | null;
          }>(
            `MATCH (e:Element:Episode { revision_key: $revisionKey })
             RETURN e.id AS id, e.digest AS digest, e.digest_format AS format,
                    e.episode_digest_version AS version,e.origin_role AS role,e.lineage_digest AS lineageDigest,
                    e.previous_revision_key AS previousRevisionKey`,
            { revisionKey },
          );
          const record = existing.records[0];
          if (!record) return null;
          const version = record.get("version");
          if (version !== null && version !== 2) throw new EpisodeLineageError("unsupported_digest_version");
          if (version === 2) {
            const lineage = await this.lineageTx(tx, record.get("id"), record.get("lineageDigest"));
            verifyLineageRetry(opts.admission?.metadata, record.get("role"), lineage);
          }
          const candidateDigest = elementDigest(el, {
            episodeDigestVersion: version, originRole: record.get("role"), lineageDigest: record.get("lineageDigest"),
            payloadHash,
            previousRevisionKey: opts.expectedPreviousRevisionKey === undefined
              ? record.get("previousRevisionKey") : opts.expectedPreviousRevisionKey,
            format: record.get("format"),
          });
          if (record.get("digest") !== candidateDigest) {
            throw new StorageContractError("revision_conflict", revisionKey);
          }
          return { id: record.get("id"), created: false };
        };
        const existing = await retry();
        if (existing) return existing;
        const head = await tx.run<{ revisionKey: string | null }>(
          `MERGE (h:OriginHead { origin_key: $originKey })
           SET h.revision_key = h.revision_key
           RETURN h.revision_key AS revisionKey`,
          { originKey: key },
        );
        const previousRevisionKey = head.records[0]!.get("revisionKey") ?? null;
        // A contender may have committed this exact revision while we waited
        // for the unique head's write lock. Recheck before attempting CREATE.
        const raced = await retry();
        if (raced) return raced;
        if (opts.expectedPreviousRevisionKey !== undefined &&
            opts.expectedPreviousRevisionKey !== previousRevisionKey) {
          throw new StorageContractError("stale_revision", revisionKey);
        }
        const prior = previousRevisionKey
          ? await tx.run<{ id: string }>(
              `MATCH (e:Element:Episode { revision_key: $revisionKey })
               RETURN e.id AS id`,
              { revisionKey: previousRevisionKey },
            )
          : null;
        const previousId = prior?.records[0]?.get("id") ?? null;
        if (previousRevisionKey !== null && previousId === null) {
          throw new StorageContractError("stale_revision", previousRevisionKey);
        }
        // One logical tick accommodates a parent issued in the same clock ms.
        const ingestedAt = receiptTime.parse(this.clock() + 1);
        let lineage: EchoLineage | undefined, metadata: EpisodeLineageInput | undefined;
        if (opts.admission) {
          requireInstallation(opts.admission.context);
          if (this.writerEpoch === undefined) throw new Error("writer_epoch_required");
          metadata = parseEpisodeLineage(opts.admission.metadata);
          lineage = await this.admitLineageTx(tx, el.id, metadata, opts.admission.context, ingestedAt);
        }
        const lineageDigest = lineage ? extractionBodyDigest(lineage) : null;
        await this.createElementTx(tx, el, payload, opts, {
          sourceRevision, revisionKey, previousRevisionKey, ingestedAt,
          ...(metadata ? { originRole: metadata.origin_role, lineageDigest: lineageDigest! } : {}),
          digest: elementDigest(el, { payloadHash, previousRevisionKey,
            ...(metadata ? { format: "episode-rfc8785-v2", episodeDigestVersion: 2, originRole: metadata.origin_role, lineageDigest } : {}) }),
        });
        if (lineage) await tx.run(`CREATE (l:EchoLineage $props)`, {
          props: { ...lineage, body: canonicalExtractionBody(lineage), digest: lineageDigest },
        });
        if (previousId) {
          await this.mergeLinkTx(tx, {
            id: uuidv7(),
            from: el.id,
            to: previousId,
            role: "INVALIDATES",
            content: "This source revision supersedes the previous revision",
            weight: 1,
          });
        }
        // Every remember contends for the single Meta node's write lock and
        // Neo4j holds it until commit, so the increment rides on the last
        // sequence-independent statement, after CREATE and originals links.
        // Topology depends on this sequence and runs under the same lock.
        // It stays inside this
        // transaction, so an aborted remember consumes no number.
        await tx.run(
          `MATCH (h:OriginHead { origin_key: $originKey })
           MATCH (e:Element:Episode { id: $id })
           SET h.revision_key = $revisionKey
           MERGE (m:Meta { key: 'meta' })
           ON CREATE SET m.ingest_seq = 0
           SET m.ingest_seq = m.ingest_seq + 1
           SET e.ingest_seq = m.ingest_seq`,
          { originKey: key, revisionKey, id: el.id },
        );
        await this.spliceTopologyTx(tx, { id: el.id, sessionKey: sessionKey(el.origin) });
        if (lineage) await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.structure_revision=coalesce(m.structure_revision,0)+1`);
        return {
          id: el.id,
          created: true,
          ...(previousId ? { invalidated: previousId } : {}),
        };
      });
    }

    return this.withWriteTx(async (tx) => {
      const existing = await tx.run<{
        id: string;
        content: string;
        schema: string;
      }>(
        `MATCH (e:Element { origin_key: $originKey })
           RETURN e.id AS id, e.content AS content, e.schema AS schema`,
        { originKey: originKey(el.origin) },
      );

      if (existing.records.length > 0) {
        const rec = existing.records[0]!;
        const oldId = rec.get("id");
        const oldContent = rec.get("content");
        if (sha256(oldContent) === sha256(el.content)) {
          return { id: oldId, created: false };
        }

        const hash8 = sha256(el.content).slice(0, 8);
        const derivedRecord = `${el.origin.record}#h${hash8}`;
        const dup = await tx.run<{ id: string }>(
          `MATCH (e:Element { origin_key: $originKey }) RETURN e.id AS id`,
          {
            originKey: originKey({ ...el.origin, record: derivedRecord }),
          },
        );
        if (dup.records.length > 0) {
          return {
            id: dup.records[0]!.get("id"),
            created: false,
            diverged: true,
          };
        }
        const divergedEl = {
          ...el,
          id: uuidv7(),
          origin: { ...el.origin, record: derivedRecord },
          properties: { ...el.properties, diverged_at: now },
        };
        await this.createElementTx(tx, divergedEl, payload, opts);
        const oldCelestial = celestialOf(rec.get("schema"));
        const invalidated =
          LINK_LATTICE.INVALIDATES.from.some((label) => label === celestial) &&
          LINK_LATTICE.INVALIDATES.to.some((label) => label === oldCelestial)
            ? oldId
            : null;
        if (!invalidated) {
          return { id: divergedEl.id, created: true, diverged: true };
        }
        await this.mergeLinkTx(tx, {
          id: uuidv7(),
          from: divergedEl.id,
          to: invalidated,
          role: "INVALIDATES",
          content:
            "Different content at the same origin was detected as a divergence",
          weight: 1,
        });
        return {
          id: divergedEl.id,
          created: true,
          diverged: true,
          invalidated,
        };
      }

      await this.createElementTx(tx, el, payload, opts);
      return { id: el.id, created: true };
    });
  }

  private async createElementTx(
    tx: ManagedTransaction,
    el: MemoryElement,
    payload: { hash: string; size: number; mediaType: string } | null,
    opts: { enqueue?: boolean; previous?: string },
    revision?: {
      sourceRevision: string;
      revisionKey: string;
      previousRevisionKey: string | null;
      ingestedAt: number;
      digest: string;
      originRole?: string;
      lineageDigest?: string;
    },
  ): Promise<void> {
    if (payload) {
      await tx.run(
        `MERGE (p:Payload { hash: $hash })
         ON CREATE SET p.size = $size, p.media_type = $mediaType`,
        { hash: payload.hash, size: payload.size, mediaType: payload.mediaType },
      );
    }
    const isEpisode = celestialOf(el.schema) === "Episode";
    const time = carriesTime(el.schema) ? el.time ?? null : null;
    await tx.run(
      `CREATE (e:${labelClause(el.schema)} {
         id: $id, schema: $schema,
         time_value: $timeValue, time_utc: $timeUtc,
         time_precision: $timePrecision,
         content: $content,
         origin_key: $originKey, session_key: $sessionKey,
         origin_source: $source, origin_session: $session,
         origin_actor: $actor, origin_record: $record,
         mass: $mass, properties: $properties,
         payload_hash: $payloadHash, digest: $digest, digest_format: $digestFormat,
         episode_digest_version: $episodeDigestVersion, origin_role: $originRole, lineage_digest: $lineageDigest,
         topology_version: $topologyVersion, topology_previous_record: $previous,
         source_revision: $sourceRevision, revision_key: $revisionKey,
         previous_revision_key: $previousRevisionKey,
         ingest_seq: null, ingested_at: $ingestedAt
       })`,
      {
        originKey: originKey(el.origin),
        sessionKey: isEpisode ? sessionKey(el.origin) : null,
        id: el.id,
        schema: el.schema,
        timeValue: time?.value ?? null,
        timeUtc: time ? toUtc(time.value) : null,
        timePrecision: time?.precision ?? null,
        content: el.content,
        source: el.origin.source,
        session: el.origin.session,
        actor: el.origin.actor,
        record: el.origin.record,
        mass: el.mass,
        properties: JSON.stringify(el.properties),
        payloadHash: payload?.hash ?? null,
        digest: revision?.digest ?? elementDigest(el, { payloadHash: payload?.hash ?? null }),
        digestFormat: revision?.lineageDigest ? "episode-rfc8785-v2" : CANONICAL_DIGEST,
        episodeDigestVersion: revision?.lineageDigest ? 2 : null,
        originRole: revision?.originRole ?? null,
        lineageDigest: revision?.lineageDigest ?? null,
        topologyVersion: isEpisode ? 1 : null,
        previous: isEpisode ? opts.previous ?? null : null,
        sourceRevision: revision?.sourceRevision ?? null,
        revisionKey: revision?.revisionKey ?? null,
        previousRevisionKey: revision?.previousRevisionKey ?? null,
        ingestedAt: revision?.ingestedAt ?? Date.now(),
      },
    );
    if (payload) {
      await tx.run(
        `MATCH (e:Element:Episode { id: $id }), (p:Payload { hash: $hash })
         MERGE (e)-[:HAS_PAYLOAD]->(p)`,
        { id: el.id, hash: payload.hash },
      );
    }
    if (opts.enqueue) {
      await tx.run(
        `MATCH (e:Element { id: $id })
         CREATE (o:Outbox { element_id: $id, enqueued_at: $now,
                            processed_at: null })-[:OF]->(e)`,
        { id: el.id, now: new Date().toISOString() },
      );
    }
  }

  /** Meta's sequence lock is held until commit, serializing cache splices. */
  private async spliceTopologyTx(tx: ManagedTransaction, episode: { id: string; sessionKey: string }): Promise<void> {
    const { id, sessionKey } = episode;
    const predecessors = await tx.run<{ id: string }>(
      `MATCH (e:Element:Episode {id: $id}), (p:Element:Episode)
       WHERE p.session_key = e.session_key
         AND (p.time_utc < e.time_utc OR (p.time_utc = e.time_utc AND p.ingest_seq < e.ingest_seq))
       RETURN p.id AS id ORDER BY p.time_utc DESC, p.ingest_seq DESC LIMIT 1`, { id });
    const successors = await tx.run<{ id: string; previousRecord: string | null }>(
      `MATCH (e:Element:Episode {id: $id}), (s:Element:Episode)
       WHERE s.session_key = e.session_key
         AND (s.time_utc > e.time_utc OR (s.time_utc = e.time_utc AND s.ingest_seq > e.ingest_seq))
       RETURN s.id AS id, s.topology_previous_record AS previousRecord
       ORDER BY s.time_utc, s.ingest_seq LIMIT 1`, { id });
    const predecessor = predecessors.records[0]?.get("id") ?? null;
    const successor = successors.records[0];
    if (successor && successor.get("previousRecord") === null) {
      const removed = await tx.run<{ id: string }>(
        `MATCH (p:Element:Episode {id: $predecessor})-[l:NEXT_EPISODE]->(s:Element:Episode {id: $successor})
         RETURN l.id AS id`, { predecessor, successor: successor.get("id") });
      await this.deleteTopologyLinksTx(tx, removed.records.map(row => row.get("id")));
      await this.mergeTopologyTx(tx, { from: id, to: successor.get("id"), sessionKey });
    }
    const parents = await tx.run<{ id: string }>(
      `MATCH (e:Element:Episode {id: $id}), (p:Element:Episode)
       WHERE (e.topology_previous_record IS NULL AND p.id = $predecessor)
         OR (e.topology_previous_record IS NOT NULL AND p.session_key = e.session_key
             AND p.origin_record = e.topology_previous_record AND p.ingest_seq < e.ingest_seq)
       RETURN p.id AS id`, { id, predecessor });
    for (const parent of parents.records) {
      await this.mergeTopologyTx(tx, { from: parent.get("id"), to: id, sessionKey });
    }
  }

  private async mergeTopologyTx(tx: ManagedTransaction, edge: { from: string; to: string; sessionKey: string }): Promise<void> {
    const result = await tx.run<{ id: string }>(
      `MATCH (p:Element:Episode {id: $from}), (e:Element:Episode {id: $to})
       MERGE (p)-[l:NEXT_EPISODE]->(e)
       ON CREATE SET l.id = $linkId, l.idem_key = $idemKey,
         l.content = CASE WHEN e.topology_previous_record IS NULL
           THEN 'This is the next episode in the same session'
           ELSE 'This episode follows the explicitly selected parent record' END,
         l.weight = 1.0
       RETURN l.id AS id`,
      { ...edge, linkId: uuidv7(), idemKey: tupleHash([edge.sessionKey, edge.from, edge.to]) });
    if (result.summary.counters.updates().relationshipsCreated > 0) {
      await this.appendConductingTx(tx, "NEXT_EPISODE", result.records[0]!.get("id"));
    }
  }

  /** Only cache links are replaced. Legacy explicit parents were not persisted,
   * so rebuilding unmarked rows would invent provenance; require journal recovery.
   */
  async rebuildTopology(): Promise<void> {
    await this.withWriteTx(async (tx) => {
      await tx.run(`MERGE (m:Meta {key: 'meta'}) ON CREATE SET m.ingest_seq = 0
        SET m.ingest_seq = m.ingest_seq`);
      const result = await tx.run<TopologyRow>(TOPOLOGY_QUERY);
      const rows = topologyExpectations(recordsToObjects(result.records));
      for (const row of rows) {
        if (row.version !== 1) throw new StorageContractError("unsupported_topology_format", row.id);
      }
      const removed = await tx.run<{ id: string }>(`MATCH ()-[l:NEXT_EPISODE]->() RETURN l.id AS id`);
      await this.deleteTopologyLinksTx(tx, removed.records.map(row => row.get("id")));
      for (const row of rows) {
        for (const parent of row.parents) {
          await this.mergeTopologyTx(tx, { from: parent, to: row.id, sessionKey: row.sessionKey });
        }
      }
    });
  }

  async putLink(input: MemoryLinkInput): Promise<MemoryLink> {
    const link = MemoryLink.parse(input);
    return this.withWriteTx(async (tx) => {
      const rows = await this.mergeLinkTx(tx, link);
      if (rows.length === 0) {
        throw new Error(
          `link rejected (endpoints missing or lattice violation): ` +
            `${link.from} -[${link.role}]-> ${link.to}`,
        );
      }
      return { ...link, id: rows[0]!.id };
    });
  }

  private async withReadTx<Result>(
    work: (tx: ManagedTransaction) => Promise<Result>,
  ): Promise<Result> {
    const session = this.driver.session({ database: this.database });
    try { return await session.executeRead(work); }
    finally { await session.close(); }
  }

  private async withWriteTx<Result>(
    work: (tx: ManagedTransaction) => Promise<Result>,
  ): Promise<Result> {
    const session = this.driver.session({ database: this.database });
    try {
      return await session.executeWrite(async (tx) => {
        const epoch = this.writerEpoch;
        if (epoch === undefined) {
          // Compatibility Store writers also serialize physical/cache changes.
          await tx.run(`MERGE (m:Meta {key:'meta'}) ON CREATE SET m.ingest_seq=0
            SET m.conducting_write_lock=true REMOVE m.conducting_write_lock`);
        } else {
          // Acquire Meta's write lock before reading the epoch, and hold it
          // through commit against concurrent claims.
          const fence = await tx.run<{ epoch: number }>(
            `MATCH (m:Meta {key: 'meta'})
             SET m.writer_epoch = m.writer_epoch
             RETURN m.writer_epoch AS epoch`,
          );
          if (fence.records[0]?.get("epoch") !== epoch) {
            throw new Error("stale_writer_epoch");
          }
        }
        return work(tx);
      });
    } finally {
      await session.close();
    }
  }

  private async mergeLinkTx(
    tx: ManagedTransaction,
    link: MemoryLink,
  ): Promise<{ id: string }[]> {
    const lattice = LINK_LATTICE[link.role];
    // INVALIDATES copies the seek fields of docs/01 §5 so valid(T) resolves
    // without expanding an incoming adjacency list. An Episode→Episode
    // revision edge belongs to the originals layer and keys without content.
    const seek =
      link.role === "INVALIDATES"
        ? ", target_id: b.id, effective_time_utc: a.time_utc"
        : "";
    const res = await tx.run<{ id: string }>(
      `MATCH (a:Element { id: $from }), (b:Element { id: $to })
       WHERE any(x IN labels(a) WHERE x IN $fromLabels)
         AND any(x IN labels(b) WHERE x IN $toLabels)
       WITH a, b, CASE WHEN $originalsLayer AND a:Episode AND b:Episode
                    THEN $originalsIdemKey ELSE $derivedIdemKey END AS key
       MERGE (a)-[l:${link.role} { idem_key: key }]->(b)
       ON CREATE SET l += { id: $id, content: $content, weight: $weight${seek} },
         l.generation = CASE WHEN $role = 'NEXT_EPISODE' OR (a:Episode AND b:Episode)
           THEN null ELSE coalesce(a.generation,b.generation) END
       RETURN l.id AS id`,
      {
        from: link.from,
        to: link.to,
        fromLabels: [...lattice.from],
        toLabels: [...lattice.to],
        originalsLayer: link.role === "INVALIDATES",
        originalsIdemKey: linkIdemKey(link, true),
        derivedIdemKey: linkIdemKey(link, false),
        id: link.id,
        content: link.content,
        weight: link.weight,
        role: link.role,
      },
    );
    if (res.summary.counters.updates().relationshipsCreated > 0 && CONDUCTING_ROLES.some(role => role === link.role)) {
      await this.appendConductingTx(tx, link.role, res.records[0]!.get("id"));
    }
    return recordsToObjects(res.records);
  }

  private async conductingRevisionTx(tx: ManagedTransaction): Promise<void> {
    await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.conducting_arc_revision=coalesce(m.conducting_arc_revision,0)+1`);
  }

  private async invalidateConductingTx(tx: ManagedTransaction, maxItems = 10000): Promise<void> {
    const changed = await tx.run(`MATCH (m:Meta {key:'meta'})
      WHERE m.conducting_arc_ready IS NULL OR m.conducting_arc_ready <> false OR m.conducting_arc_revision IS NULL
      SET m.conducting_arc_ready=false,m.conducting_arc_revision=coalesce(m.conducting_arc_revision,0)+1`);
    const coverage = await tx.run(`MATCH (c:ConductingArcCoverage) WHERE c.state <> 'UNAVAILABLE' OR c.state IS NULL
      WITH c LIMIT $limit SET c.state='UNAVAILABLE'`, {limit:neo4j.int(maxItems)});
    if (!changed.summary.counters.containsUpdates() && coverage.summary.counters.containsUpdates()) await this.conductingRevisionTx(tx);
  }

  private async publishConductingTx(tx: ManagedTransaction, partitions: ConductingPartition[]): Promise<void> {
    const changed = await tx.run(`UNWIND $partitions AS p
      MERGE (c:ConductingArcCoverage {stream:p.stream,generation:p.generation})
      WITH c WHERE c.state IS NULL OR c.state <> 'COMPLETE' SET c.state='COMPLETE'`, { partitions });
    const gate = await tx.run(`MATCH (m:Meta {key:'meta'})
      WHERE m.conducting_arc_ready IS NULL OR m.conducting_arc_ready <> true OR m.conducting_arc_revision IS NULL
      SET m.conducting_arc_ready=true`);
    if (changed.summary.counters.containsUpdates() || gate.summary.counters.containsUpdates()) await this.conductingRevisionTx(tx);
  }

  /** Called only for a newly created physical link, inside its writer transaction.
   * Duplicates do not rewrite, repair, or replace either cache or coverage rows. */
  private async appendConductingTx(tx: ManagedTransaction, role: LinkRole, id: string): Promise<void> {
    const result = await tx.run<PhysicalConductor>(`MATCH (a)-[l:${role}]->(b) WHERE l.id=$id
      OPTIONAL MATCH (g:Generation {stream:'community',generation:l.generation})
      RETURN a.id AS source_id,b.id AS peer_id,l.id AS link_id,type(l) AS role,l.generation AS generation,
        CASE WHEN type(l)='HAS_MEMBER' THEN coalesce(g.source_extraction_generation,a.source_extraction_generation) ELSE null END AS source_extraction_generation,
        a.id AS from,b.id AS to,g.source_extraction_generation AS registry_source`, { id });
    const link = result.records[0]!.toObject();
    if (role === "HAS_MEMBER" && link.source_extraction_generation === null) throw new Error("conducting_source_generation_missing");
    // Per-role seeks also reject cross-role identity reuse (even disjoint endpoints).
    for (const other of CONDUCTING_ROLES) {
      if (other === role) continue;
      const collision = await tx.run(`MATCH ()-[l:${other}]->() WHERE l.id=$id RETURN l.id LIMIT 1`, { id });
      if (collision.records.length) throw new Error("conducting_link_id_collision");
    }
    const partition = conductingPartition(role, link.generation);
    const coverage = await tx.run(`MATCH (c:ConductingArcCoverage {stream:$stream,generation:$generation}) RETURN c.state AS state`, partition);
    if (!coverage.records.length) {
      // A missing partition is empty only if this is its first retained link.
      // Never infer that fact from the serving selector, policy or the ready bit.
      const peers = await tx.run(`MATCH ()-[l]->() WHERE type(l) IN $roles
        AND coalesce(l.generation,0)=$generation AND l.id <> $id RETURN l.id LIMIT 1`, {
        roles: partition.stream === "cache" ? ["NEXT_EPISODE"] : partition.stream === "community" ? ["HAS_MEMBER"] : ["MENTIONS","RELATES_TO","DERIVED_FROM"],
        generation: partition.generation, id,
      });
      await tx.run(`CREATE (:ConductingArcCoverage {stream:$stream,generation:$generation,state:$state})`, {
        ...partition, state: peers.records.length ? "UNAVAILABLE" : "COMPLETE",
      });
      if (peers.records.length) await this.invalidateConductingTx(tx);
    } else if (coverage.records[0]!.get("state") !== "COMPLETE") {
      await this.invalidateConductingTx(tx);
    }
    const rows = [...new Set([link.from,link.to])].map(source => ({ source_id:source,peer_id:source === link.from ? link.to : link.from,
      link_id:link.link_id,role:link.role,generation:link.generation,source_extraction_generation:link.source_extraction_generation }));
    await tx.run(`UNWIND $rows AS row CREATE (a:ConductingArc) SET a=row`, { rows });
    await this.conductingRevisionTx(tx);
  }

  /** The only supported physical deletion today is topology replacement.
   * Future generation deletion/GC must use the same endpoint + HubArc removal
   * transaction and retire coverage only after its last physical/cache row. */
  private async deleteTopologyLinksTx(tx: ManagedTransaction, ids: string[]): Promise<void> {
    if (!ids.length) return;
    await tx.run(`UNWIND $ids AS id MATCH (a:ConductingArc {link_id:id}) DELETE a`, { ids });
    await tx.run(`UNWIND $ids AS id MATCH (a:HubArc {link_id:id}) DELETE a`, { ids });
    await tx.run(`UNWIND $ids AS id MATCH ()-[l:NEXT_EPISODE]->() WHERE l.id=id DELETE l`, { ids });
    await this.conductingRevisionTx(tx);
  }

  /** Maintenance/startup scan: each retained input collection is capped before
   * collection, not the total scan work (filters may inspect unrelated rows).
   * No serving request calls this physical scan. */
  private async conductingSnapshotTx(tx: ManagedTransaction, maxItems: number) {
    const result = await tx.run(`
      CALL () { MATCH (a)-[l]->(b) WHERE type(l) IN $roles
        WITH a,l,b LIMIT $limit
        OPTIONAL MATCH (g:Generation {stream:'community',generation:l.generation})
        RETURN collect({source_id:a.id,peer_id:b.id,from:a.id,to:b.id,link_id:l.id,role:type(l),generation:l.generation,
          source_extraction_generation:CASE WHEN type(l)='HAS_MEMBER' THEN coalesce(g.source_extraction_generation,a.source_extraction_generation) ELSE null END,
          registry_source:g.source_extraction_generation}) AS links }
      CALL () { MATCH (a:ConductingArc) WITH a LIMIT $limit
        RETURN collect(a { .source_id,.peer_id,.link_id,.role,.generation,.source_extraction_generation }) AS arcs }
      CALL () { MATCH (c:ConductingArcCoverage) WITH c LIMIT $limit RETURN collect(c { .stream,.generation,.state }) AS coverage }
      CALL () { MATCH (g:Generation) WHERE g.stream IN ['extraction','community'] WITH g LIMIT $limit
        RETURN collect(g { .stream,.generation }) AS generations }
      CALL () { MATCH (g:ExtractionGeneration) WITH g LIMIT $limit RETURN collect({stream:'extraction',generation:g.id}) AS extraction }
      MATCH (m:Meta {key:'meta'}) RETURN links,arcs,coverage,generations,extraction,
        m.conducting_arc_ready AS ready,m.conducting_arc_revision AS revision`, { roles: [...CONDUCTING_ROLES], limit:neo4j.int(maxItems+1) });
    const record = result.records[0]!;
    const links = record.get("links") as PhysicalConductor[], arcs = record.get("arcs") as ConductingArcRow[];
    const coverage = record.get("coverage") as ConductingPartition[], generations = record.get("generations") as ConductingPartition[], extraction = record.get("extraction") as ConductingPartition[];
    const truncated = [links,arcs,coverage,generations,extraction].some(rows => rows.length > maxItems);
    const partitions = [...new Map([{stream:"cache",generation:0},...generations,...extraction,...coverage,
      ...links.map(link => conductingPartition(link.role,link.generation))].map(p => [JSON.stringify([p.stream,p.generation]),{stream:p.stream,generation:p.generation}])).values()];
    const expected = new Map<string,ConductingArcRow>(), dataIssues: string[] = [], violations: string[] = [], ids = new Set<string>();
    for (const link of links) {
      if (!z.uuid().safeParse(link.link_id).success || !z.uuid().safeParse(link.from).success || !z.uuid().safeParse(link.to).success
        || (link.role === "HAS_MEMBER" && link.source_extraction_generation === null)) dataIssues.push(`invalid-physical:${link.link_id}`);
      if (ids.has(link.link_id)) dataIssues.push(`duplicate-link-id:${link.link_id}`);
      ids.add(link.link_id);
      if (link.from === link.to) violations.push(`self-link:${link.link_id}`);
      for (const source of new Set([link.from,link.to])) {
        const row = {source_id:source,peer_id:source===link.from?link.to:link.from,link_id:link.link_id,role:link.role,
          generation:link.generation,source_extraction_generation:link.source_extraction_generation};
        expected.set(arcIdentity(row),row);
      }
    }
    const actual = new Map(arcs.map(row => [arcIdentity(row),row]));
    for (const [key,row] of expected) {
      if (!actual.has(key)) dataIssues.push(`missing-row:${key}`);
      else if (arcTuple(actual.get(key)!) !== arcTuple(row)) dataIssues.push(`mismatched-row:${key}`);
    }
    for (const row of arcs) if (!expected.has(arcIdentity(row))) dataIssues.push(`stale-row:${arcIdentity(row)}`);
    if (actual.size !== arcs.length) dataIssues.push("duplicate-endpoint-identity");
    const issues = [...dataIssues,...violations];
    for (const partition of partitions) if (!coverage.some(c => c.stream===partition.stream && c.generation===partition.generation && c.state==="COMPLETE")) {
      issues.push(`coverage-incomplete:${JSON.stringify(partition)}`);
    }
    if (truncated) issues.push("maintenance_limit_exceeded");
    const report: ConductingArcVerification = {ready:record.get("ready")===true,revision:record.get("revision"),truncated,
      physical_links:links.length,endpoint_rows:arcs.length,partitions:coverage,issues};
    return {report,partitions,expected:[...expected.values()],dataIssues};
  }

  /** Read-only diagnostic scan. It never repairs data or certifies publication;
   * use checkConductingArcs to fence detection and persist an unavailable gate. */
  async verifyConductingArcs(options: { maxItems?: number } = {}): Promise<ConductingArcVerification> {
    const {maxItems} = ConductingMaintenanceOptions.parse(options);
    return this.withReadTx(async tx => (await this.conductingSnapshotTx(tx,maxItems)).report);
  }

  async checkConductingArcs(options: { maxItems?: number } = {}): Promise<ConductingArcVerification> {
    const {maxItems} = ConductingMaintenanceOptions.parse(options);
    if (this.writerEpoch === undefined) throw new Error("writer_epoch_required");
    return this.withWriteTx(async tx => {
      const snapshot = await this.conductingSnapshotTx(tx,maxItems);
      if (snapshot.report.issues.length) await this.invalidateConductingTx(tx);
      return (await this.conductingSnapshotTx(tx,maxItems)).report;
    });
  }

  /** Atomic, collection-capped operational rebuild; total scan work is not bounded.
   * UNAVAILABLE commits first: interruption, overflow and invalid authority leave
   * PPR closed. A subsequent explicit call can resume by reconstructing afresh. */
  async rebuildConductingArcs(options: { maxItems?: number } = {}): Promise<ConductingArcVerification> {
    const {maxItems} = ConductingMaintenanceOptions.parse(options);
    if (this.writerEpoch === undefined) throw new Error("writer_epoch_required");
    await this.withWriteTx(tx => this.invalidateConductingTx(tx,maxItems));
    return this.withWriteTx(async tx => {
      const snapshot = await this.conductingSnapshotTx(tx,maxItems);
      if (snapshot.report.truncated || snapshot.expected.length > maxItems) throw new Error("maintenance_limit_exceeded");
      if (snapshot.dataIssues.some(issue => issue.startsWith("invalid-physical:") || issue.startsWith("duplicate-link-id:"))) throw new Error("conducting_physical_invalid");
      await tx.run(`MATCH (a:ConductingArc) DELETE a`);
      await tx.run(`UNWIND $rows AS row CREATE (a:ConductingArc) SET a=row`, {rows:snapshot.expected});
      const verified = await this.conductingSnapshotTx(tx,maxItems);
      if (verified.dataIssues.length || verified.report.truncated) throw new Error("conducting_rebuild_mismatch");
      await this.publishConductingTx(tx,verified.partitions);
      return (await this.conductingSnapshotTx(tx,maxItems)).report;
    });
  }

  /** Bounded physical ConductingArc probe. Raw rows are ordered and capped before
   * any serving predicate; unavailable coverage is never treated as degree zero. */
  /** Authenticated, writer-fenced authority inventory. Every collection is
   * independently capped; overflow is a refusal, never an incomplete snapshot. */
  async authoritySnapshot(options: { maxItems?: number } = {}, context?: InstallationContext): Promise<AuthoritySnapshot> {
    requireInstallation(context!);
    const maxItems = z.number().int().min(1).max(20000).default(10000).parse(options.maxItems);
    if (this.writerEpoch === undefined) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "writer_epoch_required");
    return this.withWriteTx(async tx => {
      const policy = await this.receiptLockTx(tx);
      const result = await tx.run(`
        CALL () { MATCH (e:Element) WITH e ORDER BY e.id LIMIT $limit RETURN collect(e.id) AS members }
        CALL () { MATCH (g:Generation) WHERE g.stream IN ['extraction','community'] WITH g ORDER BY g.stream,g.generation LIMIT $limit RETURN collect(g.generation) AS generations }
        CALL () { MATCH (m:Meta {key:'meta'}) RETURN m.ingest_seq AS ingest_seq,coalesce(m.structure_revision,0) AS structure_revision }
        CALL () { MATCH (p:PolicyAuthority {key:'installation'}) RETURN p.revision AS policy_revision }
        CALL () { MATCH (a)-[l]->(b) WHERE type(l) IN $roles WITH a,l,b ORDER BY l.id LIMIT $limit RETURN collect({id:l.id,from:a.id,to:b.id,role:CASE WHEN type(l)='DERIVED_FROM' THEN 'DERIVED_FROM' ELSE 'ConductingArc' END}) AS links }
        CALL () { MATCH (a:Element)-[l:INVALIDATES]->(b:Element) WITH a,l,b ORDER BY l.id LIMIT $limit RETURN collect({id:l.id,source_hash:l.source_hash,outcome_hash:l.outcome_hash}) AS invalidation }
        CALL () { MATCH (e:Element:Episode) WITH e ORDER BY e.id LIMIT $limit RETURN collect(e.source_hash) AS sources }
        RETURN members,generations,ingest_seq,structure_revision,policy_revision,links,invalidation,sources`, { roles: [...CONDUCTING_ROLES], limit: neo4j.int(maxItems + 1) });
      const row = result.records[0]; if (!row) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "snapshot query returned no record");
      const count = (name: string) => (row.get(name) as unknown[]).length;
      for (const name of ["members","generations","links","invalidation","sources"]) if (count(name) > maxItems) throw new AuthoritySnapshotError("authority_snapshot_limit_exceeded", name);
      const members = (row.get("members") as string[]).filter((v): v is string => typeof v === "string").sort();
      const generations = (row.get("generations") as unknown[]).filter((v): v is number => typeof v === "number").sort((a,b) => a-b);
      const links = row.get("links") as AuthoritySnapshot["physical_links"];
      const invalidation = row.get("invalidation") as unknown[];
      const rawSources = row.get("sources") as unknown[];
      if (!members.length || !members.every((v,i) => i === 0 || v > members[i-1]!)) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "member identity inventory is incomplete");
      if (!rawSources.every(v => typeof v === "string" && /^[0-9a-f]{64}$/.test(v))) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "source hash evidence is incomplete");
      if (!invalidation.every(v => v && typeof v === "object" && typeof (v as Record<string, unknown>).id === "string" && typeof (v as Record<string, unknown>).source_hash === "string" && /^[0-9a-f]{64}$/.test((v as Record<string, unknown>).source_hash as string) && typeof (v as Record<string, unknown>).outcome_hash === "string" && /^[0-9a-f]{64}$/.test((v as Record<string, unknown>).outcome_hash as string))) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "invalidation hash evidence is incomplete");
      const sources = [...rawSources as string[]].sort();
      return { members, retained_generations: generations, coverage: { ingest_seq: Number(row.get("ingest_seq")), structure_revision: Number(row.get("structure_revision")), policy_revision: policy.policy_revision }, physical_links: links, invalidation_evidence: invalidation as AuthoritySnapshot["invalidation_evidence"], source_hashes: sources };
    });
  }

  async probeConductingArcs(sourceId: string, options: { limit?: number } = {}, context?: InstallationContext): Promise<ConductingArcProbe> {
    requireInstallation(context!);
    z.strictObject({ limit: z.literal(256).optional() }).parse(options);
    const source = z.uuidv7().parse(sourceId);
    return this.withWriteTx(async tx => {
      const policy = await this.receiptLockTx(tx);
      await this.graphPinTx(tx, policy, this.clock());
      await this.authorizeEpisodesTx(tx, [source], policy);
      const rows = await this.graphRawProbeTx(tx, source);
      return { source_id: source, count: rows.length, saturated: rows.length === 256, coverage: "complete" };
    });
  }

  private async graphPinTx(tx: ManagedTransaction, policy: PolicyState, T: number): Promise<GraphEnvelope["pin"]> {
    const gate = await tx.run(`MATCH (m:Meta {key:'meta'}) OPTIONAL MATCH (s:Meta {key:'extraction_selector'})
      RETURN m.conducting_arc_ready AS ready,m.conducting_arc_revision AS revision,s.generation_id AS generation`);
    const row = gate.records[0];
    if (row?.get("ready") !== true || !receiptTime.safeParse(row.get("revision")).success) throw new GraphAccessError("degree_probe_unavailable");
    const generationId = z.uuidv7().safeParse(row.get("generation"));
    if (!generationId.success) throw new GraphAccessError("degree_probe_unavailable");
    const generation = await this.extractionRecordTx(tx, "ExtractionGeneration", generationId.data, Generation);
    const coverage = await tx.run(`UNWIND ['episodes','active_extraction'] AS partition
      MATCH (c:ExtractionCoverage {key:$id+':'+partition}) RETURN partition,c.body AS body`, { id: generation.id });
    if (generation.state !== "active" || coverage.records.length !== 2) throw new GraphAccessError("degree_probe_unavailable");
    const values = coverage.records.map(record => {
      const value = Coverage.parse(JSON.parse(record.get("body")));
      if (value.generation_id !== generation.id || value.partition !== record.get("partition") || value.covered_ingest_seq < value.required_ingest_seq || value.covered_ingest_seq < generation.covered_ingest_seq) throw new GraphAccessError("degree_probe_unavailable");
      return value;
    });
    return { policy_revision: policy.policy_revision, generation_id: generation.id, coverage_revision: row.get("revision"),
      covered_ingest_seq: Math.min(...values.map(value => value.covered_ingest_seq)), T };
  }

  private async graphRawProbeTx(tx: ManagedTransaction, source: string): Promise<ConductingArcRow[]> {
    // Neo4j 5.26 requires the full composite order to stream this unique seek;
    // source equality makes it equivalent to link-ID order. Force SEEK so small
    // mixed stores cannot select an ordered full-index scan. Reject Top/Sort.
    const query = `MATCH (arc:ConductingArc {source_id:$source}) USING INDEX SEEK arc:ConductingArc(source_id,link_id)
      WHERE arc.link_id > '' RETURN arc.source_id AS source_id,arc.link_id AS link_id,arc.peer_id AS peer_id,
      arc.role AS role,arc.generation AS generation,arc.source_extraction_generation AS source_extraction_generation
      ORDER BY arc.source_id ASC, arc.link_id ASC LIMIT 256`;
    const planned = await tx.run(`EXPLAIN ${query}`, { source });
    const operators: Exclude<typeof planned.summary.plan, false>[] = [];
    const visit = (plan: typeof planned.summary.plan) => { if (plan) { operators.push(plan); for (const child of plan.children ?? []) visit(child); } };
    visit(planned.summary.plan);
    const limit = operators.find(p => /^Limit(?:@|$)/.test(p.operatorType));
    const seek = limit?.children?.[0];
    if (limit?.arguments["Details"] !== "256" || limit.children?.length !== 1
      || !seek || !/^NodeUniqueIndexSeek(?:@|$)/.test(seek.operatorType)
      || seek.arguments["Order"] !== "arc.source_id ASC, arc.link_id ASC"
      || operators.some(p => /Top|Sort|Scan|Expand/.test(p.operatorType))) throw new GraphAccessError("ordered_probe_unavailable");
    const rows = await tx.run<ConductingArcRow>(query, { source });
    return rows.records.map(record => record.toObject());
  }

  async graphEnvelope(seedIds: string[], options: { T?: number; maxNodes?: number; maxArcs?: number } = {}, context?: InstallationContext): Promise<GraphEnvelope> {
    requireInstallation(context!);
    const seeds = [...new Set(z.array(z.uuidv7()).min(1).max(128).parse(seedIds))].sort();
    const request = z.strictObject({ T: receiptTime.optional(), maxNodes: z.number().int().min(1).max(2000).optional(), maxArcs: z.number().int().min(0).max(20000).optional() }).parse(options);
    const maxNodes = request.maxNodes ?? 2000, maxArcs = request.maxArcs ?? 20000, T = request.T ?? this.clock();
    return this.withWriteTx(async tx => {
      // Same Meta fence as policy/generation writes: no torn policy or selector.
      const policy = await this.receiptLockTx(tx), pin = await this.graphPinTx(tx, policy, T);
      const probes: ConductingArcProbe[] = [], eligible: ConductingArcRow[] = [], ids = new Set<string>();
      let staleTopology = false;
      const allowed = async (id: string): Promise<boolean> => {
        const result = await tx.run(`MATCH (e:Element {id:$id}) RETURN e.schema AS schema,e.time_utc AS time,e.origin_source AS source`, { id });
        const e = result.records[0];
        // Derived source/witness authority is not yet materialized by this
        // runtime. Unknown/derived nodes must not inherit Episode permission.
        if (!e || !["anamnesis.original-message/1","anamnesis.original-document/1"].includes(e.get("schema")) || typeof e.get("time") !== "string" || !(Date.parse(e.get("time")) <= T)) return false;
        return ![...policy.denies.values()].some(deny => !policy.revoked.has(deny.policy_id)
          && (deny.selector.episode_id === undefined || deny.selector.episode_id === id)
          && (deny.selector.source === undefined || deny.selector.source === e.get("source")));
      };
      for (const source of seeds) {
        if (!await allowed(source)) continue;
        ids.add(source);
        const rows = await this.graphRawProbeTx(tx, source);
        probes.push({ source_id: source, count: rows.length, saturated: rows.length === 256, coverage: "complete" });
        // A saturated raw probe cannot stand in for a qualified hub shortlist.
        if (rows.length === 256) continue;
        for (const row of rows) {
          if (row.role !== "NEXT_EPISODE") continue;
          if (row.generation !== null || row.source_extraction_generation !== null) { staleTopology = true; continue; }
          const physical = await tx.run(`MATCH (a)-[l:NEXT_EPISODE]->(b) USING INDEX l:NEXT_EPISODE(id)
            WHERE l.id=$id RETURN a.id AS a,b.id AS b,l.generation AS generation,l.source_extraction_generation AS source_generation`, { id: row.link_id });
          const link = physical.records[0];
          if (physical.records.length !== 1 || !link || link.get("generation") !== null || link.get("source_generation") !== null
            || !((link.get("a") === source && link.get("b") === row.peer_id) || (link.get("b") === source && link.get("a") === row.peer_id))) {
            staleTopology = true; continue;
          }
          if (!await allowed(row.peer_id)) continue;
          ids.add(row.peer_id); eligible.push(row);
        }
      }
      if (staleTopology) {
        // The current attempt retains its verified subset without refill. Close
        // the next attempt under the same fence, using only the affected key.
        await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.conducting_arc_ready=false
          MERGE (c:ConductingArcCoverage {stream:'cache',generation:0}) SET c.state='UNAVAILABLE'`);
        await this.conductingRevisionTx(tx);
      }
      const orderedNodes = [...ids].sort(), nodes = orderedNodes.slice(0,maxNodes), retained = new Set(nodes);
      const orderedArcs = eligible.filter(row => retained.has(row.source_id) && retained.has(row.peer_id));
      const arcs = orderedArcs.slice(0,maxArcs);
      return { nodes, arcs, probes, pin, truncated: orderedNodes.length > maxNodes || orderedArcs.length > maxArcs || probes.some(probe => probe.saturated),
        overflow: { nodes: Math.max(0,orderedNodes.length-maxNodes), arcs: Math.max(0,orderedArcs.length-maxArcs), saturated_sources: probes.filter(probe => probe.saturated).length } };
    });
  }

  async getElement(id: string): Promise<MemoryElement | null> {
    const rows = await this.run<{ e: ElementNode }>(
      `MATCH (e:Element { id: $id }) RETURN e`,
      { id },
    );
    return rows.length ? toElement(nodeProps(rows[0]!["e"])) : null;
  }

  async getPayload(hash: string): Promise<Uint8Array | null> {
    const rows = await this.run<{ hash: string }>(
      `MATCH (p:Payload { hash: $hash }) RETURN p.hash AS hash`,
      { hash },
    );
    return rows.length && await this.objects.has(hash) ? this.objects.get(hash) : null;
  }

  /** One explicit retry operation per Episode. Reusing a completed operation is
   * a no-op; retry a quarantined attempt with a new operation ID. Pending rows
   * can resume after process loss. Provider work never runs in a retried DB tx. */
  async recoverEmbedding(input: RpcEmbeddingRecoverParams, context: InstallationContext): Promise<RpcEmbeddingAttempt> {
    requireInstallation(context);
    const request = RpcEmbeddingRecoverParams.parse(input), provider = this.embeddingProvider;
    if (!provider) throw new RecallError("embedding_not_configured");
    const profile = provider.profile, profileId = embeddingProfileId(profile);
    const prepared = await this.withWriteTx(async tx => {
      const policy = await this.receiptLockTx(tx);
      await this.authorizeEpisodesTx(tx, [request.episode_id], policy);
      const rows = await tx.run<{ content: string; revision: string; digest: string }>(
        `MATCH (e:Element:Episode {id:$id}) RETURN e.content AS content,e.revision_key AS revision,e.digest AS digest`, { id: request.episode_id });
      const row = rows.records[0]!;
      const oldRows = await tx.run<{ body: string }>(`MATCH (a:EmbeddingAttempt {operation_id:$id}) RETURN a.body AS body`, { id: request.operation_id });
      const old = oldRows.records[0] ? RpcEmbeddingAttempt.parse(JSON.parse(oldRows.records[0].get("body"))) : null;
      if (old && (old.episode_id !== request.episode_id || old.profile_id !== profileId || old.input_revision !== row.get("revision") || old.input_digest !== row.get("digest")))
        throw new ReceiptError("idempotency_conflict");
      const attempt = old ?? RpcEmbeddingAttempt.parse({ ...request, profile_id: profileId, model: profile.model,
        model_incarnation: profile.model_incarnation, dimensions: profile.dimensions,
        input_revision: row.get("revision"), input_digest: row.get("digest"), created_at: this.clock(), completed_at: null, state: "pending", reason: null });
      if (!old) await tx.run(`CREATE (:EmbeddingAttempt {operation_id:$id,episode_id:$episode,profile_id:$profile,state:'pending',body:$body})`,
        { id: request.operation_id, episode: request.episode_id, profile: profileId, body: canonicalJson(attempt) });
      return { attempt, content: row.get("content") };
    });
    if (prepared.attempt.state !== "pending") return prepared.attempt;
    let vector: number[] | null = null, reason: RpcEmbeddingAttempt["reason"] = null;
    try { vector = validateVector(await provider.embed(prepared.content, "document"), profile); }
    catch (error) { if (!(error instanceof EmbeddingError)) throw error; reason = error.reason; }
    return this.withWriteTx(async tx => {
      const policy = await this.receiptLockTx(tx);
      await this.authorizeEpisodesTx(tx, [request.episode_id], policy);
      const rows = await tx.run<{ body: string; revision: string; digest: string }>(
        `MATCH (a:EmbeddingAttempt {operation_id:$operation}),(e:Element:Episode {id:$episode})
         RETURN a.body AS body,e.revision_key AS revision,e.digest AS digest`, { operation: request.operation_id, episode: request.episode_id });
      const row = rows.records[0]!, prior = RpcEmbeddingAttempt.parse(JSON.parse(row.get("body")));
      if (prior.state !== "pending") return prior;
      if (row.get("revision") !== prior.input_revision || row.get("digest") !== prior.input_digest) reason = "stale_input";
      const attempt = RpcEmbeddingAttempt.parse({ ...prior, state: reason ? "quarantined" : "succeeded", reason, completed_at: this.clock() });
      if (!reason && vector) {
        const key = tupleHash([request.episode_id, profileId]);
        // First valid vector for this immutable input/model wins. Neither a
        // failed nor a concurrent retry can overwrite it or another model.
        await tx.run(`MERGE (v:EmbeddingVector {key:$key})
          ON CREATE SET v:Embedding_${profileId},v.episode_id=$episode,v.profile_id=$profile,
            v.input_revision=$revision,v.input_digest=$digest,v.vector=$vector,v.operation_id=$operation
          WITH v MATCH (m:Meta {key:'meta'})
          SET m.structure_revision=coalesce(m.structure_revision,0)+CASE WHEN v.operation_id=$operation THEN 1 ELSE 0 END`,
          { key, episode: request.episode_id, profile: profileId, revision: prior.input_revision, digest: prior.input_digest, vector, operation: request.operation_id });
      }
      await tx.run(`MATCH (a:EmbeddingAttempt {operation_id:$id}) SET a.body=$body,a.state=$state`,
        { id: request.operation_id, body: canonicalJson(attempt), state: attempt.state });
      return attempt;
    });
  }

  async embeddingStatus(operationId: string, context: InstallationContext): Promise<RpcEmbeddingAttempt | { state: "unknown"; operation_id: string }> {
    requireInstallation(context); const id = z.uuidv7().parse(operationId);
    return this.withWriteTx(async tx => {
      const policy = await this.receiptLockTx(tx);
      const rows = await tx.run<{ body: string }>(`MATCH (a:EmbeddingAttempt {operation_id:$id}) RETURN a.body AS body`, { id });
      if (!rows.records[0]) return { state: "unknown", operation_id: id };
      const attempt = RpcEmbeddingAttempt.parse(JSON.parse(rows.records[0].get("body")));
      await this.authorizeEpisodesTx(tx, [attempt.episode_id], policy);
      return attempt;
    });
  }

  /** Bound the entire new-occurrence candidate partition before handing any
   * content to the semantic judge. Overflow is unknown, not an empty candidate set. */
  private async semanticCandidatesTx(tx: ManagedTransaction, generation: string, policy: PolicyState) {
    const rows = await tx.run<{ props: ElementProperties }>(`MATCH (f:Fact {generation:$generation})
      USING INDEX f:Fact(generation,id) WHERE f.id IS NOT NULL
      RETURN properties(f) AS props ORDER BY f.id LIMIT 129`, { generation });
    if (rows.records.length > 128) throw new Error("semantic_candidates_unavailable");
    const candidates = [];
    for (const row of rows.records) {
      const p = row.get("props");
      const sources = z.array(z.uuidv7()).min(1).max(16).parse(p["source_episode_ids"]);
      await this.authorizeEpisodesTx(tx, sources, policy);
      candidates.push({ id: z.uuidv7().parse(p["id"]), content: z.string().parse(p["content"]), digest: extractionBodyDigest(p) });
    }
    return candidates;
  }

  private async semanticPremisesTx(tx: ManagedTransaction, request: ProposeRetainedClaim, profile: string, policy: PolicyState): Promise<SemanticReviewPremises> {
    const source = await this.semanticEpisodeTx(tx, request.source_episode_id, policy);
    const generation = await this.writableExtractionGenerationTx(tx, request.generation_id);
    const selection = await this.extractionSelectionTx(tx);
    if (generation.state === "active" && selection.generation_id !== generation.id) throw new Error("semantic_generation_unselected");
    const head = await this.extractionHeadTx(tx, source.id);
    if (head !== source.revision_key) throw new Error("stale_materialization");
    const judge = await this.extractionRecordTx(tx, "ExtractionAttempt", request.judge_attempt_id, ExtractionAttempt);
    if (judge.state !== "succeeded" || judge.generation_id !== generation.id || judge.source_id !== source.id
      || judge.source_revision !== source.revision_key || judge.body_digest !== source.content_digest
      || judge.source_ingest_seq !== source.ingest_seq || judge.policy_context.revision !== policy.policy_revision) throw new Error("stale_materialization");
    const decisions = await this.extractionDecisionsTx(tx, judge);
    const decision = decisions[request.claim_index];
    if (!decision || !["retain", "correct"].includes(decision.disposition)) throw new Error("audit_decision_refused");
    const premise = await this.extractionRecordTx(tx, "ExtractionJudgeInput", judge.id, ExtractionJudgeInput);
    if (premise.source_head_revision !== head || premise.policy_revision !== policy.policy_revision) throw new Error("stale_materialization");
    const candidates = await this.semanticCandidatesTx(tx, generation.id, policy);
    return SemanticReviewPremises.parse({ request, request_digest: extractionBodyDigest(request), judge_profile_id: profile,
      source, audit_evidence: decision.evidence, source_head_revision_key: head, policy_revision: policy.policy_revision,
      generation_digest: extractionBodyDigest(generation), selection, candidates, candidate_digest: extractionBodyDigest(candidates) });
  }

  async prepareSemanticReview(input: ProposeRetainedClaim, profile: string, context: InstallationContext) {
    const request = ProposeRetainedClaim.parse(input), digest = extractionBodyDigest(request);
    return this.extractionTx(context, async (tx, policy) => {
      await this.authorizeEpisodesTx(tx, [request.source_episode_id], policy);
      const old = await tx.run(`MATCH (a:AdjudicationInput {id:$id})
        OPTIONAL MATCH (p:AdjudicationProposal {id:a.id}) RETURN a.body AS body,p.body AS proposal`, { id: request.proposal_id });
      if (old.records[0]) {
        const premises = SemanticReviewPremises.parse(JSON.parse(old.records[0].get("body")));
        if (premises.request_digest !== digest || premises.judge_profile_id !== profile) throw new Error("semantic_proposal_conflict");
        if (!old.records[0].get("proposal")) throw new Error("semantic_review_incomplete");
        return { created: false, premises, proposal: RetainedSemanticProposal.parse(JSON.parse(old.records[0].get("proposal"))) };
      }
      const premises = await this.semanticPremisesTx(tx, request, profile, policy);
      await tx.run(`CREATE (:AdjudicationInput {id:$id,body:$body,started_at:$now})`, { id: request.proposal_id, body: canonicalExtractionBody(premises), now: this.clock() });
      return { created: true, premises, proposal: null };
    });
  }

  private async validateSemanticResolutionTx(tx: ManagedTransaction, premises: SemanticReviewPremises, resolution: SemanticResolution) {
    for (const entity of resolution.entity_resolutions) {
      if (entity.status === "unresolved") continue;
      const rows = await tx.run(`MATCH (e:Element {id:$id}) RETURN e.schema AS schema,e.generation AS generation`, { id: entity.entity_id });
      if (entity.status === "existing") {
        if (rows.records[0]?.get("schema") !== "anamnesis.entity/1" || rows.records[0]?.get("generation") !== premises.request.generation_id) throw new Error("semantic_entity_stale");
      } else {
        const keys = await tx.run(`MATCH (e:Entity {generation:$generation,entity_key:$key}) RETURN e.id LIMIT 1`, { generation: premises.request.generation_id, key: entity.entity_key });
        if (rows.records.length || keys.records.length) throw new Error("semantic_entity_stale");
      }
    }
  }

  private semanticValidation(premises: SemanticReviewPremises, resolution: SemanticResolution, claim: ProposeRetainedClaim["semantic_claim"]) {
    const validated = validateSemanticClaim(claim, { generation: premises.request.generation_id, fact_language_policy: "source",
      entity_resolutions: resolution.entity_resolutions, attribution_speakers: resolution.attribution_speakers,
      allow_no_single_locus: resolution.allow_no_single_locus,
      episode: { ...premises.source, content_language: resolution.content_language } });
    if (canonicalExtractionBody(validated.evidence) !== canonicalExtractionBody(premises.audit_evidence)) throw new Error("semantic_evidence_mismatch");
    return validated;
  }

  /** Completion has retained pre-call premises. It does not accept booleans
   * asserting independence, lineage, policy, or semantic permission. */
  async completeSemanticReview(proposalId: string, resolved: SemanticResolution, judged: SemanticReviewOutput, context: InstallationContext) {
    const id = z.uuidv7().parse(proposalId), resolution = SemanticResolution.parse(resolved), output = SemanticReviewOutput.parse(judged);
    return this.extractionTx(context, async (tx, policy) => {
      const rows = await tx.run(`MATCH (a:AdjudicationInput {id:$id}) RETURN a.body AS body`, { id });
      if (!rows.records[0]) throw new Error("semantic_review_unavailable");
      const premises = SemanticReviewPremises.parse(JSON.parse(rows.records[0].get("body")));
      const terminal = await tx.run(`MATCH (a:AdjudicationAttempt {id:$id}) RETURN a.outcome AS outcome`, { id });
      if (terminal.records[0] && terminal.records[0].get("outcome") !== "succeeded") throw new Error("semantic_review_terminal");
      const current = await this.semanticPremisesTx(tx, premises.request, premises.judge_profile_id, policy);
      if (canonicalExtractionBody(current) !== canonicalExtractionBody(premises)) throw new Error("semantic_review_stale");
      await this.validateSemanticResolutionTx(tx, premises, resolution);
      const validated = this.semanticValidation(premises, resolution, premises.request.semantic_claim);
      if (output.disposition === "retain" && canonicalExtractionBody(output.semantic_claim) !== canonicalExtractionBody(premises.request.semantic_claim)) throw new Error("semantic_output_mismatch");
      if (output.semantic_claim) this.semanticValidation(premises, resolution, output.semantic_claim);
      const proposal = RetainedSemanticProposal.parse({ premises, resolution, output,
        proposed_claim_digest: extractionBodyDigest(semanticReviewClaimBody(validated.claim, premises.source, resolution)),
        output_claim_digest: extractionBodyDigest(output.semantic_claim) });
      const old = await tx.run(`MATCH (p:AdjudicationProposal {id:$id}) RETURN p.body AS body`, { id });
      const body = canonicalExtractionBody(proposal);
      if (old.records[0]) {
        if (old.records[0].get("body") !== body) throw new Error("semantic_proposal_conflict");
        return proposal;
      }
      await tx.run(`CREATE (:AdjudicationAttempt {id:$id,outcome:'succeeded',input_body:$input,output_body:$output,finished_at:$now})
        CREATE (:AdjudicationProposal {id:$id,body:$body,digest:$digest})`,
        { id, input: canonicalExtractionBody(premises), output: canonicalExtractionBody({ resolution, output }), body, digest: extractionBodyDigest(proposal), now: this.clock() });
      return proposal;
    });
  }

  async failSemanticReview(proposalId: string, error: string, context: InstallationContext): Promise<void> {
    const id = z.uuidv7().parse(proposalId);
    await this.extractionTx(context, async tx => {
      await tx.run(`MATCH (a:AdjudicationInput {id:$id})
        MERGE (t:AdjudicationAttempt {id:$id}) ON CREATE SET t.outcome='validation_error',t.input_body=a.body,t.error_digest=$error,t.finished_at=$now`,
        { id, error: sha256(error), now: this.clock() });
    });
  }

  async reviewRetainedClaim(input: ReviewRetainedClaim, context: InstallationContext) {
    const request = ReviewRetainedClaim.parse(input);
    return this.extractionTx(context, async (tx, policy) => {
      if (!context.client_binding) throw new Error("semantic_operator_binding_required");
      const proposal = await this.retainedSemanticProposalTx(tx, request.proposal_id);
      await this.authorizeEpisodesTx(tx, [proposal.premises.source.id], policy);
      const body = canonicalExtractionBody({ ...request, operator_binding: context.client_binding });
      const old = await tx.run(`MATCH (r:AdjudicationReview) WHERE r.proposal_id=$proposal OR r.review_id=$id RETURN r.body AS body`, { proposal: request.proposal_id, id: request.review_id });
      if (old.records.length) {
        if (old.records.length !== 1 || old.records[0]!.get("body") !== body) throw new Error("semantic_review_conflict");
        return request;
      }
      await tx.run(`CREATE (:AdjudicationReview {review_id:$id,proposal_id:$proposal,body:$body,action:$action})`, { id: request.review_id, proposal: request.proposal_id, body, action: request.action });
      return request;
    });
  }

  private async retainedSemanticProposalTx(tx: ManagedTransaction, id: string): Promise<RetainedSemanticProposal> {
    const rows = await tx.run(`MATCH (p:AdjudicationProposal {id:$id}) MATCH (a:AdjudicationAttempt {id:p.id})
      RETURN p.body AS body,p.digest AS digest,a.outcome AS outcome,a.input_body AS input,a.output_body AS output`, { id });
    const row = rows.records[0];
    if (!row) throw new Error("retained_review_unavailable");
    const proposal = RetainedSemanticProposal.parse(JSON.parse(row.get("body")));
    if (proposal.premises.request.proposal_id !== id || row.get("digest") !== extractionBodyDigest(proposal) || row.get("outcome") !== "succeeded"
      || row.get("input") !== canonicalExtractionBody(proposal.premises) || row.get("output") !== canonicalExtractionBody({ resolution: proposal.resolution, output: proposal.output })) throw new Error("semantic_proposal_corrupt");
    return proposal;
  }

  /** One accepted new occurrence. This path does not select a serving generation,
   * infer approval from audit decisions, or merge into a previous generation. */
  async materializeRetainedClaim(input: MaterializeRetainedClaim, context: InstallationContext): Promise<MaterializationResult> {
    canonicalExtractionBody(input);
    const request = MaterializeRetainedClaim.parse(input), digest = extractionBodyDigest(request);
    let allocated: { fact_id: string; link_id: string; mention_ids: string[] } | undefined;
    return this.extractionTx(context, async (tx, policy) => {
      const prior = await tx.run(`MATCH (o:MaterializationOperation {id:$id}) RETURN o.digest AS digest,o.result AS result`, { id: request.operation_id });
      if (prior.records[0]) {
        if (prior.records[0].get("digest") !== digest) throw new Error("materialization_conflict");
        await this.semanticEpisodeTx(tx, request.source_episode_id, policy);
        return MaterializationResult.parse({ ...JSON.parse(prior.records[0].get("result")), created: false });
      }
      if (!request.proposal_id) throw new Error("retained_review_unavailable");
      const proposal = await this.retainedSemanticProposalTx(tx, request.proposal_id);
      const { premises, resolution, output } = proposal;
      const original = premises.request;
      if (request.generation_id !== original.generation_id || request.source_episode_id !== original.source_episode_id
        || request.judge_attempt_id !== original.judge_attempt_id || request.claim_index !== original.claim_index
        || extractionBodyDigest(request.semantic_claim) !== proposal.output_claim_digest) throw new Error("semantic_candidate_mismatch");
      const review = await tx.run(`MATCH (r:AdjudicationReview {proposal_id:$id}) RETURN r.body AS body`, { id: request.proposal_id });
      if (!review.records[0]) throw new Error("retained_review_unavailable");
      const accepted = ReviewRetainedClaim.extend({ operator_binding: z.uuidv7() }).parse(JSON.parse(review.records[0].get("body")));
      if (accepted.action !== "accept" || accepted.proposal_id !== request.proposal_id || output.disposition === "suppress" || !output.semantic_claim) throw new Error("semantic_review_refused");
      const consumed = await tx.run(`MATCH (c:AdjudicationConsumption {proposal_id:$id}) RETURN c.operation_id`, { id: request.proposal_id });
      if (consumed.records.length) throw new Error("semantic_proposal_consumed");
      const current = await this.semanticPremisesTx(tx, original, premises.judge_profile_id, policy);
      if (canonicalExtractionBody(current) !== canonicalExtractionBody(premises)) throw new Error("semantic_review_stale");
      await this.validateSemanticResolutionTx(tx, premises, resolution);
      const validated = this.semanticValidation(premises, resolution, output.semantic_claim);
      if (proposal.proposed_claim_digest !== extractionBodyDigest(semanticReviewClaimBody(original.semantic_claim, premises.source, resolution))) throw new Error("semantic_candidate_mismatch");
      const occurrence = extractionBodyDigest([original.generation_id, original.source_episode_id, original.judge_attempt_id, original.claim_index]);
      const existing = await tx.run(`MATCH (o:MaterializationOperation {occurrence_key:$key}) RETURN o.id`, { key: occurrence });
      if (existing.records.length) throw new Error("claim_already_materialized");
      allocated ??= { fact_id: uuidv7(), link_id: uuidv7(), mention_ids: validated.identity.entity_ids.map(() => uuidv7()) };
      const result = await this.writeValidatedFactTx(tx, { validated, resolution, source: premises.source,
        generation: request.generation_id, profile: premises.judge_profile_id, policy: policy.policy_revision,
        operationId: request.operation_id, digest, occurrence, proposalId: request.proposal_id, allocated });
      await tx.run(`CREATE (:AdjudicationConsumption {proposal_id:$proposal,operation_id:$id,fact_id:$fact,link_id:$link})`,
        { proposal: request.proposal_id, id: request.operation_id, fact: result.fact_id, link: result.link_id });
      return result;
    });
  }

  /** Both admission paths write only a mechanically validated new occurrence.
   * mergeLinkTx creates physical links and their real conducting rows atomically.
   * Relation adjudication is separate; this body never invalidates another Fact. */
  private async writeValidatedFactTx(tx: ManagedTransaction, input: {
    validated: ValidatedSemanticClaim; resolution: SemanticResolution; source: SemanticReviewPremises["source"];
    generation: string; profile: string; policy: number; operationId: string; digest: string; occurrence: string;
    proposalId: string | null; allocated: { fact_id: string; link_id: string; mention_ids: string[] };
  }): Promise<MaterializationResult> {
    const { validated, resolution, source, generation, profile, policy, operationId, digest, occurrence, proposalId, allocated } = input;
    const claim = validated.claim, { fact_id, link_id } = allocated;
    const inherited = claim.time.resolution === "inherited";
    const precision = inherited ? source.time.time_precision : claim.time.time_precision;
    const element = MemoryElement.parse({ id: fact_id, schema: "anamnesis.claim/1", content: claim.content,
      time: { value: new Date(claim.time.time_utc).toISOString(), precision: ["instant", "inherited"].includes(precision) ? "second" : precision },
      origin: { source: "semantic-extraction", session: generation, actor: profile, record: occurrence },
      mass: claim.confidence, properties: { ...validated.identity.properties, sub_kind: claim.sub_kind, modality: claim.modality,
        confidence: claim.confidence, content_language: claim.content_language, semantic_time: claim.time,
        time_basis: inherited ? "episode_fallback" : "claim" } });
    await this.createElementTx(tx, element, null, {});
    await tx.run(`MATCH (f:Fact {id:$id}) SET f.generation=$generation,f.content_language=$language,f.sub_kind=$subkind,f.modality=$modality,
      f.confidence=$confidence,f.meaning_digest=$meaning,f.primary_episode_id=$source,f.source_episode_ids=$sources,f.max_source_ingest_seq=$seq,
      f.echo_state=$echo,f.echo_depth=$depth,f.echo_lineage_truncated=false,f.parent_recall_ids=$parents,f.corroboration_root_episode_ids=$roots,
      f.entity_ids=$entities,f.support_fact_ids=[],f.proposal_id=$proposal,f.policy_revision=$policy,f.semantic_profile_id=$profile`,
      { id: fact_id, generation, language: claim.content_language, subkind: claim.sub_kind, modality: claim.modality,
        confidence: claim.confidence, meaning: validated.meaning_digest, source: source.id, sources: [source.id], seq: source.ingest_seq,
        echo: validated.identity.echo_state, depth: validated.identity.echo_depth, parents: validated.identity.parent_recall_ids,
        roots: validated.identity.corroboration_root_episode_ids, entities: validated.identity.entity_ids, proposal: proposalId, policy, profile });
    await this.mergeLinkTx(tx, MemoryLink.parse({ id: link_id, from: fact_id, to: source.id, role: "DERIVED_FROM", content: "semantic evidence", weight: 1 }));
    await tx.run(`MATCH ()-[l:DERIVED_FROM]->() WHERE l.id=$id SET l.span=$span,l.evidence_text=$text,l.proposal_id=$proposal`,
      { id: link_id, span: validated.evidence ? [validated.evidence.start, validated.evidence.end] : null, text: validated.evidence?.text ?? null, proposal: proposalId });
    const createdEntities = new Set<string>();
    for (const entity of resolution.entity_resolutions) {
      if (entity.status !== "new" || !validated.identity.entity_ids.includes(entity.entity_id) || createdEntities.has(entity.entity_id)) continue;
      await this.createElementTx(tx, MemoryElement.parse({ id: entity.entity_id, schema: "anamnesis.entity/1", content: entity.normalized_name,
        origin: { source: "semantic-extraction", session: generation, actor: profile, record: entity.entity_key },
        properties: { normalized_name: entity.normalized_name, entity_kind: entity.entity_kind, entity_key: entity.entity_key } }), null, {});
      await tx.run(`MATCH (e:Entity {id:$id}) SET e.generation=$generation,e.entity_key=$key`, { id: entity.entity_id, generation, key: entity.entity_key });
      createdEntities.add(entity.entity_id);
    }
    for (const [i, entity] of validated.identity.entity_ids.entries()) {
      await this.mergeLinkTx(tx, MemoryLink.parse({ id: allocated.mention_ids[i], from: fact_id, to: entity, role: "MENTIONS", content: "semantic entity mention", weight: 1 }));
      await tx.run(`MERGE (w:EntityWitness {entity_id:$entity,generation:$generation,policy_revision:$policy}) SET w.state='COMPLETE'`, { entity, generation, policy: neo4j.int(policy) });
    }
    const result = MaterializationResult.parse({ created: true, fact_id, link_id });
    await tx.run(`CREATE (:MaterializationOperation {id:$id,digest:$digest,result:$result,occurrence_key:$key,source_episode_id:$source,generation:$generation,semantic_profile_id:$profile,fact_id:$fact,link_id:$link})
      WITH 1 AS ignored MATCH (m:Meta {key:'meta'}) SET m.structure_revision=coalesce(m.structure_revision,0)+1`,
      { id: operationId, digest, result: canonicalExtractionBody(result), key: occurrence, source: source.id, generation, profile, fact: fact_id, link: link_id });
    return result;
  }

  /** Originals-only increment. All candidate reads, policy revalidation, source
   * resolution, packing and receipt issuance share the Meta write barrier.
   * The daemon serial owner writes the response before acknowledging any policy
   * command. No derived authority, profile-cache anchors or PPR are invented. */
  async recall(input: z.input<typeof RpcRecallParams>, context: InstallationContext): Promise<RpcRecallResult> {
    requireInstallation(context);
    const request = RpcRecallParams.parse(input), budget = admittedBudget(request.budget, this.tokenizers, this.recallDefaultBytes);
    const now = receiptTime.parse(this.clock()), T = request.T ?? now, recallId = uuidv7();
    const provider = this.embeddingProvider, profileId = provider ? embeddingProfileId(provider.profile) : null;
    let queryVector: number[] | null = null;
    let vectorReason: RpcRecallResult["diagnostics"]["vector_reason"] = provider ? "not_requested" : "not_configured";
    if (provider && request.limit > 0 && budget.limit > 0) {
      try { queryVector = validateVector(await provider.embed(request.query, "query"), provider.profile); vectorReason = "available"; }
      catch (error) { if (!(error instanceof EmbeddingError)) throw error; vectorReason = error.reason; }
    }
    return this.withWriteTx(async tx => {
      const policy = await this.receiptLockTx(tx);
      const selection = await this.extractionSelectionTx(tx);
      const denies = [...policy.denies.values()].filter(deny => !policy.revoked.has(deny.policy_id));
      // AND semantics stay identical to feedback authority, including policies
      // that specify both source and Episode ID.
      const parameters = { T: new Date(T).toISOString(), denies: denies.map(deny => policySelector(deny.selector)), schemas: ["anamnesis.original-message/1", "anamnesis.original-document/1"] };
      const allowed = `e.schema IN $schemas AND e.time_utc <= $T
        AND NONE(d IN $denies WHERE (d.episode_id IS NULL OR d.episode_id=e.id) AND (d.source IS NULL OR d.source=e.origin_source))
        AND NOT EXISTS { MATCH ()-[inv:INVALIDATES]->() WHERE inv.target_id=e.id AND inv.effective_time_utc <= $T AND inv.id IS NOT NULL }`;
      const lists: Record<string, { id: string }[]> = {}, nodes = new Map<string, ElementNode>(), derivedNodes = new Map<string, ElementNode>();
      let pprUsed = false, pprScores = new Map<string, number>();
      const channel = async (name: string, query: string, params: Record<string, unknown>) => {
        const rows = await tx.run<{ e: ElementNode }>(query, { ...parameters, ...params });
        lists[name] = rows.records.map(row => { const e = row.get("e"); const id = String(e.properties["id"]); nodes.set(id, e); return { id }; });
      };
      if (request.limit > 0 && budget.limit > 0) {
        await channel("identity", `MATCH (e:Element:Episode {id:$id}) WHERE ${allowed} RETURN e LIMIT 1`, { id: request.query });
        const q = luceneQuery(request.query);
        if (q) await channel("bm25", `CALL db.index.fulltext.queryNodes('element_content',$q,{limit:256}) YIELD node,score
          WITH node AS e,score WHERE e:Episode AND ${allowed} RETURN e ORDER BY score DESC,e.id ASC LIMIT 64`, { q });
        if (request.session) await channel("session", `MATCH (e:Episode {session_key:$session}) WHERE ${allowed}
          RETURN e ORDER BY e.time_utc DESC,e.ingest_seq DESC,e.id ASC LIMIT 32`, { session: tupleHash([request.session.source, request.session.session]) });
        if (queryVector && profileId) await channel("vector", `CALL db.index.vector.queryNodes($index,256,$vector) YIELD node,score
          MATCH (e:Element:Episode {id:node.episode_id}) WHERE node.profile_id=$profile AND node.input_revision=e.revision_key AND node.input_digest=e.digest AND ${allowed}
          RETURN e ORDER BY score DESC,e.id ASC LIMIT 64`, { index: `vec_episode_${profileId}`, vector: queryVector, profile: profileId });
      }
      const cacheRows = await tx.run<{ id: string; s: number | null; last: number | null; utility: number | null }>(
        `UNWIND $ids AS id OPTIONAL MATCH (c:HitCache {episode_id:id})
         RETURN id,c.s AS s,c.t_last_hit AS last,c.utility AS utility`, { ids: [...nodes.keys()] });
      const caches = new Map(cacheRows.records.map(row => [row.get("id"), row]));
      const ranked: RpcRecallItem[] = [];
      for (const [id, node] of nodes) {
        const props = nodeProps(node), e = toElement(props), cache = caches.get(id)!;
        const mass = e.mass * retention(Math.max(0, now - (cache.get("last") ?? Number(props["ingested_at"]))) / 86400000, cache.get("s") ?? initialStability(e.mass));
        const relevance = normalizedRrf(lists, id), utility = cache.get("utility") ?? 0;
        const item = { id, kind: "Episode", schema: e.schema, epistemic: "observed", content: e.content, time: e.time,
          mass, utility, relevance, score: relevance * Math.sqrt(Math.max(mass, 0.02)) * (1 + 0.25 * utility), sources: [id],
          provenance: { derived_from: [{ id, kind: "Episode", visible_at_T: true }], supersedes: [], supersedes_redacted: false, contrasts: [], warnings: [] },
          channels: Object.keys(lists).filter(name => lists[name]!.some(candidate => candidate.id === id)) };
        ranked.push(RpcRecallResult.shape.results.element.parse(item));
      }
      if (selection.generation_id && budget.limit > 0) {
        const q = luceneQuery(request.query);
        if (q) {
          const derivedRows = await tx.run(`CALL db.index.fulltext.queryNodes('element_content',$q,{limit:256}) YIELD node,score
            WITH node AS f,score WHERE f:Fact AND f.generation=$generation AND f.time_utc <= $T
            RETURN f,score ORDER BY score DESC,f.id ASC LIMIT 64`, { q, generation: selection.generation_id, T: new Date(T).toISOString() });
          const bm25 = lists["bm25"] ?? (lists["bm25"] = []);
          for (const row of derivedRows.records) {
            const fact = row.get("f") as ElementNode, id = String(fact.properties["id"]);
            derivedNodes.set(id, fact); bm25.push({ id });
          }
        }
      }
      if (selection.generation_id && (Object.values(lists).some(list => list.length > 0) || derivedNodes.size > 0)) {
        const seeds = [...new Set(Object.values(lists).flat().map(hit => hit.id).concat([...derivedNodes.keys()]))];
        const graph = await tx.run(`MATCH (a:ConductingArc)
          WHERE a.generation=$generation AND (a.source_id IN $seeds OR a.peer_id IN $seeds)
          RETURN a.source_id AS source,a.peer_id AS peer,a.role AS role,a.link_id AS id LIMIT 1024`, { generation: selection.generation_id, seeds });
        const arcs = graph.records.map(record => ({ from: record.get("source"), to: record.get("peer"), role: record.get("role"), id: record.get("id") }));
        const graphNodes = [...new Set(seeds.concat(arcs.flatMap(arc => [arc.from, arc.to])))];
        if (graphNodes.length) {
          const solved = solvePpr({ nodes: graphNodes, arcs, seeds: new Map(seeds.map(id => [id, 1])) });
          const sorted = [...graphNodes].sort(); pprScores = new Map(sorted.map((id, index) => [id, solved.values[index]!]));
          pprUsed = true;
        }
      }
      // Derived serving is strictly generation-selected and policy-authorized.
      // Every source Episode is rechecked for primaries AND their mandatory peers.
      const factItems = new Map<string, RpcRecallItem>();
      if (selection.generation_id && budget.limit > 0) {
        const factItem = async (node: ElementNode): Promise<RpcRecallItem | null> => {
          const id = String(node.properties["id"]), cached = factItems.get(id);
          if (cached) return cached;
          const sourceRows = await tx.run(`MATCH (f:Fact {id:$id})-[:DERIVED_FROM]->(e:Element:Episode) RETURN e.id AS source LIMIT 2`, { id });
          const sourceId = sourceRows.records[0]?.get("source");
          if (typeof sourceId !== "string") return null;
          try { await this.authorizeEpisodesTx(tx, [z.uuidv7().parse(sourceId)], policy); }
          catch (error) { if (error instanceof ReceiptError && error.code === "policy_denied") return null; throw error; }
          const fact = toElement(nodeProps(node)), ppr = pprScores.get(id) ?? 0;
          const item = RpcRecallResult.shape.results.element.parse({ id, kind: "Fact", schema: "anamnesis.claim/1", epistemic: "derived",
            content: fact.content, time: fact.time!, mass: Math.max(0, Math.min(1, ppr || fact.mass)), utility: 0,
            relevance: Math.max(0, ppr), score: Math.max(0, ppr || fact.mass), sources: [z.uuidv7().parse(sourceId)],
            provenance: { derived_from: [{ id: z.uuidv7().parse(sourceId), kind: "Episode", visible_at_T: true }], supersedes: [], supersedes_redacted: false,
              contrasts: [], warnings: [] }, channels: derivedNodes.has(id) ? ["bm25"] : [] });
          factItems.set(id, item); return item;
        };
        for (const [id, node] of derivedNodes) {
          const item = await factItem(node); if (!item) continue;
          const peers = await tx.run<{ other: ElementNode }>(`MATCH (f:Fact {id:$id})-[:CONTRASTS]-(other:Fact {generation:$generation})
            WHERE other.time_utc <= $T RETURN DISTINCT other ORDER BY other.id LIMIT 4`, { id, generation: selection.generation_id, T: parameters.T });
          for (const row of peers.records) {
            const peer = await factItem(row.get("other"));
            if (peer) item.provenance.contrasts.push(peer.id);
          }
          ranked.push(item);
        }
      }
      // Originals are retained in `nodes`; Facts are served from `derivedNodes`.
      ranked.sort((a, b) => b.score - a.score || b.relevance - a.relevance || b.mass - a.mass || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      const bundles: RecallBundle[] = [];
      for (const primary of ranked) {
        const supersedes = await tx.run<{ e: ElementNode }>(`MATCH (:Element:Episode {id:$id})-[:INVALIDATES]->(e:Element:Episode)
          RETURN DISTINCT e ORDER BY e.id ASC LIMIT 9`, { id: primary.id });
        for (const row of supersedes.records.slice(0, 8)) {
          const old = toElement(nodeProps(row.get("e")));
          const denied = denies.some(deny => (deny.selector.episode_id === undefined || deny.selector.episode_id === old.id)
            && (deny.selector.source === undefined || deny.selector.source === old.origin.source));
          if (denied) primary.provenance.supersedes_redacted = true;
          else primary.provenance.supersedes.push({ id: old.id, content: old.content });
        }
        if (primary.provenance.supersedes_redacted) primary.provenance.warnings.push({ code: "supersedes_withheld", content: "Prior revision content is withheld by current policy." });
        if (supersedes.records.length > 8) primary.provenance.warnings.push({ code: "supersedes_incomplete", content: "Prior revision provenance exceeds the bounded eight-entry view." });
        bundles.push({ primary, companions: primary.provenance.contrasts.map(id => factItems.get(id)!) });
      }
      const result = RpcRecallResult.parse(packRecall(bundles, {
        recall_id: recallId, expires_at: now + 3600000, results: [], companions: [], entities: [], context_text: "", used_budget: 0,
        budget, renderer: "canonical-jsonl-v1", diagnostics: { pipeline: ranked.some(item => item.kind === "Fact") ? "derived-hybrid-v1" : "originals-hybrid-v1", now, T,
          policy_revision: policy.policy_revision, channels_used: Object.keys(lists).filter(name => lists[name]!.length > 0) as RpcRecallResult["diagnostics"]["channels_used"],
          vector_reason: vectorReason, embedding_profile_id: profileId, candidate_count: ranked.length, skipped_bundles: 0,
          ppr_used: pprUsed, identity_mode: "exact_episode_id" },
      }, request.limit, this.tokenizers));
      await this.authorizeEpisodesTx(tx, [...new Set(result.results.flatMap(item => item.sources))], policy);
      await this.issueReceiptTx(tx, IssueReceiptInput.parse({ recall_id: recallId, primary_ids: result.results.map(item => item.id) }), context, policy,
        { response: result, context_digest: sha256(result.context_text), result_digest: sha256(canonicalContext({ results: result.results, companions: result.companions })), query: request.query, query_vector: queryVector, candidates: ranked.map(({ id, score, relevance, mass, utility }) => ({ id, score, relevance, mass, utility })) });
      return result;
    });
  }

  async searchText(
    query: string,
    opts: { limit?: number; until?: string; validOnly?: boolean } = {},
  ): Promise<SearchHit[]> {
    const q = luceneQuery(query);
    if (!q) return [];
    const rows = await this.run<{ e: ElementNode; score: number }>(
      `CALL db.index.fulltext.queryNodes('element_content', $q)
       YIELD node, score
       WHERE ($until IS NULL OR node.time_utc IS NULL
              OR node.time_utc <= $until)
         AND (NOT $validOnly OR NOT EXISTS {
           MATCH ()-[inv:INVALIDATES]->()
           WHERE inv.target_id = node.id
             AND inv.effective_time_utc <= coalesce($until, $endOfTime)
             AND inv.id IS NOT NULL
         })
       RETURN node AS e, score
       ORDER BY score DESC
       LIMIT $limit`,
      {
        q,
        until: opts.until ? toUtc(opts.until) : null,
        endOfTime: END_OF_TIME,
        validOnly: opts.validOnly ?? false,
        limit: neo4j.int(opts.limit ?? 20),
      },
    );
    return rows.map((r) => ({
      element: toElement(nodeProps(r["e"])),
      score: r.score,
    }));
  }

  async linksOf(id: string, role?: LinkRole): Promise<MemoryLink[]> {
    const rows = await this.run<{
      l: LinkRelationship;
      role: LinkRole;
      from: string;
      to: string;
    }>(
      `MATCH (a:Element)-[l]-(b:Element)
       WHERE (a.id = $id OR b.id = $id)
         AND ($role IS NULL OR type(l) = $role)
       WITH DISTINCT l, startNode(l) AS s, endNode(l) AS t
       RETURN l, type(l) AS role, s.id AS from, t.id AS to`,
      { id, role: role ?? null },
    );
    return rows.map((r) => {
      const p = relProps(r["l"]);
      return MemoryLink.parse({
        id: p["id"],
        from: r.from,
        to: r.to,
        role: r.role,
        content: p["content"],
        weight: p["weight"],
      });
    });
  }

  async isValidAt(id: string, at: string): Promise<boolean> {
    const atUtc = toUtc(at);
    const rows = await this.run<{ valid: boolean }>(
      `MATCH (e:Element { id: $id })
       RETURN (e.time_utc IS NULL OR e.time_utc <= $at)
              AND NOT EXISTS {
                MATCH ()-[inv:INVALIDATES]->()
                WHERE inv.target_id = e.id
                  AND inv.effective_time_utc <= $at
                  AND inv.id IS NOT NULL
              } AS valid`,
      { id, at: atUtc },
    );
    return rows.length > 0 && rows[0]!.valid;
  }

  async pending(limit = 100): Promise<string[]> {
    const rows = await this.run<{ id: string }>(
      `MATCH (o:Outbox) WHERE o.processed_at IS NULL
       RETURN o.element_id AS id ORDER BY id LIMIT $limit`,
      { limit: neo4j.int(limit) },
    );
    return rows.map((r) => r.id);
  }

  /** Deferred entries stay in the outbox (non-terminal attempt, e.g. provider_unavailable) for a later pass. */
  async drainEmbeddingOutbox(limit = 100, context: InstallationContext = { principal: "installation", commit_mode: "auto" }):
    Promise<{ drained: number; quarantined: number; deferred: number; deferral_reason: string | null } | { drained: 0; reason: "embeddings_disabled" }> {
    const bounded = z.number().int().min(1).max(1000).parse(limit);
    if (!this.embeddingProvider) return { drained: 0, reason: "embeddings_disabled" };
    let drained = 0, quarantined = 0, deferred = 0;
    let deferralReason: string | null = null;
    for (const episodeId of await this.pending(bounded)) {
      const attempt = await this.recoverEmbedding({ operation_id: uuidv7(), episode_id: z.uuidv7().parse(episodeId) }, context);
      if (attempt.state === "succeeded" || attempt.state === "quarantined") {
        await this.markProcessed([episodeId]);
        drained++;
        if (attempt.state === "quarantined") quarantined++;
      } else { deferred++; deferralReason = attempt.reason ?? attempt.state; }
    }
    return { drained, quarantined, deferred, deferral_reason: deferralReason };
  }

  async markProcessed(elementIds: string[]): Promise<void> {
    await this.withWriteTx((tx) => tx.run(
      `MATCH (o:Outbox) WHERE o.element_id IN $ids
       SET o.processed_at = $now`,
      { ids: elementIds, now: new Date().toISOString() },
    ).then(() => undefined));
  }

  async requeue(schema: string): Promise<number> {
    return this.withWriteTx(async (tx) => {
      const rows = await tx.run<{ n: number }>(
      `MATCH (e:Element { schema: $schema })
       CREATE (o:Outbox { element_id: e.id, enqueued_at: $now,
                          processed_at: null })-[:OF]->(e)
       RETURN count(o) AS n`,
      { schema, now: new Date().toISOString() },
      );
      const record = rows.records[0];
      if (!record) throw new Error("requeue count returned no result");
      return record.get("n");
    });
  }

  async verify(): Promise<IntegrityIssue[]> {
    const issues: IntegrityIssue[] = [];
    const rows = await this.run<{ e: ElementNode }>(
      `MATCH (e:Element) RETURN e`,
    );
    for (const row of rows) {
      const p = nodeProps(row["e"]);
      const elementId = String(p["id"]);
      const format = p["digest_format"] ?? null;
      if ((format !== null && format !== CANONICAL_DIGEST && format !== "episode-rfc8785-v2")
        || (p["episode_digest_version"] != null && p["episode_digest_version"] !== 2)
        || (format === "episode-rfc8785-v2") !== (p["episode_digest_version"] === 2)) {
        issues.push({ elementId, kind: "unsupported-digest-format" });
        continue;
      }
      // Choose the existing stored format before validation, never as a fallback
      // from failed modern admission. No defaults enter the historical digest.
      let el: MemoryElement;
      let payloadHash: string | null;
      let previousRevisionKey: string | null;
      try {
        payloadHash = StoredHash.parse(p["payload_hash"] ?? null);
        previousRevisionKey = StoredHash.parse(p["previous_revision_key"] ?? null);
        el = format === null ? decodeHistoricalElement(p) : toElement(p);
      } catch (error) {
        if (!(error instanceof z.ZodError) && !(error instanceof SyntaxError)) throw error;
        issues.push({ elementId, kind: "malformed-element" });
        continue;
      }
      if (format === null) {
        const reasons = historicalEligibility(el);
        if (reasons.length) issues.push({ elementId, kind: "semantic-ineligibility", reasons });
      }
      try {
        if (p["episode_digest_version"] === 2) {
          try { await this.withReadTx(tx => this.lineageTx(tx, elementId, String(p["lineage_digest"]))); }
          catch (error) {
            if (!(error instanceof EpisodeLineageError) && !(error instanceof z.ZodError) && !(error instanceof SyntaxError)) throw error;
            issues.push({ elementId, kind: "digest-mismatch" });
          }
        }
        if (elementDigest(el, { payloadHash, previousRevisionKey, format,
          episodeDigestVersion: p["episode_digest_version"] === 2 ? 2 : null,
          originRole: p["origin_role"] as string | null, lineageDigest: p["lineage_digest"] as string | null }) !== p["digest"]) {
          issues.push({ elementId: el.id, kind: "digest-mismatch" });
        }
      } catch (error) {
        if (!(error instanceof StorageContractError)) throw error;
        if (error.code !== "unsupported_digest_format" && error.code !== "invalid_canonical_json") throw error;
        issues.push({ elementId: el.id, kind: error.code === "unsupported_digest_format"
          ? "unsupported-digest-format" : "digest-mismatch" });
      }
      if (payloadHash) {
        if (format === null) {
          // Raw integrity must see damaged bytes, not ObjectStore.has()'s
          // serving eligibility result (which also rejects missing sidecars).
          const metadata = await this.run<{ hash: string }>(
            "MATCH (p:Payload {hash:$hash}) RETURN p.hash AS hash", { hash: payloadHash });
          if (!metadata.length) issues.push({ elementId, kind: "missing-payload" });
          try {
            const payload = await this.objects.get(payloadHash);
            if (sha256(payload) !== payloadHash) issues.push({ elementId, kind: "payload-hash-mismatch" });
          } catch (error) {
            if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) throw error;
            if (metadata.length) issues.push({ elementId, kind: "missing-payload" });
          }
        } else {
          const payload = await this.getPayload(payloadHash);
          if (!payload) {
            issues.push({ elementId: el.id, kind: "missing-payload" });
          } else if (sha256(payload) !== payloadHash) {
            issues.push({ elementId: el.id, kind: "payload-hash-mismatch" });
          }
        }
      }
    }
    for (const row of topologyExpectations(await this.run<TopologyRow>(TOPOLOGY_QUERY))) {
      if (row.version !== 1) {
        issues.push({ elementId: row.id, kind: "unsupported-topology-format" });
      } else if (row.actual.filter((edge) => edge !== null).length !== row.parents.length || row.parents.some((parent) =>
        !row.actual.some((edge) => edge !== null && edge.from === parent && edge.key === tupleHash([row.sessionKey, parent, row.id])))) {
        issues.push({ elementId: row.id, kind: "topology-mismatch" });
      }
    }
    return issues;
  }

  async counts(): Promise<{
    elements: number;
    links: number;
    pending: number;
  }> {
    const rows = await this.run<{
      elements: number;
      links: number;
      pending: number;
    }>(
      `CALL () { MATCH (e:Element) RETURN count(e) AS elements }
       CALL () { MATCH (:Element)-[l]->(:Element) RETURN count(l) AS links }
       CALL () { MATCH (o:Outbox) WHERE o.processed_at IS NULL
                 RETURN count(o) AS pending }
       RETURN elements, links, pending`,
    );
    const r = rows[0]!;
    return {
      elements: r.elements,
      links: r.links,
      pending: r.pending,
    };
  }

  /** Internal installation API only. No unfenced compatibility writer is allowed
   * for extraction, including status reads that may reveal retained content. */
  private async extractionTx<T>(context: InstallationContext, work: (tx: ManagedTransaction, policy: PolicyState) => Promise<T>): Promise<T> {
    requireInstallation(context);
    if (this.writerEpoch === undefined) throw new Error("writer_epoch_required");
    return this.withWriteTx(async tx => work(tx, await this.receiptLockTx(tx)));
  }

  private async extractionRecordTx<T>(tx: ManagedTransaction, label: "ExtractionGeneration" | "ExtractionAttempt" | "ModelTask" | "ExtractionJudgeInput", id: string, schema: z.ZodType<T>): Promise<T> {
    const rows = await tx.run<{ body: string }>(`MATCH (n:${label} {id:$id}) RETURN n.body AS body`, { id });
    if (!rows.records[0]) throw new Error(`unknown_${label}`);
    return schema.parse(JSON.parse(rows.records[0].get("body")));
  }

  private async extractionSourceTx(tx: ManagedTransaction, sourceId: string) {
    const rows = await tx.run<{ content: string; revision: string; seq: number }>(
      `MATCH (e:Element:Episode {id:$id}) RETURN e.content AS content,e.revision_key AS revision,e.ingest_seq AS seq`, { id: sourceId });
    const row = rows.records[0];
    if (!row) throw new Error("unknown_source");
    const content = z.string().parse(row.get("content"));
    return { content, source_revision: receiptHash.parse(row.get("revision")), source_ingest_seq: receiptTime.positive().parse(row.get("seq")), body_digest: sha256(Buffer.from(content, "utf8")) };
  }

  private async validateExtractionSourceTx(tx: ManagedTransaction, task: ModelTask) {
    const source = await this.extractionSourceTx(tx, task.source_id);
    if (source.source_revision !== task.source_revision || source.body_digest !== task.body_digest || source.source_ingest_seq !== task.source_ingest_seq) throw new Error("stale_input");
    return source;
  }

  private async writableExtractionGenerationTx(tx: ManagedTransaction, id: string): Promise<Generation> {
    const generation = await this.extractionRecordTx(tx, "ExtractionGeneration", id, Generation);
    if (!["active", "catching_up"].includes(generation.state)) throw new Error("generation_not_writable");
    return generation;
  }

  async createExtractionGeneration(input: Generation, context: InstallationContext): Promise<Generation> {
    requireInstallation(context);
    const value = Generation.parse(input), body = canonicalExtractionBody(value);
    return this.extractionTx(context, async tx => {
      const rows = await tx.run<{ body: string; creation: string }>(`MATCH (g:ExtractionGeneration {id:$id}) RETURN g.body AS body,g.creation_body AS creation`, { id: value.id });
      const old = rows.records[0];
      if (old) {
        if (old.get("creation") !== body) throw new Error("generation_conflict");
        return Generation.parse(JSON.parse(old.get("body")));
      }
      if (value.covered_ingest_seq !== 0 || !["active", "catching_up"].includes(value.state)) throw new Error("invalid_generation_initial_state");
      await tx.run(`MATCH (m:Meta {key:'meta'})
        CREATE (:ExtractionGeneration {id:$id,body:$body,creation_body:$body,state:$state,
          covered_ingest_seq:0,source_high_watermark:m.ingest_seq})`, { id: value.id, body, state: value.state });
      const retained = await tx.run(`MATCH ()-[l:MENTIONS|RELATES_TO|DERIVED_FROM]->() WHERE l.generation=$id RETURN l.id LIMIT 1`, {id:value.id});
      await tx.run(`MERGE (c:ConductingArcCoverage {stream:'extraction',generation:$id})
        SET c.state=$state`, {id:value.id,state:retained.records.length ? "UNAVAILABLE" : "COMPLETE"});
      if (retained.records.length) await this.invalidateConductingTx(tx);
      await this.conductingRevisionTx(tx);
      return value;
    });
  }

  async getExtractionGeneration(id: string, context: InstallationContext): Promise<Generation> {
    return this.extractionTx(context, tx => this.extractionRecordTx(tx, "ExtractionGeneration", z.uuidv7().parse(id), Generation));
  }

  async createModelTask(input: CreateModelTask, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = CreateModelTask.parse(input), digest = extractionBodyDigest(request);
    return this.extractionTx(context, async (tx, policy) => {
      await this.writableExtractionGenerationTx(tx, request.generation_id);
      await this.authorizeEpisodesTx(tx, [request.source_id], policy);
      const source = await this.extractionSourceTx(tx, request.source_id);
      const oldRows = await tx.run<{ body: string; digest: string }>(`MATCH (t:ModelTask {id:$id}) RETURN t.body AS body,t.creation_digest AS digest`, { id: request.id });
      const old = oldRows.records[0];
      if (old) {
        if (old.get("digest") !== digest) throw new Error("task_conflict");
        const task = ModelTask.parse(JSON.parse(old.get("body")));
        await this.validateExtractionSourceTx(tx, task);
        return task;
      }
      const workKey = `${request.generation_id}:${request.source_id}`;
      const work = await tx.run(`MATCH (t:ModelTask {work_key:$key}) RETURN t.id AS id`, { key: workKey });
      if (work.records.length) throw new Error("task_conflict");
      const now = receiptTime.parse(this.clock());
      const task = ModelTask.parse({ ...request, source_revision: source.source_revision, source_ingest_seq: source.source_ingest_seq, body_digest: source.body_digest,
        attempt_id: null, state: "queued", lease: null, policy_context: null, attempts: 0, version: 0, created_at: now, updated_at: now });
      await this.extractionNotCoveredTx(tx, task);
      await tx.run(`CREATE (:ModelTask {id:$id,work_key:$key,generation_id:$generation,source_id:$source,source_ingest_seq:$seq,creation_digest:$digest,body:$body,state:'queued'})`,
        { id: task.id, key: workKey, generation: task.generation_id, source: task.source_id, seq: task.source_ingest_seq, digest, body: canonicalExtractionBody(task) });
      if (task.pipeline) await tx.run(`CREATE (:ExtractionPipeline {id:$id,generation_id:$generation,source_id:$source,source_ingest_seq:$seq})`,
        {id:task.id,generation:task.generation_id,source:task.source_id,seq:task.source_ingest_seq});
      return task;
    });
  }

  async getExtractionTask(id: string, context: InstallationContext): Promise<ModelTask> {
    return this.extractionTx(context, async (tx, policy) => {
      const task = await this.extractionRecordTx(tx, "ModelTask", z.uuidv7().parse(id), ModelTask);
      await this.authorizeEpisodesTx(tx, [task.source_id], policy);
      return task;
    });
  }

  async getExtractionAttempt(id: string, context: InstallationContext): Promise<ExtractionAttempt> {
    return this.extractionTx(context, async (tx, policy) => {
      const attempt = await this.extractionRecordTx(tx, "ExtractionAttempt", z.uuidv7().parse(id), ExtractionAttempt);
      await this.authorizeEpisodesTx(tx, [attempt.source_id], policy);
      return attempt;
    });
  }

  private async extractionHeadTx(tx: ManagedTransaction, sourceId: string): Promise<string> {
    const rows = await tx.run(`MATCH (e:Element:Episode {id:$id}) MATCH (h:OriginHead {origin_key:e.origin_key}) RETURN h.revision_key AS head`, {id:sourceId});
    return receiptHash.parse(rows.records[0]?.get('head'));
  }

  private async extractionClaimContextTx(tx: ManagedTransaction, id: string): Promise<ExtractionClaimContext> {
    const task = await this.extractionRecordTx(tx,'ModelTask',id,ModelTask);
    if (task.pipeline !== 'claim-judge-audit-v1' || task.kind !== 'claim' || task.state !== 'succeeded' || !task.attempt_id) throw new ExtractionAuditError('extraction_audit_incomplete');
    const attempt = await this.extractionRecordTx(tx,'ExtractionAttempt',task.attempt_id,ExtractionAttempt);
    if (attempt.state !== 'succeeded' || !attempt.output || attempt.task_id !== task.id || attempt.generation_id !== task.generation_id
      || attempt.source_id !== task.source_id || attempt.source_revision !== task.source_revision || attempt.body_digest !== task.body_digest) throw new ExtractionAuditError('extraction_audit_conflict');
    const body = ExtractionModelOutput.parse(JSON.parse(attempt.output.canonical_body));
    if (body.task !== 'claim') throw new ExtractionAuditError('extraction_audit_conflict');
    await this.validateExtractionSourceTx(tx,task);
    return ExtractionClaimContext.parse({task_id:task.id,attempt_id:attempt.id,body_digest:attempt.output.body_digest,claims:body.claims});
  }

  /** Idempotent child admission under the same fence as the immutable parent.
   * The caller supplies no claim body, generation, source or model identity. */
  async createExtractionJudgeTask(input: {claim_task_id:string}, context: InstallationContext): Promise<ModelTask> {
    const request = z.strictObject({claim_task_id:z.uuidv7()}).parse(input);
    return this.extractionTx(context,async(tx,policy)=>{
      const task = await this.extractionRecordTx(tx,'ModelTask',request.claim_task_id,ModelTask);
      await this.authorizeEpisodesTx(tx,[task.source_id],policy);
      await this.extractionClaimContextTx(tx,task.id);
      const rows = await tx.run(`MATCH (p:ExtractionPipeline {id:$id}) RETURN p.judge_task_id AS judge`,{id:task.id});
      if (!rows.records[0]) throw new ExtractionAuditError('extraction_audit_conflict');
      const existing = rows.records[0].get('judge');
      if (existing) return this.extractionRecordTx(tx,'ModelTask',z.uuidv7().parse(existing),ModelTask);
      await this.writableExtractionGenerationTx(tx,task.generation_id);
      await this.extractionNotCoveredTx(tx,task);
      const now = Math.max(task.updated_at,this.clock());
      const judge = ModelTask.parse({...task,id:uuidv7(),kind:'judge_claims',attempt_id:null,state:'queued',lease:null,policy_context:null,version:0,attempts:0,created_at:now,updated_at:now});
      await tx.run(`CREATE (:ModelTask {id:$id,work_key:$key,generation_id:$generation,source_id:$source,source_ingest_seq:$seq,body:$body,state:'queued'})
        WITH 1 AS ignored MATCH (p:ExtractionPipeline {id:$parent}) SET p.judge_task_id=$id`,
        {id:judge.id,key:`${task.id}:judge`,generation:task.generation_id,source:task.source_id,seq:task.source_ingest_seq,body:canonicalExtractionBody(judge),parent:task.id});
      return judge;
    });
  }

  private async extractionDecisionsTx(tx: ManagedTransaction, attempt: ExtractionAttempt): Promise<ExtractionDisposition[]> {
    // Exactly 65 composite point seeks, including one overflow sentinel. There
    // is no suffix-order Top over an unbounded attempt partition.
    const rows = await tx.run(`UNWIND range(0,64) AS index MATCH (d:ExtractionDisposition {judge_attempt_id:$id,claim_index:index})
      USING INDEX SEEK d:ExtractionDisposition(judge_attempt_id,claim_index)
      RETURN d.body AS body ORDER BY index`,{id:attempt.id});
    const values = z.array(ExtractionDisposition).max(64).parse(rows.records.map(row=>JSON.parse(row.get('body'))));
    if (!attempt.output) { if (values.length) throw new ExtractionAuditError('extraction_audit_conflict'); return values; }
    const output = ExtractionModelOutput.parse(JSON.parse(attempt.output.canonical_body));
    if (output.task !== 'judge_claims') throw new ExtractionAuditError('extraction_audit_conflict');
    const premise = await this.extractionRecordTx(tx,'ExtractionJudgeInput',attempt.id,ExtractionJudgeInput);
    const expected = output.decisions.map(d=>({...d,judge_attempt_id:attempt.id,claim_attempt_id:premise.claim_context.attempt_id,claim_body_digest:premise.claim_context.body_digest}));
    if (canonicalExtractionBody(values) !== canonicalExtractionBody(expected)) throw new ExtractionAuditError('extraction_audit_conflict');
    return values;
  }

  async readExtractionDecisions(attemptId: string, context: InstallationContext): Promise<ExtractionDisposition[]> {
    return this.extractionTx(context,async(tx,policy)=>{
      const attempt = await this.extractionRecordTx(tx,'ExtractionAttempt',z.uuidv7().parse(attemptId),ExtractionAttempt);
      await this.authorizeEpisodesTx(tx,[attempt.source_id],policy);
      return this.extractionDecisionsTx(tx,attempt);
    });
  }

  /** Relation adjudication seam. The automatic pipeline never decides that two
   * Facts restate or contradict each other; a calibrated judge supplied later
   * emits those decisions and Fact->Fact INVALIDATES stays non-automatic (D50). */
  private async materializeExtractionPipelineTx(tx: ManagedTransaction, pipeline: {
    claim: ModelTask; claim_attempt: ExtractionAttempt | null; judge: ModelTask | null; judge_attempt: ExtractionAttempt | null; decisions: ExtractionDisposition[];
  }, policy: PolicyState): Promise<boolean> {
    const judge = pipeline.judge_attempt, claim = pipeline.claim_attempt;
    if (!judge || judge.state !== "succeeded" || !judge.output || !claim || claim.state !== "succeeded" || !claim.output || !pipeline.judge) return false;
    const claimOutput = ExtractionModelOutput.parse(JSON.parse(claim.output.canonical_body));
    if (claimOutput.task !== "claim") throw new ExtractionAuditError("extraction_audit_conflict");
    const judgeOutput = ExtractionModelOutput.parse(JSON.parse(judge.output.canonical_body));
    if (judgeOutput.task !== "judge_claims") throw new ExtractionAuditError("extraction_audit_conflict");
    // Only model-reported confidence admits a claim to semantic writes. Audit-only
    // output (no confidence) stays an auditable decision and never becomes a Fact.
    const candidates = pipeline.decisions.flatMap(decision => {
      if (decision.disposition !== "retain" && decision.disposition !== "correct") return [];
      const extracted = claimOutput.claims[decision.claim_index];
      if (!extracted) throw new ExtractionAuditError("extraction_audit_conflict");
      const confidence = decision.confidence ?? extracted.confidence;
      return confidence === undefined ? [] : [{ decision, extracted, confidence }];
    });
    // Per-source custody is written exactly once; readiness requires every covered
    // source to carry custody even when the judge admitted nothing or every claim was refused.
    const custody = extractionBodyDigest([judge.generation_id, judge.source_id]);
    const priorCustody = await tx.run(`MATCH (o:MaterializationOperation {occurrence_key:$key}) RETURN o.result AS result`, { key: custody });
    if (priorCustody.records[0]) return JSON.parse(priorCustody.records[0].get("result")).created === true;
    const source = await this.semanticEpisodeTx(tx, judge.source_id, policy);
    if (judge.source_revision !== source.revision_key || judge.body_digest !== source.content_digest || judge.source_ingest_seq !== source.ingest_seq)
      throw new ExtractionAuditError("extraction_audit_stale");
    const reported = claimOutput.language.toLowerCase();
    const language = /^[a-z]{2,8}(?:-[a-z0-9]{1,8})*$/.test(reported) ? reported : "und";
    const profile = pipeline.judge.model;
    const factIds: string[] = [], refused: string[] = [];
    for (const { decision, extracted, confidence } of candidates) {
      const occurrence = extractionBodyDigest([judge.generation_id, judge.source_id, judge.id, decision.claim_index]);
      const resolutions: SemanticResolution["entity_resolutions"] = [], references: { mention: string; entity_id: string | null }[] = [];
      for (const entity of extracted.entities ?? []) {
        if (references.some(reference => reference.mention === entity.mention)) continue;
        const key = extractionBodyDigest({ generation: judge.generation_id, normalized_name: entity.normalized_name, entity_kind: entity.entity_kind });
        const existing = await tx.run(`MATCH (e:Entity {generation:$generation,entity_key:$key}) RETURN e.id AS id LIMIT 1`, { generation: judge.generation_id, key });
        const known = existing.records[0]?.get("id");
        if (typeof known === "string") { resolutions.push({ status: "existing", mention: entity.mention, entity_id: known }); references.push({ mention: entity.mention, entity_id: known }); }
        else if (!source.content.includes(entity.normalized_name)) { resolutions.push({ status: "unresolved", mention: entity.mention }); references.push({ mention: entity.mention, entity_id: null }); }
        else { const entity_id = uuidv7(); resolutions.push({ status: "new", mention: entity.mention, entity_id, normalized_name: entity.normalized_name, entity_kind: entity.entity_kind, entity_key: key }); references.push({ mention: entity.mention, entity_id }); }
      }
      const time = extracted.time ? semanticClaimTime(extracted.time) : { time_value: source.time.time_value, time_utc: source.time.time_utc, time_precision: "inherited" as const, resolution: "inherited" as const, anchor_time_utc: source.time.time_utc };
      const semantic = { content: extracted.text, content_language: language, sub_kind: extracted.sub_kind ?? "fact", modality: extracted.speech_act ?? "asserted",
        confidence, time, entities: references, subject_keys: null, predicate_text: [...extracted.text.normalize("NFC")].slice(0, 256).join(""),
        scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] }, scope_complete: false,
        evidence: { kind: "source_locus" as const, span: { start: decision.evidence.start, end: decision.evidence.end } } };
      const resolution = SemanticResolution.parse({ entity_resolutions: resolutions, attribution_speakers: [], allow_no_single_locus: false, content_language: language });
      let validated: ValidatedSemanticClaim;
      try {
        validated = validateSemanticClaim(semantic, { generation: judge.generation_id, fact_language_policy: "source", allow_no_single_locus: false,
          episode: { ...source, content_language: language }, entity_resolutions: resolutions, attribution_speakers: [] });
      } catch (error) {
        if (!(error instanceof SemanticClaimValidationError)) throw error;
        // A refused claim is retained as a content-free operation so the pipeline stays idempotent and auditable.
        await tx.run(`CREATE (:MaterializationOperation {id:$id,digest:$digest,result:$result,occurrence_key:$key,source_episode_id:$source,generation:$generation,semantic_profile_id:$profile,fact_id:$fact,link_id:$link})`,
          { id: uuidv7(), digest: extractionBodyDigest(semantic), result: canonicalExtractionBody({ created: false, refused: error.code }), key: occurrence,
            source: source.id, generation: judge.generation_id, profile, fact: `refused:${occurrence}`, link: `refused:${occurrence}` });
        refused.push(error.code);
        continue;
      }
      const allocated = { fact_id: uuidv7(), link_id: uuidv7(), mention_ids: validated.identity.entity_ids.map(() => uuidv7()) };
      await this.writeValidatedFactTx(tx, { validated, resolution, source, generation: judge.generation_id, profile, policy: policy.policy_revision,
        operationId: uuidv7(), digest: extractionBodyDigest(semantic), occurrence, proposalId: null, allocated });
      factIds.push(allocated.fact_id);
    }
    const created = factIds.length > 0;
    await tx.run(`CREATE (:MaterializationOperation {id:$id,digest:$digest,result:$result,occurrence_key:$key,source_episode_id:$source,generation:$generation,semantic_profile_id:$profile,fact_id:$fact,link_id:$link})`,
      { id: uuidv7(), digest: extractionBodyDigest({ generation: judge.generation_id, source: source.id, judge: judge.id, facts: factIds, refused }),
        result: canonicalExtractionBody({ created, facts: factIds.length, refused }), key: custody, source: source.id, generation: judge.generation_id, profile,
        fact: created ? `custody:${source.id}` : `suppressed:${source.id}`, link: created ? `custody:${source.id}` : `suppressed:${source.id}` });
    return created;
  }

  private async readExtractionPipelineTx(tx: ManagedTransaction,id: string): Promise<ExtractionPipeline> {
    const rows = await tx.run(`MATCH (p:ExtractionPipeline {id:$id}) RETURN p.judge_task_id AS judge`,{id});
    if (!rows.records[0]) return {state:'unknown',pipeline_id:id};
    const claim = await this.extractionRecordTx(tx,'ModelTask',id,ModelTask);
    const judgeId = rows.records[0].get('judge');
    const judge = judgeId ? await this.extractionRecordTx(tx,'ModelTask',z.uuidv7().parse(judgeId),ModelTask) : null;
    const terminal = async(task:ModelTask|null) => task?.attempt_id && task.state !== 'leased' ? this.extractionRecordTx(tx,'ExtractionAttempt',task.attempt_id,ExtractionAttempt) : null;
    const claimAttempt = await terminal(claim), judgeAttempt = await terminal(judge);
    const decisions = judgeAttempt ? await this.extractionDecisionsTx(tx,judgeAttempt) : [];
    const materialized = judgeAttempt && judgeAttempt.state === "succeeded"
      ? await this.materializeExtractionPipelineTx(tx, { claim, claim_attempt: claimAttempt, judge, judge_attempt: judgeAttempt, decisions }, await this.receiptLockTx(tx))
      : false;
    return ExtractionPipeline.parse({state:'known',pipeline_id:id,mode:'claim-judge-audit-v1',semantic_writes:materialized,claim,claim_attempt:claimAttempt,judge,judge_attempt:judgeAttempt,decisions});
  }

  async readExtractionPipeline(id: string, context: InstallationContext): Promise<ExtractionPipeline> {
    return this.extractionTx(context,async(tx,policy)=>{
      const value = await this.readExtractionPipelineTx(tx,z.uuidv7().parse(id));
      if (value.state === 'known') await this.authorizeEpisodesTx(tx,[value.claim.source_id],policy);
      return value;
    });
  }

  private checkExtractionCAS(task: ModelTask, version: number): void {
    if (task.version !== version) throw new Error("task_conflict");
  }

  private checkExtractionLease(task: ModelTask, epoch: string): void {
    if (task.state !== "leased" || task.lease?.epoch !== epoch) throw new Error("lease_conflict");
    if (task.lease.writer_epoch !== this.writerEpoch) throw new Error("ownership_lost");
    if (this.clock() >= task.lease.expires_at) throw new Error("lease_expired");
  }

  private async saveExtractionTaskTx(tx: ManagedTransaction, task: ModelTask): Promise<ModelTask> {
    const parsed = ModelTask.parse(task);
    await tx.run(`MATCH (t:ModelTask {id:$id}) SET t.body=$body,t.state=$state,t.attempt_id=$attempt`,
      { id: task.id, body: canonicalExtractionBody(parsed), state: task.state, attempt: task.attempt_id });
    return parsed;
  }

  async leaseModelTask(input: LeaseModelTask, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = LeaseModelTask.parse(input);
    return this.extractionTx(context, async (tx, policy) => {
      const task = await this.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      if (task.state !== "queued" || task.attempts >= 1000) throw new Error("invalid_transition");
      await this.writableExtractionGenerationTx(tx, task.generation_id);
      await this.authorizeEpisodesTx(tx, [task.source_id], policy);
      await this.validateExtractionSourceTx(tx, task);
      await this.extractionNotCoveredTx(tx, task);
      const now = Math.max(task.updated_at, receiptTime.parse(this.clock()));
      const leased = await this.saveExtractionTaskTx(tx, { ...task, state: "leased", version: task.version + 1, attempts: task.attempts + 1, attempt_id: uuidv7(), updated_at: now,
        lease: { worker_id: request.worker_id, epoch: uuidv7(), writer_epoch: this.writerEpoch!, expires_at: now + request.lease_ms },
        policy_context: { revision: policy.policy_revision, authority: "installation" } });
      if (task.kind === 'judge_claims') {
        const parent = await tx.run(`MATCH (p:ExtractionPipeline {judge_task_id:$id}) RETURN p.id AS id`, {id:task.id});
        if (parent.records.length !== 1) throw new ExtractionAuditError('extraction_audit_conflict');
        const pipelineId = z.uuidv7().parse(parent.records[0]!.get('id'));
        const claim = await this.extractionClaimContextTx(tx, pipelineId);
        const premise = ExtractionJudgeInput.parse({task_id:task.id,attempt_id:leased.attempt_id,pipeline_id:pipelineId,
          source_head_revision:await this.extractionHeadTx(tx,task.source_id),policy_revision:policy.policy_revision,claim_context:claim});
        await tx.run(`CREATE (:ExtractionJudgeInput {id:$id,body:$body})`, {id:leased.attempt_id,body:canonicalExtractionBody(premise)});
      }
      return leased;
    });
  }

  /** Source text leaves the database only after lease and current policy checks. */
  async extractionTaskInput(taskId: string, leaseEpoch: string, context: InstallationContext): Promise<{ task: ModelTask; text: string; claim_context?: ExtractionClaimContext }> {
    return this.extractionTx(context, async (tx, policy) => {
      const task = await this.extractionRecordTx(tx, "ModelTask", z.uuidv7().parse(taskId), ModelTask);
      this.checkExtractionLease(task, z.uuidv7().parse(leaseEpoch));
      await this.authorizeEpisodesTx(tx, [task.source_id], policy);
      const source = await this.validateExtractionSourceTx(tx, task);
      if (task.kind === 'judge_claims') {
        const premise = await this.extractionRecordTx(tx,'ExtractionJudgeInput',task.attempt_id!,ExtractionJudgeInput);
        if (premise.policy_revision !== policy.policy_revision || premise.source_head_revision !== await this.extractionHeadTx(tx,task.source_id)) throw new ExtractionAuditError('extraction_audit_stale');
        return {task,text:source.content,claim_context:premise.claim_context};
      }
      return { task, text: source.content };
    });
  }

  private async finishExtractionTx(tx: ManagedTransaction, task: ModelTask, policy: PolicyState,
    outcome: Pick<ExtractionAttempt, "state" | "reason" | "disposition" | "output" | "spans">, requestDigest: string): Promise<ExtractionAttempt> {
    const now = Math.max(task.updated_at, receiptTime.parse(this.clock()));
    const attempt = ExtractionAttempt.parse({ state: outcome.state, reason: outcome.reason, disposition: outcome.disposition, output: outcome.output, spans: outcome.spans,
      id: task.attempt_id ?? uuidv7(), task_id: task.id,
      generation_id: task.generation_id, source_id: task.source_id, source_revision: task.source_revision, source_ingest_seq: task.source_ingest_seq, body_digest: task.body_digest,
      created_at: task.updated_at, updated_at: now, lease: task.lease, policy_context: { revision: policy.policy_revision, authority: "installation" } });
    await tx.run(`CREATE (:ExtractionAttempt {id:$id,task_id:$task,generation_id:$generation,source_id:$source,source_ingest_seq:$seq,state:$state,request_digest:$digest,body:$body})`,
      { id: attempt.id, task: task.id, generation: task.generation_id, source: task.source_id, seq: task.source_ingest_seq, state: attempt.state, digest: requestDigest, body: canonicalExtractionBody(attempt) });
    await this.saveExtractionTaskTx(tx, { ...task, state: attempt.state, attempt_id: attempt.id, lease: null, version: task.version + 1, updated_at: now,
      attempts: task.attempts + (task.attempt_id === null ? 1 : 0), policy_context: attempt.policy_context });
    return attempt;
  }

  /** Exact completion replay returns the stored immutable outcome. Neither the
   * submitted output nor caller-selected policy context is ever an authority. */
  async recordExtractionAttempt(input: CompleteExtractionAttempt, context: InstallationContext): Promise<ExtractionAttempt> {
    requireInstallation(context);
    const request = CompleteExtractionAttempt.parse(input), digest = extractionBodyDigest(request);
    return this.extractionTx(context, async (tx, policy) => {
      const oldRows = await tx.run<{ body: string; digest: string }>(`MATCH (a:ExtractionAttempt {id:$id}) RETURN a.body AS body,a.request_digest AS digest`, { id: request.id });
      const old = oldRows.records[0];
      if (old) {
        if (old.get("digest") !== digest) throw new Error("attempt_conflict");
        const attempt = ExtractionAttempt.parse(JSON.parse(old.get("body")));
        if (attempt.output) await this.authorizeEpisodesTx(tx, [attempt.source_id], policy);
        return attempt;
      }
      const task = await this.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      this.checkExtractionLease(task, request.lease_epoch);
      if (task.attempt_id !== request.id) throw new Error("attempt_conflict");
      await this.writableExtractionGenerationTx(tx, task.generation_id);
      const source = await this.validateExtractionSourceTx(tx, task);
      try { await this.authorizeEpisodesTx(tx, [task.source_id], policy); }
      catch (error) {
        if (!(error instanceof ReceiptError) || error.code !== "policy_denied") throw error;
        return this.finishExtractionTx(tx, task, policy, { state: "cancelled", reason: "policy_denied", output: null, disposition: null, spans: [] }, digest);
      }
      if (request.output) {
        const validated = validateModelOutput(JSON.parse(request.output.canonical_body), task.kind);
        if (canonicalExtractionBody(validated.output) !== canonicalExtractionBody(request.output) || validated.disposition !== request.disposition
          || canonicalExtractionBody(request.spans) !== canonicalExtractionBody(validated.spans)) throw new Error("output_mismatch");
        validateSourceSpans(source.content, request.spans);
      }
      if (task.kind === 'judge_claims') {
        const premise = await this.extractionRecordTx(tx,'ExtractionJudgeInput',task.attempt_id!,ExtractionJudgeInput);
        if (premise.policy_revision !== policy.policy_revision || premise.source_head_revision !== await this.extractionHeadTx(tx,task.source_id)) {
          return this.finishExtractionTx(tx,task,policy,{state:'failed',reason:'premises_changed',disposition:null,output:null,spans:[]},digest);
        }
        const parent = await this.extractionClaimContextTx(tx,premise.pipeline_id);
        if (canonicalExtractionBody(parent) !== canonicalExtractionBody(premise.claim_context) || premise.task_id !== task.id || premise.attempt_id !== task.attempt_id) throw new ExtractionAuditError('extraction_audit_conflict');
        if (request.output) {
          const body = ExtractionModelOutput.parse(JSON.parse(request.output.canonical_body));
          if (body.task !== 'judge_claims' || body.claim_body_digest !== parent.body_digest || body.decisions.length !== parent.claims.length
            || body.decisions.some((d,i)=>d.claim_index !== i || canonicalExtractionBody(d.evidence) !== canonicalExtractionBody(parent.claims[i]!.evidence))) {
            return this.finishExtractionTx(tx,task,policy,{state:'failed',reason:'provider_mismatch',disposition:null,output:null,spans:[]},digest);
          }
          for (const d of body.decisions) {
            const decision = ExtractionDisposition.parse({...d,judge_attempt_id:task.attempt_id,claim_attempt_id:parent.attempt_id,claim_body_digest:parent.body_digest});
            await tx.run(`CREATE (:ExtractionDisposition {judge_attempt_id:$id,claim_index:$index,body:$body})`,
              {id:task.attempt_id,index:neo4j.int(d.claim_index),body:canonicalExtractionBody(decision)});
          }
        }
      }
      return this.finishExtractionTx(tx, task, policy, request, digest);
    });
  }

  async cancelModelTask(input: ModelTaskCAS, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = ModelTaskCAS.parse(input);
    return this.extractionTx(context, async (tx, policy) => {
      const task = await this.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      if (!["queued", "leased"].includes(task.state)) throw new Error("invalid_transition");
      await this.finishExtractionTx(tx, task, policy, { state: "cancelled", reason: "cancelled", output: null, disposition: null, spans: [] }, extractionBodyDigest({ action: "cancel", ...request }));
      return this.extractionRecordTx(tx, "ModelTask", task.id, ModelTask);
    });
  }

  async settleModelTask(input: SettleModelTask, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = SettleModelTask.parse(input);
    return this.extractionTx(context, async (tx, policy) => {
      const task = await this.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      if (task.state !== "leased" || task.lease?.epoch !== request.lease_epoch) throw new Error("lease_conflict");
      if (request.reason === "expired" && this.clock() < task.lease.expires_at) throw new Error("lease_not_expired");
      if (request.reason === "worker_lost" && task.lease.writer_epoch === this.writerEpoch) throw new Error("worker_still_owned");
      await this.finishExtractionTx(tx, task, policy, { state: request.reason, reason: request.reason, output: null, disposition: null, spans: [] }, extractionBodyDigest(request));
      return this.extractionRecordTx(tx, "ModelTask", task.id, ModelTask);
    });
  }

  private async extractionNotCoveredTx(tx: ManagedTransaction, task: ModelTask): Promise<void> {
    const rows = await tx.run(`MATCH (c:ExtractionCoverage {generation_id:$generation}) WHERE c.covered_ingest_seq >= $seq RETURN c.key AS key LIMIT 1`, { generation: task.generation_id, seq: task.source_ingest_seq });
    if (rows.records.length) throw new Error("coverage_frozen");
  }

  async retryModelTask(input: ModelTaskCAS, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = ModelTaskCAS.parse(input);
    return this.extractionTx(context, async (tx, policy) => {
      const task = await this.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      if (!["failed", "expired", "worker_lost"].includes(task.state)) throw new Error("invalid_transition");
      await this.writableExtractionGenerationTx(tx, task.generation_id);
      await this.authorizeEpisodesTx(tx, [task.source_id], policy);
      await this.validateExtractionSourceTx(tx, task);
      await this.extractionNotCoveredTx(tx, task);
      return this.saveExtractionTaskTx(tx, { ...task, state: "queued", attempt_id: null, lease: null, policy_context: null, version: task.version + 1, updated_at: Math.max(task.updated_at, this.clock()) });
    });
  }

  /** Only immutable failed/cancelled attempts can justify a content-free omission.
   * A lost or expired lease is unresolved work, not an extraction outcome. */
  private extractionPipelineOmission(pipeline: ExtractionPipeline) {
    if (pipeline.state !== "known") return null;
    for (const [stage, task, attempt] of [
      ["claim", pipeline.claim, pipeline.claim_attempt],
      ["judge", pipeline.judge, pipeline.judge_attempt],
    ] as const) {
      if (stage === "judge" && pipeline.claim.state !== "succeeded") continue;
      if (task && attempt && (task.state === "failed" || task.state === "cancelled")
        && attempt.state === task.state && attempt.id === task.attempt_id && attempt.task_id === task.id) {
        return { pipeline_id: pipeline.pipeline_id, stage, attempt_id: attempt.id, state: attempt.state, reason: attempt.reason };
      }
    }
    return null;
  }

  /** Each explicit advance seals at most 256 terminal outcomes, including
   * content-free omissions. Retry is then forbidden for that sealed work.
   * The shared generation cursor is the minimum of the two partition cursors;
   * these are audit coverage partitions, not embedding/recall readiness gates. */
  async recordExtractionCoverage(input: AdvanceExtractionCoverage, context: InstallationContext): Promise<Coverage> {
    requireInstallation(context);
    const request = AdvanceExtractionCoverage.parse(input);
    return this.extractionTx(context, async tx => {
      const generation = await this.writableExtractionGenerationTx(tx, request.generation_id);
      const key = `${generation.id}:${request.partition}`;
      const rows = await tx.run<{ body: string }>(`MATCH (c:ExtractionCoverage {key:$key}) RETURN c.body AS body`, { key });
      const prior = rows.records[0] ? Coverage.parse(JSON.parse(rows.records[0].get("body"))) : null;
      const covered = prior?.covered_ingest_seq ?? 0;
      if (covered !== request.expected_covered_ingest_seq) throw new Error("coverage_conflict");
      if (request.covered_ingest_seq < covered) throw new Error("coverage_regression");
      if (request.covered_ingest_seq - covered > 256) throw new Error("coverage_batch_too_large");
      const meta = await tx.run<{ seq: number }>(`MATCH (m:Meta {key:'meta'}) RETURN m.ingest_seq AS seq`);
      const required = receiptTime.parse(meta.records[0]?.get("seq"));
      if (request.covered_ingest_seq > required) throw new Error("coverage_exceeds_required");
      const prefix = await tx.run<{ source: string; seq: number; task: string | null; attempt: string | null }>(
        `MATCH (e:Element:Episode) WHERE e.ingest_seq > $from AND e.ingest_seq <= $to
         OPTIONAL MATCH (t:ModelTask {work_key:$generation+':'+e.id})
         OPTIONAL MATCH (a:ExtractionAttempt {id:t.attempt_id})
         RETURN e.id AS source,e.ingest_seq AS seq,t.body AS task,a.body AS attempt ORDER BY seq LIMIT $limit`,
        { generation: generation.id, from: covered, to: request.covered_ingest_seq, limit: neo4j.int(256) });
      if (prefix.records.length !== request.covered_ingest_seq - covered) throw new Error("coverage_hole");
      let omissionDigest = prior?.omission_digest ?? extractionBodyDigest([]);
      for (const [index, row] of prefix.records.entries()) {
        if (row.get("seq") !== covered + index + 1 || !row.get("task")) throw new Error("coverage_hole");
        const task = ModelTask.parse(JSON.parse(row.get("task")!));
        if (task.pipeline && (task.state === "queued" || task.state === "leased" || !row.get("attempt"))) throw new ExtractionAuditError("extraction_audit_incomplete");
        if (!row.get("attempt")) throw new Error("coverage_hole");
        const attempt = ExtractionAttempt.parse(JSON.parse(row.get("attempt")!));
        if (task.state === "queued" || task.state === "leased" || attempt.state !== task.state || attempt.id !== task.attempt_id
          || attempt.task_id !== task.id || attempt.source_id !== row.get("source") || attempt.source_ingest_seq !== row.get("seq") || attempt.generation_id !== generation.id) throw new Error("coverage_hole");
        if (task.pipeline) {
          const pipeline = await this.readExtractionPipelineTx(tx,task.id);
          const omission = this.extractionPipelineOmission(pipeline);
          if (omission) omissionDigest = extractionBodyDigest({ prior: omissionDigest, ...omission });
          else {
            if (pipeline.state !== 'known' || pipeline.claim.state !== 'succeeded' || pipeline.judge?.state !== 'succeeded') throw new ExtractionAuditError('extraction_audit_incomplete');
            // These rows seal audit work only, never materialization/embedding readiness.
            omissionDigest = extractionBodyDigest({prior:omissionDigest,pipeline_id:task.id,judge_attempt_id:pipeline.judge_attempt!.id,decisions:pipeline.decisions.map(d=>d.disposition)});
          }
        } else if (attempt.state !== "succeeded" || !["retain", "correct"].includes(attempt.disposition ?? "")) omissionDigest = extractionBodyDigest({ prior: omissionDigest, id: attempt.id, seq: attempt.source_ingest_seq, state: attempt.state, reason: attempt.reason, disposition: attempt.disposition });
      }
      const now = Math.max(generation.updated_at, prior?.updated_at ?? 0, receiptTime.parse(this.clock()));
      const value = Coverage.parse({ generation_id: generation.id, partition: request.partition, required_ingest_seq: required, covered_ingest_seq: request.covered_ingest_seq, omission_digest: omissionDigest, updated_at: now });
      await tx.run(`MERGE (c:ExtractionCoverage {key:$key}) SET c.generation_id=$generation,c.partition=$partition,c.covered_ingest_seq=$covered,c.body=$body`,
        { key, generation: generation.id, partition: request.partition, covered: value.covered_ingest_seq, body: canonicalExtractionBody(value) });
      const other = await tx.run<{ covered: number }>(`MATCH (c:ExtractionCoverage {key:$key}) RETURN c.covered_ingest_seq AS covered`, { key: `${generation.id}:${request.partition === "episodes" ? "active_extraction" : "episodes"}` });
      const cursor = Math.min(value.covered_ingest_seq, other.records[0]?.get("covered") ?? 0);
      if (cursor < generation.covered_ingest_seq) throw new Error("coverage_regression");
      const next = Generation.parse({ ...generation, covered_ingest_seq: cursor, updated_at: now });
      await tx.run(`MATCH (g:ExtractionGeneration {id:$id}) SET g.covered_ingest_seq=$covered,g.body=$body`, { id: generation.id, covered: cursor, body: canonicalExtractionBody(next) });
      return value;
    });
  }

  private async extractionSelectionTx(tx: ManagedTransaction): Promise<ExtractionSelection> {
    const rows = await tx.run(`MATCH (s:Meta {key:'extraction_selector'})
      RETURN s.generation_id AS generation_id,s.selector_version AS selector_version`);
    const parsed = ExtractionSelection.safeParse(rows.records[0]?.toObject());
    if (!parsed.success) throw new GenerationReadinessError("selector_unavailable");
    if (parsed.data.generation_id) {
      const active = await this.extractionRecordTx(tx, "ExtractionGeneration", parsed.data.generation_id, Generation);
      if (active.state !== "active") throw new GenerationReadinessError("generation_not_active");
    }
    return parsed.data;
  }

  async readExtractionSelection(context: InstallationContext): Promise<ExtractionSelection> {
    return this.extractionTx(context, tx => this.extractionSelectionTx(tx));
  }

  private async checkExtractionSelectionTx(tx: ManagedTransaction, request: SelectExtractionGeneration): Promise<ExtractionSelection> {
    const selection = await this.extractionSelectionTx(tx);
    if (selection.selector_version !== request.expected_selector_version) throw new GenerationReadinessError("selector_version_conflict");
    if (selection.generation_id !== request.expected_generation_id) throw new GenerationReadinessError("selector_conflict");
    return selection;
  }

  private async extractionCoverageTx(tx: ManagedTransaction, id: string): Promise<Coverage[]> {
    const rows = await tx.run(`UNWIND ['episodes','active_extraction'] AS partition
      MATCH (c:ExtractionCoverage {key:$id+':'+partition})
      RETURN partition,c.generation_id AS generation,c.covered_ingest_seq AS covered,c.body AS body ORDER BY partition`, { id });
    if (rows.records.length !== 2) throw new GenerationReadinessError("coverage_unavailable");
    return rows.records.map(row => {
      const value = Coverage.parse(JSON.parse(row.get("body")));
      if (value.partition !== row.get("partition") || value.generation_id !== id || row.get("generation") !== id
        || value.covered_ingest_seq !== row.get("covered")) throw new GenerationReadinessError("coverage_unavailable");
      return value;
    });
  }

  /** Audit coverage is necessary, never sufficient for derived activation.
   * No caller-supplied readiness flags can stand in for missing serving proofs. */
  async cutoverExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext): Promise<Generation> {
    requireInstallation(context);
    const request = SelectExtractionGeneration.parse(input);
    return this.extractionTx(context, async (tx, policy) => {
      await this.checkExtractionSelectionTx(tx, request);
      const target = await this.extractionRecordTx(tx, "ExtractionGeneration", request.generation_id, Generation);
      if (target.state !== "catching_up" && target.state !== "active") throw new Error("generation_not_caught_up");
      const values = await this.extractionCoverageTx(tx, target.id);
      const meta = await tx.run(`MATCH (m:Meta {key:'meta'}) MATCH (g:ExtractionGeneration {id:$id})
        RETURN m.ingest_seq AS seq,g.source_high_watermark AS watermark,g.covered_ingest_seq AS covered`, { id: target.id });
      const row = meta.records[0]!, live = receiptTime.parse(row.get("seq"));
      const watermark = receiptTime.safeParse(row.get("watermark"));
      if (!watermark.success) throw new GenerationReadinessError("generation_watermark_unavailable");
      const pending = await tx.run(`MATCH (t:ModelTask {generation_id:$id}) WHERE t.state IN ['queued','leased'] RETURN t.id LIMIT 1`, { id: target.id });
      if (pending.records.length) throw new GenerationReadinessError("generation_work_in_flight");
      if (target.covered_ingest_seq !== live || row.get("covered") !== live || watermark.data > live
        || values.some(value => value.covered_ingest_seq !== live || value.required_ingest_seq !== live)) throw new GenerationReadinessError("coverage_incomplete");
      const conducting = await tx.run(`MATCH (m:Meta {key:'meta'})
        MATCH (c:ConductingArcCoverage {stream:'extraction',generation:$id})
        RETURN m.conducting_arc_ready AS ready,c.state AS state`, { id: target.id });
      if (conducting.records[0]?.get("ready") !== true || conducting.records[0]?.get("state") !== "COMPLETE") throw new GraphAccessError("degree_probe_unavailable");
      const retained = await this.conductingSnapshotTx(tx, 10000);
      if (retained.report.truncated || retained.report.issues.length) throw new GraphAccessError("degree_probe_unavailable");
      // Check the existing access-index prerequisites; this does not certify
      // nonexistent generation-scoped derived indexes or an ordered query plan.
      const indexes = await tx.run(`SHOW INDEXES YIELD name,state WHERE name IN $names RETURN name,state`, {
        names: ["conducting_arc_source_link", "conducting_arc_coverage", "extraction_coverage_key", "extraction_generation_id", "meta_key", "fact_generation_id", "entity_generation_key"],
      });
      if (indexes.records.length !== 7 || indexes.records.some(index => index.get("state") !== "ONLINE")) throw new GraphAccessError("ordered_probe_unavailable");
      // Serving readiness is derived only from persisted, generation-scoped
      // materialization custody. Audit success without an accepted proposal and
      // consumption remains insufficient. Every source in the covered prefix
      // must have materialization custody or an explicitly sealed omission.
      const sources = await tx.run(`MATCH (e:Element:Episode) WHERE e.ingest_seq > 0 AND e.ingest_seq <= $seq
        OPTIONAL MATCH (o:MaterializationOperation {generation:$generation,source_episode_id:e.id})
        OPTIONAL MATCH (t:ModelTask {work_key:$generation+':'+e.id})
        RETURN e.id AS id,count(o) AS operations,t.id AS task LIMIT 257`, { seq: live, generation: target.id });
      // The source partition is bounded at 256, but each source can retain up to
      // 64 claims. Derived custody rows use their own bounded overflow sentinel.
      const maxDerived = 256 * 64;
      const operationRows = await tx.run(`MATCH (o:MaterializationOperation {generation:$generation})
        RETURN o.source_episode_id AS source,o.semantic_profile_id AS profile,o.fact_id AS fact,o.link_id AS link LIMIT $limit`, { generation: target.id, limit: neo4j.int(maxDerived + 1) });
      let missing = false;
      for (const source of sources.records) if (source.get("operations") === 0) {
        // Both coverage partitions above pin this terminal attempt. Retrying it
        // is forbidden after sealing, so no fabricated materialization is needed.
        const task = source.get("task");
        if (!task || !this.extractionPipelineOmission(await this.readExtractionPipelineTx(tx, task))) missing = true;
      }
      const overflow = sources.records.length > 256 || operationRows.records.length > maxDerived;
      const malformed = operationRows.records.some(row => typeof row.get("source") !== "string" || typeof row.get("fact") !== "string" || typeof row.get("link") !== "string");
      const links = await tx.run(`MATCH (f:Element:Fact)-[l:DERIVED_FROM]->(e:Element:Episode)
        WHERE l.generation=$generation RETURN f.id AS fact,e.id AS source,l.id AS link,l.generation AS generation LIMIT $limit`, { generation: target.id, limit: neo4j.int(maxDerived + 1) });
      const linkBad = links.records.length > maxDerived || links.records.some(row => row.get("generation") !== target.id || !row.get("fact") || !row.get("source") || !row.get("link"));
      // Several Facts may mention one Entity; the witness requirement is per distinct entity.
      const entities = await tx.run(`MATCH (f:Element:Fact {generation:$generation}) UNWIND coalesce(f.entity_ids,[]) AS entity
        WITH DISTINCT entity OPTIONAL MATCH (w:EntityWitness {entity_id:entity,generation:$generation,policy_revision:$policy})
        RETURN entity,count(w) AS witnesses LIMIT $limit`, { generation: target.id, policy: neo4j.int(policy.policy_revision), limit: neo4j.int(maxDerived + 1) });
      const witnessBad = entities.records.length > maxDerived || entities.records.some(row => row.get("witnesses") !== 1);
      const indexesReady = indexes.records.length === 7 && indexes.records.every(index => index.get("state") === "ONLINE");
      let embeddingCoverageBad = false;
      if (this.embeddingProvider) {
        const profileId = embeddingProfileId(this.embeddingProvider.profile);
        const configured = await tx.run(`MATCH (p:EmbeddingProfile) RETURN p.id AS id`);
        const vectors = await tx.run(`MATCH (v:EmbeddingVector {profile_id:$profile}) RETURN v.episode_id AS episode`, { profile: profileId });
        const vectorEpisodes = new Set(vectors.records.map(record => record.get("episode")));
        embeddingCoverageBad = configured.records.length !== 1 || configured.records[0]?.get("id") !== profileId
          || sources.records.some(source => !vectorEpisodes.has(source.get("id")));
      }
      if (missing || overflow || malformed || linkBad || witnessBad || !indexesReady || embeddingCoverageBad) {
        throw new GenerationReadinessError("activation_prerequisite_unavailable", [
          ...(embeddingCoverageBad ? ["selected_model_embedding_coverage"] : []),
          ...(!indexesReady ? ["generation_scoped_indexes"] : []),
          ...(missing || overflow || malformed ? ["derived_authority_and_links"] : []),
          ...(witnessBad ? ["entity_witness_policy_coverage"] : []),
          ...(linkBad ? ["invalidation_mapping_equivalence"] : []),
        ]);
      }
      const activated = Generation.parse({ ...target, state: "active", updated_at: Math.max(target.updated_at, this.clock()) });
      await tx.run(`MATCH (g:ExtractionGeneration {id:$id}), (s:Meta {key:'extraction_selector'})
        SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=s.selector_version+1`, { id: target.id, body: canonicalExtractionBody(activated) });
      await tx.run(`MERGE (r:DerivedServingReadiness {generation:$generation})
        SET r.state='COMPLETE',r.profile_id=$profile,r.policy_revision=$policy,r.covered_ingest_seq=$seq,r.link_revision=$revision`, { generation: target.id,
          profile: this.embeddingProvider ? embeddingProfileId(this.embeddingProvider.profile) : "embeddings-disabled-v1", policy: policy.policy_revision, seq: live, revision: row.get("covered") });
      return activated;
    });
  }

  /** Rollback reopening is not activation. Fence it with the same server epoch
   * and advance that epoch atomically, so stale rollback retries cannot reopen
   * a target during a later selection era. */
  async rollbackExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext): Promise<Generation> {
    requireInstallation(context);
    const request = SelectExtractionGeneration.parse(input);
    return this.extractionTx(context, async tx => {
      const selection = await this.checkExtractionSelectionTx(tx, request);
      const target = await this.extractionRecordTx(tx, "ExtractionGeneration", request.generation_id, Generation);
      if (target.state !== "retired") throw new Error("rollback_requires_retired");
      if (selection.selector_version === Number.MAX_SAFE_INTEGER) throw new GenerationReadinessError("selector_version_exhausted");
      const reopened = Generation.parse({ ...target, state: "catching_up", updated_at: Math.max(target.updated_at, this.clock()) });
      await tx.run(`MATCH (g:ExtractionGeneration {id:$id}), (m:Meta {key:'meta'}), (s:Meta {key:'extraction_selector'})
        SET g.body=$body,g.state='catching_up',g.source_high_watermark=m.ingest_seq,
          s.selector_version=s.selector_version+1`, { id: target.id, body: canonicalExtractionBody(reopened) });
      return reopened;
    });
  }

  /** Acquire a new pin or revalidate an old epoch under current policy and the
   * writer barrier. An unversioned legacy selector is never a valid reader pin. */
  async readExtractionCoverage(input: ReadExtractionCoverage | string, context: InstallationContext): Promise<ExtractionCoverageRead> {
    requireInstallation(context);
    const request = ReadExtractionCoverage.parse(typeof input === "string" ? { generation_id: input } : input);
    return this.extractionTx(context, async (tx, policy) => {
      const selection = await this.extractionSelectionTx(tx);
      if (request.expected_selector_version !== undefined && request.expected_selector_version !== selection.selector_version) throw new GenerationReadinessError("selector_version_conflict");
      if (selection.generation_id !== request.generation_id) throw new Error("generation_not_selected");
      const generation = await this.extractionRecordTx(tx, "ExtractionGeneration", request.generation_id, Generation);
      const values = await this.extractionCoverageTx(tx, generation.id);
      const required = Math.min(...values.map(value => value.required_ingest_seq));
      const covered = Math.min(...values.map(value => value.covered_ingest_seq));
      if (covered !== generation.covered_ingest_seq) throw new Error("coverage_stale");
      return ExtractionCoverageRead.parse({ generation_id: generation.id, selector_version: selection.selector_version,
        required_ingest_seq: required, covered_ingest_seq: covered,
        omission_digest: extractionBodyDigest(values.map(value => [value.partition, value.omission_digest])),
        policy_revision: policy.policy_revision, read_at: this.clock() });
    });
  }

  async admitDream(input: RpcDreamAdmitParams, context: InstallationContext): Promise<RpcDreamJob> {
    requireInstallation(context);
    const request = RpcDreamAdmitParams.parse(input);
    return this.extractionTx(context, async (tx, policy) => {
      if (policy.policy_revision !== request.policy_revision) throw new Error("dream_fence_stale");
      const meta = await tx.run(`MATCH (m:Meta {key:'meta'}) RETURN m.structure_revision AS structure,m.policy_revision AS policy,m.ingest_seq AS ingest`);
      const m = meta.records[0];
      if ((m?.get("structure") ?? 0) !== request.structure_revision || (m?.get("ingest") ?? 0) < request.covered_ingest_seq) throw new Error("dream_fence_stale");
      await this.authorizeEpisodesTx(tx, request.source_ids, policy);
      const rows = await tx.run(`MATCH (e:Element:Episode) WHERE e.id IN $ids RETURN e.id AS id,e.revision_key AS revision,e.content AS content,e.ingest_seq AS seq`, { ids: request.source_ids });
      if (rows.records.length !== request.source_ids.length) throw new Error("dream_source_missing");
      const receipts = rows.records.map(r => ({ id:r.get("id"), revision:r.get("revision"), body_digest:sha256(Buffer.from(r.get("content"), "utf8")), ingest_seq:r.get("seq"), allowed:true }));
      const body = canonicalExtractionBody({ ...request, source_receipts: receipts });
      const jobId = `dream-${sha256(Buffer.from(body))}`;
      const old = await tx.run<{ body:string }>(`MATCH (j:DreamJob {id:$id}) RETURN j.body AS body`, {id:jobId});
      if (old.records[0]) return RpcDreamJob.parse(JSON.parse(old.records[0].get("body")));
      const job = RpcDreamJob.parse({ ...request, job_id:jobId, source_receipts:receipts, state:"queued", version:0, lease:null, semantic_writes:false, authority:"none" });
      await tx.run(`CREATE (:DreamJob {id:$id,body:$body,state:'queued',version:0})`, {id:jobId,body:canonicalExtractionBody(job)});
      return job;
    });
  }
  async dreamStatus(id: string, context: InstallationContext): Promise<RpcDreamJob> {
    requireInstallation(context);
    return this.extractionTx(context, async tx => { const rows = await tx.run<{body:string}>(`MATCH (j:DreamJob {id:$id}) RETURN j.body AS body`,{id}); if (!rows.records[0]) throw new Error("dream_job_missing"); return RpcDreamJob.parse(JSON.parse(rows.records[0].get("body"))); });
  }
  async leaseDream(input: RpcDreamLeaseParams, context: InstallationContext): Promise<RpcDreamJob> { return this.mutateDream(input, context, false); }
  async expireDream(input: RpcDreamExpireParams, context: InstallationContext): Promise<RpcDreamJob> { return this.mutateDream(input, context, true); }
  async executeDream(input: RpcDreamExecuteParams, context: InstallationContext): Promise<RpcDreamJob> {
    requireInstallation(context);
    return this.extractionTx(context, async tx => {
      const rows = await tx.run<{body:string}>(`MATCH (j:DreamJob {id:$id}) RETURN j.body AS body`, { id: input.job_id });
      if (!rows.records[0]) throw new Error("dream_job_missing");
      const job = RpcDreamJob.parse(JSON.parse(rows.records[0].get("body")));
      if (job.version !== input.expected_version) throw new Error("dream_version_conflict");
      if (job.state !== "queued") throw new Error("dream_not_queued");
      const exportBytes = canonicalExtractionBody({ phase: job.phase, fence: { extraction_generation: job.extraction_generation, covered_ingest_seq: job.covered_ingest_seq, structure_revision: job.structure_revision, policy_revision: job.policy_revision }, source_receipts: job.source_receipts });
      const exportDigest = sha256(Buffer.from(exportBytes));
      const sourceIds = job.source_receipts.map(receipt => receipt.id);
      const arcRows = await tx.run<{ from: string; to: string; weight: number }>(`MATCH (a:Element)-[l]->(b:Element)
        WHERE a.id IN $ids AND b.id IN $ids AND type(l) IN ['MENTIONS','RELATES_TO']
        RETURN a.id AS from,b.id AS to,toFloat(coalesce(l.weight,1.0)) AS weight ORDER BY from,to,l.id LIMIT 500000`, { ids: sourceIds });
      const arcs = arcRows.records.map(row => ({ from: row.get('from'), to: row.get('to'), weight: row.get('weight') }));
      const adapterInput = { operation_id: job.job_id, export_bytes: exportBytes, export_digest: exportDigest, source_receipts: job.source_receipts, graph: { node_count: sourceIds.length, arc_count: arcs.length, byte_count: Buffer.byteLength(exportBytes), nodes: sourceIds, arcs } };
      try {
        if (!this.dreamLeidenAdapter) throw new Error("dream_adapter_unavailable");
        const result = await this.dreamLeidenAdapter.execute(adapterInput);
        job.state = "succeeded"; job.version++;
        (job as any).execution = { state: "succeeded", attempt: 1, result, retryable: false };
        await tx.run(`MATCH (j:DreamJob {id:$id}) SET j.body=$body,j.state=$state,j.version=$version`, { id:job.job_id, body:canonicalExtractionBody(job), state:job.state, version:job.version });
        return job;
      } catch (error) {
        if (!(error instanceof DreamAdapterError) && (error as Error).message !== "dream_adapter_unavailable") throw error;
        job.state = "unknown"; job.version++;
        (job as any).execution = { state: "unknown", attempt: 1, error: error instanceof Error ? error.message : "dream_adapter_unavailable", retryable: false };
        await tx.run(`MATCH (j:DreamJob {id:$id}) SET j.body=$body,j.state=$state,j.version=$version`, { id:job.job_id, body:canonicalExtractionBody(job), state:job.state, version:job.version });
        return job;
      }
    });
  }
  private async mutateDream(input: RpcDreamLeaseParams | RpcDreamExpireParams, context: InstallationContext, expire: boolean): Promise<RpcDreamJob> {
    requireInstallation(context); return this.extractionTx(context, async tx => {
      const rows = await tx.run<{body:string}>(`MATCH (j:DreamJob {id:$id}) RETURN j.body AS body`,{id:input.job_id}); if (!rows.records[0]) throw new Error("dream_job_missing");
      const job = RpcDreamJob.parse(JSON.parse(rows.records[0].get("body"))); if (job.version !== input.expected_version) throw new Error("dream_version_conflict");
      if (expire) { const request = input as RpcDreamExpireParams; if (job.state !== "leased" || !job.lease || job.lease.epoch !== request.lease_epoch) throw new Error("dream_lease_fenced"); job.state="queued"; job.lease=null; }
      else { const request = input as RpcDreamLeaseParams; if (job.state !== "queued") throw new Error("dream_not_queued"); job.state="leased"; job.lease={worker_id:request.worker_id,epoch:`${job.job_id}:${job.version+1}`,expires_at:Date.now()+request.lease_ms}; }
      job.version++; await tx.run(`MATCH (j:DreamJob {id:$id}) SET j.body=$body,j.state=$state,j.version=$version`,{id:job.job_id,body:canonicalExtractionBody(job),state:job.state,version:job.version}); return job;
    });
  }

  async close(): Promise<void> {
    await this.driver.close();
  }

  private async run<Row extends RecordShape>(
    cypher: string,
    params: QueryParameters = {},
  ): Promise<Row[]> {
    const res = await this.driver.executeQuery<Row>(cypher, params, {
      database: this.database,
    });
    return recordsToObjects(res.records);
  }
}

function recordsToObjects<Row extends RecordShape>(
  records: Neo4jRecord<Row>[],
): Row[] {
  return records.map((record) => record.toObject());
}

function nodeProps(node: ElementNode): ElementProperties {
  return node.properties;
}

function relProps(rel: LinkRelationship): LinkProperties {
  return rel.properties;
}

const StoredHash = z.string().regex(/^[0-9a-f]{64}$/).nullable();
const HistoricalStoredElement = HistoricalElement.extend({ id: z.uuidv7() });

/** Frozen post167/pre194 structural read: semantic eligibility is reported
 * separately, not used to reject intact historical bytes. No serving bypass. */
function decodeHistoricalElement(p: ElementProperties): MemoryElement {
  return HistoricalStoredElement.parse({
    id: p["id"], schema: p["schema"], content: p["content"], mass: p["mass"],
    properties: typeof p["properties"] === "string" ? JSON.parse(p["properties"]) : undefined,
    ...(p["time_value"] != null || p["time_precision"] != null
      ? { time: { value: p["time_value"], precision: p["time_precision"] } } : {}),
    origin: { source: p["origin_source"], session: p["origin_session"], actor: p["origin_actor"], record: p["origin_record"] },
  });
}

function toElement(p: ElementProperties): MemoryElement {
  return MemoryElement.parse({
    id: p["id"],
    schema: p["schema"],
    ...(p["time_value"] ? { time: { value: p["time_value"], precision: p["time_precision"] } } : {}),
    content: p["content"],
    origin: {
      source: p["origin_source"],
      session: p["origin_session"],
      actor: p["origin_actor"],
      record: p["origin_record"],
    },
    mass: p["mass"],
    properties: {
      ...JSON.parse((p["properties"] as string) ?? "{}"),
      ...(p["payload_hash"] ? { payload_hash: p["payload_hash"] } : {}),
    },
  });
}
