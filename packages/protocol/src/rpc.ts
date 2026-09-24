import { z } from "zod";
import { MemoryElement, Origin, TimePoint, validateElementSemantics } from "./element.ts";
import { CreateExtractionPipeline, RunExtractionPipeline, ExtractionPipelineStatus, ExtractionPipeline } from './extraction-audit.ts';
import { ModelTask } from './extraction.ts';

/** Ingest and receipt-feedback contract. Incompatible wire shapes require a new version. */
export const RPC_VERSION = 1;
export const RPC_LIMITS = {
  frame_bytes: 1024 * 1024,
  chunk_bytes: 512 * 1024,
  object_bytes: 64 * 1024 * 1024,
  content_bytes: 64 * 1024,
  properties_bytes: 64 * 1024,
  identifier_bytes: 1024,
  connections: 64,
  uploads_per_connection: 2,
  uploads: 32,
  upload_temp_bytes_per_connection: 128 * 1024 * 1024,
  upload_temp_bytes: 1024 * 1024 * 1024,
  queued_requests_per_connection: 16,
  queued_requests: 256,
  spool_bytes: 1024 * 1024 * 1024,
} as const;

export const RPC_METHODS = [
  "hello", "status", "shutdown", "object.begin", "object.chunk",
  "extraction.audit.create", "extraction.audit.run", "extraction.audit.status",
  "dream.admit", "dream.status", "dream.lease", "dream.expire", "dream.execute",
  "object.commit", "remember", "ingest.status", "commit", "graph.envelope", "hit-cache.verify", "hit-cache.rebuild", "backup", "restore", "backup.status", "restore.status", "policy.set", "policy.revoke", "recall", "embedding.recover", "embedding.status", "embedding.requeue",
] as const;
export const RPC_FUTURE_METHODS = [] as const;
export const RpcMethod = z.enum(RPC_METHODS);
export type RpcMethod = z.infer<typeof RpcMethod>;

const encoder = new TextEncoder();
// Works with either Unicode-aware or UTF-16 regular-expression consumers.
const wellFormedUnicode = /^(?:[^\uD800-\uDFFF]|[\uD800-\uDBFF][\uDC00-\uDFFF])*$/;
const unicodeString = z.string().regex(wellFormedUnicode, "malformed Unicode");
function boundedString(maxBytes: number, minLength = 1) {
  return unicodeString.min(minLength).max(maxBytes).refine(
    (value) => encoder.encode(value).byteLength <= maxBytes,
    `UTF-8 value exceeds ${maxBytes} bytes`,
  ).meta({ "x-maxUtf8Bytes": maxBytes });
}
const identifier = boundedString(RPC_LIMITS.identifier_bytes);
const counter = z.number().int().min(0).max(Number.MAX_SAFE_INTEGER);
const positiveCounter = counter.min(1);
export const RpcHash = z.string().regex(/^[0-9a-f]{64}$/);
export type RpcHash = z.infer<typeof RpcHash>;
const requestId = z.union([counter, boundedString(128)]);

/** Recursion retains Unicode and finite-number checks inside arbitrary properties. */
const jsonValue: z.ZodType<z.core.util.JSONType> = z.lazy(() => z.union([
  unicodeString,
  z.number(),
  z.boolean(),
  z.null(),
  z.array(jsonValue),
  z.record(unicodeString, jsonValue),
]));
const properties = z.record(unicodeString, jsonValue).refine(
  (value) => encoder.encode(JSON.stringify(value)).byteLength <= RPC_LIMITS.properties_bytes,
  "properties exceed the encoded byte limit",
).meta({ "x-maxJsonBytes": RPC_LIMITS.properties_bytes });
const origin = Origin.extend({
  source: identifier,
  session: identifier,
  actor: identifier,
  record: identifier,
});

/** Only original semantic Episodes enter through remember; no control schemas. */
export const RpcEpisode = z.strictObject({
  schema: z.enum(["anamnesis.original-message/1", "anamnesis.original-document/1"]),
  time: TimePoint,
  content: boundedString(RPC_LIMITS.content_bytes),
  origin,
  mass: MemoryElement.shape.mass,
  properties: properties.default({}),
}).superRefine(validateElementSemantics);
export type RpcEpisode = z.infer<typeof RpcEpisode>;
export type RpcEpisodeInput = z.input<typeof RpcEpisode>;

export const RpcRememberParams = z.strictObject({
  episode: RpcEpisode,
  source_revision: identifier,
  expected_previous_revision_key: RpcHash.nullable(),
  payload_hash: RpcHash.optional(),
  // Semantic admission is deferred until the stored digest version is known.
  // Metadata-free requests retain the compatibility/import contract.
  origin_role: jsonValue.optional(),
  lineage_mode: jsonValue.optional(),
  parent_recall_ids: jsonValue.optional(),
});
export type RpcRememberParams = z.infer<typeof RpcRememberParams>;
export type RpcRememberParamsInput = z.input<typeof RpcRememberParams>;

/** Reserved budget vocabulary; token installation is a runtime admission check. */
export const RpcOutputBudget = z.discriminatedUnion("unit", [
  z.strictObject({ unit: z.literal("utf8_bytes"), limit: counter }),
  z.strictObject({ unit: z.literal("unicode_scalars"), limit: counter }),
  z.strictObject({ unit: z.literal("tokens"), limit: counter, tokenizer_id: boundedString(256).regex(/^.+@sha256:[0-9a-f]{64}$/) }),
]);
export type RpcOutputBudget = z.infer<typeof RpcOutputBudget>;

export const RpcRecallParams = z.strictObject({
  query: boundedString(8192, 0), limit: counter.max(64).default(10),
  budget: RpcOutputBudget.optional(), T: counter.max(8640000000000000).optional(),
  session: z.strictObject({ source: identifier, session: identifier }).optional(),
});
export type RpcRecallParams = z.infer<typeof RpcRecallParams>;
export const RpcEmbeddingRecoverParams = z.strictObject({ operation_id: z.uuidv7(), episode_id: z.uuidv7() });
export type RpcEmbeddingRecoverParams = z.infer<typeof RpcEmbeddingRecoverParams>;
export const RpcEmbeddingStatusParams = z.strictObject({ operation_id: z.uuidv7() });
/** provider_unavailable is transient: the attempt is `deferred` and the Episode stays queued until the outbox's
 * retry budget is spent, which quarantines it as provider_unavailable_exhausted. Every other reason quarantines at once. */
export const RpcEmbeddingAttemptReason = z.enum(["provider_unavailable", "provider_unavailable_exhausted", "provider_rejected", "profile_mismatch", "invalid_vector", "input_too_large", "stale_input"]);
export const RpcEmbeddingAttempt = z.strictObject({
  operation_id: z.uuidv7(), episode_id: z.uuidv7(), profile_id: RpcHash,
  model: identifier, model_incarnation: RpcHash, dimensions: positiveCounter.max(4096),
  input_revision: RpcHash, input_digest: RpcHash, created_at: counter, completed_at: counter.nullable(),
  state: z.enum(["pending", "succeeded", "quarantined", "deferred"]),
  reason: RpcEmbeddingAttemptReason.nullable(),
  /** Evidence from the branch that failed (HTTP status, timeout budget, socket code); rows written before it existed read as null. */
  detail: boundedString(256).nullable().default(null),
});
export type RpcEmbeddingAttempt = z.infer<typeof RpcEmbeddingAttempt>;
export const RpcEmbeddingRequeueParams = z.strictObject({
  limit: positiveCounter.max(1000).default(100),
  reasons: z.array(RpcEmbeddingAttemptReason).min(1).max(7).optional(),
});
export type RpcEmbeddingRequeueParams = z.input<typeof RpcEmbeddingRequeueParams>;
export const RpcEmbeddingRequeueResult = z.strictObject({ requeued: counter });
export type RpcEmbeddingRequeueResult = z.infer<typeof RpcEmbeddingRequeueResult>;
export const RpcRecallChannel = z.enum(["identity", "bm25", "vector", "session"]);
export const RpcRecallItem = z.discriminatedUnion("kind", [
  z.strictObject({ id: z.uuidv7(), kind: z.literal("Episode"), schema: z.enum(["anamnesis.original-message/1", "anamnesis.original-document/1"]), epistemic: z.literal("observed"), content: boundedString(RPC_LIMITS.frame_bytes), time: TimePoint, score: z.number().nonnegative(), relevance: z.number().nonnegative(), mass: z.number().min(0).max(1), utility: z.number().min(-1).max(1), rank: counter.max(63).optional(), sources: z.array(z.uuidv7()).length(1), provenance: z.strictObject({ derived_from: z.array(z.strictObject({ id: z.uuidv7(), kind: z.enum(["Episode", "Fact"]), visible_at_T: z.boolean() })).length(1), supersedes: z.array(z.strictObject({ id: z.uuidv7(), content: boundedString(RPC_LIMITS.frame_bytes) })).max(8), supersedes_redacted: z.boolean(), contrasts: z.array(z.uuidv7()).max(4), warnings: z.array(z.strictObject({ code: z.enum(["supersedes_withheld", "supersedes_incomplete"]), content: boundedString(512) })).max(2) }), channels: z.array(RpcRecallChannel).max(4) }),
  z.strictObject({ id: z.uuidv7(), kind: z.literal("Fact"), schema: z.literal("anamnesis.claim/1"), epistemic: z.literal("derived"), content: boundedString(RPC_LIMITS.frame_bytes), time: TimePoint, score: z.number().nonnegative(), relevance: z.number().nonnegative(), mass: z.number().min(0).max(1), utility: z.number().min(-1).max(1), rank: counter.max(63).optional(), sources: z.array(z.uuidv7()).length(1), provenance: z.strictObject({ derived_from: z.array(z.strictObject({ id: z.uuidv7(), kind: z.literal("Episode"), visible_at_T: z.boolean() })).length(1), supersedes: z.array(z.strictObject({ id: z.uuidv7(), content: boundedString(RPC_LIMITS.frame_bytes) })).max(8), supersedes_redacted: z.boolean(), contrasts: z.array(z.uuidv7()).max(4), warnings: z.array(z.strictObject({ code: z.enum(["supersedes_withheld", "supersedes_incomplete"]), content: boundedString(512) })).max(2) }), channels: z.array(RpcRecallChannel).max(4) }),
]);
export type RpcRecallItem = z.infer<typeof RpcRecallItem>;
export const RpcRecallResult = z.strictObject({
  recall_id: z.uuidv7(), expires_at: counter, results: z.array(RpcRecallItem).max(64),
  companions: z.array(RpcRecallItem).max(256), entities: z.array(z.never()).max(0),
  context_text: unicodeString, used_budget: counter, budget: RpcOutputBudget,
  renderer: z.literal("canonical-jsonl-v1"),
  diagnostics: z.strictObject({
    pipeline: z.enum(["originals-hybrid-v1", "derived-hybrid-v1"]), now: counter, T: counter, policy_revision: counter,
    channels_used: z.array(RpcRecallChannel).max(4),
    vector_reason: z.enum(["not_configured", "not_requested", "provider_unavailable", "provider_rejected", "profile_mismatch", "invalid_vector", "input_too_large", "available"]),
    embedding_profile_id: RpcHash.nullable(), candidate_count: counter.max(177), skipped_bundles: counter.max(177),
    ppr_used: z.boolean(), identity_mode: z.literal("exact_episode_id"),
  }),
});
export type RpcRecallResult = z.infer<typeof RpcRecallResult>;

export const ConductingArcRow = z.strictObject({
  source_id: z.uuidv7(), link_id: z.uuidv7(), peer_id: z.uuidv7(),
  role: z.enum(["NEXT_EPISODE", "MENTIONS", "RELATES_TO", "HAS_MEMBER", "DERIVED_FROM"]),
  generation: z.number().int().nonnegative().nullable(),
  source_extraction_generation: z.number().int().nonnegative().nullable(),
});
export const ConductingArcProbeResult = z.strictObject({
  source_id: z.uuidv7(), count: z.number().int().min(0).max(256),
  saturated: z.boolean(), coverage: z.literal("complete"),
});
export type ConductingArcProbeResult = z.infer<typeof ConductingArcProbeResult>;

// The last sextet's unused bits must be zero, not merely decodable by Buffer.
const canonicalBase64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/][AQgw]==|[A-Za-z0-9+/]{2}[AEIMQUYcgkosw048]=)?$/;
const chunkBytes = z.string().min(4)
  .max(4 * Math.ceil(RPC_LIMITS.chunk_bytes / 3))
  .regex(canonicalBase64)
  .refine((value) => {
    const padding = value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0;
    return value.length / 4 * 3 - padding <= RPC_LIMITS.chunk_bytes;
  }, "decoded chunk exceeds the byte limit")
  .meta({ "x-maxDecodedBytes": RPC_LIMITS.chunk_bytes, contentEncoding: "base64" });

export const RpcHelloParams = z.strictObject({
  /** This token authenticates one installation principal, never `client`. */
  token: boundedString(1024),
  /** Informational label only; must never become an authorization identity. */
  client: boundedString(128),
  commit_mode: z.enum(["auto", "receipt"]),
  version: z.literal(RPC_VERSION),
});
export type RpcHelloParams = z.infer<typeof RpcHelloParams>;
const deliveryIdentity = {
  revision_key: RpcHash,
  body_digest: RpcHash,
  data_incarnation: z.uuid(),
};
export const RpcIngestStatusParams = z.strictObject(deliveryIdentity);
export type RpcIngestStatusParams = z.infer<typeof RpcIngestStatusParams>;

/** Feedback contains no derived state; attribution is resolved from the receipt. */
export const RpcCommitParams = z.strictObject({
  operation_id: z.uuidv7(), recall_id: z.uuidv7(),
  adopted: z.array(z.uuidv7()).max(64).optional(),
  reward: z.number().finite().min(-1).max(1).optional(),
}).superRefine((value, context) => {
  if (value.adopted === undefined && value.reward === undefined) context.addIssue({ code: "custom", message: "adopted or reward is required" });
  if (value.adopted && new Set(value.adopted).size !== value.adopted.length) context.addIssue({ code: "custom", message: "adopted IDs must be distinct" });
});
export type RpcCommitParams = z.infer<typeof RpcCommitParams>;
export const RpcDreamFence = z.strictObject({ extraction_generation: counter, covered_ingest_seq: counter, structure_revision: counter, policy_revision: counter });
export const RpcDreamAdmitParams = RpcDreamFence.extend({ phase: z.enum(["community", "synthesis", "profile"]), source_ids: z.array(z.uuidv7()).min(1).max(256) });
export const RpcDreamLeaseParams = z.strictObject({ job_id: boundedString(256), expected_version: counter, worker_id: boundedString(256), lease_ms: counter.min(1).max(30000) });
export type RpcDreamLeaseParams = z.infer<typeof RpcDreamLeaseParams>;
export const RpcDreamExpireParams = z.strictObject({ job_id: boundedString(256), expected_version: counter, lease_epoch: boundedString(512) });
export type RpcDreamExpireParams = z.infer<typeof RpcDreamExpireParams>;
export const RpcDreamExecuteParams = z.strictObject({ job_id: boundedString(256), expected_version: counter });
export type RpcDreamExecuteParams = z.infer<typeof RpcDreamExecuteParams>;
export const RpcDreamJob = RpcDreamAdmitParams.extend({ job_id: boundedString(256), source_receipts: z.array(z.strictObject({ id: z.uuidv7(), revision: RpcHash, body_digest: RpcHash, ingest_seq: positiveCounter, allowed: z.literal(true) })).max(256), state: z.enum(["queued", "leased", "unknown", "succeeded"]), version: counter, lease: z.strictObject({ worker_id: boundedString(256), epoch: boundedString(512), expires_at: counter }).nullable(), semantic_writes: z.literal(false), authority: z.literal("none"), execution: z.strictObject({ state: z.enum(["attempted", "unknown", "succeeded"]), attempt: z.number().int().positive(), error: z.string().max(1024).optional(), result: z.unknown().optional(), retryable: z.literal(false) }).optional() });
export type RpcDreamAdmitParams = z.infer<typeof RpcDreamAdmitParams>;
export type RpcDreamJob = z.infer<typeof RpcDreamJob>;
export const RpcHitCacheParams = z.strictObject({});
export type RpcHitCacheParams = z.infer<typeof RpcHitCacheParams>;

/** Deliberately narrow authority: exact durable Episode/source selectors, ANDed.
 * No derived/entity/literal inference or payload scanning is advertised. */
export const RpcPolicySelector = z.strictObject({
  episode_id: z.uuidv7().optional(), source: identifier.optional(),
}).refine(value => value.episode_id !== undefined || value.source !== undefined, "selector is required");
export const RpcPolicySetParams = z.strictObject({
  policy_id: z.uuidv7(), selector: RpcPolicySelector, scope: z.literal("content"),
});
export type RpcPolicySetParams = z.infer<typeof RpcPolicySetParams>;
export const RpcPolicyRevokeParams = z.strictObject({ policy_id: z.uuidv7() });
export type RpcPolicyRevokeParams = z.infer<typeof RpcPolicyRevokeParams>;
export const RpcPolicyResult = RpcPolicySetParams.extend({
  action: z.enum(["deny", "revoke"]), applied: z.boolean(), policy_revision: counter,
  evaluator: z.literal("episode-source-v1"),
});
export type RpcPolicyResult = z.infer<typeof RpcPolicyResult>;

function request<const Method extends RpcMethod, Params extends z.ZodType>(method: Method, params: Params) {
  return z.strictObject({ jsonrpc: z.literal("2.0"), id: requestId, method: z.literal(method), params });
}
function validateFrame(value: unknown, context: z.RefinementCtx): void {
  const json = JSON.stringify(value);
  if (json !== undefined && encoder.encode(json).byteLength > RPC_LIMITS.frame_bytes) {
    context.addIssue({ code: "custom", message: "encoded RPC frame exceeds the byte limit" });
  }
}

/** Framing must enforce the raw byte cap before JSON decoding or allocation. */
export const RpcRequest = z.discriminatedUnion("method", [
  request("hello", RpcHelloParams),
  request("status", z.strictObject({})),
  request("shutdown", z.strictObject({})),
  request("object.begin", z.strictObject({
    sha256: RpcHash,
    size: counter.max(RPC_LIMITS.object_bytes),
    media_type: boundedString(255),
  })),
  request("object.chunk", z.strictObject({ upload_id: z.uuid(), seq: counter, bytes_b64: chunkBytes })),
  request("object.commit", z.strictObject({ upload_id: z.uuid() })),
  request("remember", RpcRememberParams),
  request("ingest.status", RpcIngestStatusParams),
  request("commit", RpcCommitParams),
  request("extraction.audit.create", CreateExtractionPipeline),
  request("extraction.audit.run", RunExtractionPipeline),
  request("extraction.audit.status", ExtractionPipelineStatus),
  request("dream.admit", RpcDreamAdmitParams),
  request("dream.status", z.strictObject({ job_id: boundedString(256) })),
  request("dream.lease", RpcDreamLeaseParams),
  request("dream.expire", RpcDreamExpireParams),
  request("dream.execute", RpcDreamExecuteParams),
  request("recall", RpcRecallParams),
  request("graph.envelope", z.strictObject({ seed_ids: z.array(z.uuidv7()).min(1).max(128), T: counter.optional() })),
  request("embedding.recover", RpcEmbeddingRecoverParams),
  request("embedding.status", RpcEmbeddingStatusParams),
  request("embedding.requeue", RpcEmbeddingRequeueParams),
  request("backup", z.strictObject({ operation_id: z.uuidv7(), destination: boundedString(1024) })),
  request("restore", z.strictObject({ operation_id: z.uuidv7(), archive: boundedString(1024) })),
  request("backup.status", z.strictObject({ operation_id: z.uuidv7() })),
  request("restore.status", z.strictObject({ operation_id: z.uuidv7() })),
  request("policy.set", RpcPolicySetParams),
  request("policy.revoke", RpcPolicyRevokeParams),
  request("hit-cache.verify", RpcHitCacheParams),
  request("hit-cache.rebuild", RpcHitCacheParams),
]).superRefine(validateFrame).meta({ "x-maxJsonBytes": RPC_LIMITS.frame_bytes });
export type RpcRequest = z.infer<typeof RpcRequest>;
export type RpcRequestInput = z.input<typeof RpcRequest>;

/** First frame on a TCP connection, before any request; UDS peers never send it.
 * Same u32-be framing as requests. A mismatch is answered with `unauthorized`. */
export const RpcTcpAuth = z.strictObject({ auth: z.strictObject({ bearer: boundedString(RPC_LIMITS.identifier_bytes) }) });
export type RpcTcpAuth = z.infer<typeof RpcTcpAuth>;

export const RpcErrorCode = z.enum([
  "parse_error", "invalid_request", "invalid_params", "unsupported_method",
  "unsupported_version", "unauthorized", "unauthenticated", "authentication_failed", "already_authenticated",
  "storage_unavailable", "resource_exhausted", "shutting_down", "ownership_lost",
  "revision_conflict", "stale_revision", "idempotency_conflict", "incarnation_mismatch",
  "object_not_found", "object_metadata_conflict", "object_corrupt", "upload_not_found",
  "upload_sequence_mismatch", "object_size_mismatch", "object_hash_mismatch",
  "lineage_unavailable", "lineage_binding_mismatch", "lineage_mismatch",
  "spool_corrupt", "unsupported_digest_version", "unknown_recall", "receipt_expired", "empty_commit", "invalid_selection", "unsupported_policy", "internal_error",
  "commit_mode_mismatch", "policy_denied", "policy_unavailable", "unknown_policy", "invalid_hit_evidence",
  "extraction_not_configured", "extraction_audit_conflict", "extraction_audit_incomplete", "extraction_audit_stale", "backup_adapter_unavailable", "restore_adapter_unavailable", "daemon_live", "member_mismatch", "archive_layout", "invalid_completion", "completion_mismatch", "incompatible_archive", "object_digest_mismatch", "invalid_dump",
  "dream_input_invalid", "dream_fence_stale", "dream_source_missing", "dream_source_denied", "dream_source_stale", "dream_job_missing", "dream_version_conflict", "dream_lease_invalid", "dream_lease_expired", "dream_not_queued", "dream_lease_fenced", "dream_adapter_unavailable",
  "embedding_not_configured", "invalid_budget", "receipt_unavailable", "degree_probe_unavailable", "ordered_probe_unavailable",
]);
export type RpcErrorCode = z.infer<typeof RpcErrorCode>;
export const RpcError = z.strictObject({
  code: z.literal([-32700, -32600, -32601, -32602, -32603, -32000]),
  message: boundedString(1024),
  data: z.strictObject({ code: RpcErrorCode, retryable: z.boolean() }),
});
export type RpcError = z.infer<typeof RpcError>;

export const RpcObjectMetadata = z.strictObject({
  hash: RpcHash,
  size: counter.max(RPC_LIMITS.object_bytes),
  media_type: boundedString(255),
});
export type RpcObjectMetadata = z.infer<typeof RpcObjectMetadata>;

export const RpcCommittedResult = z.strictObject({
  state: z.literal("committed"),
  ...deliveryIdentity,
  id: z.uuidv7(),
  created: z.boolean(),
  ingest_seq: positiveCounter,
});
export type RpcCommittedResult = z.infer<typeof RpcCommittedResult>;
export const RpcSpooledResult = z.strictObject({
  state: z.literal("spooled"),
  ...deliveryIdentity,
  fs_epoch: z.uuid(),
  spool_seq: positiveCounter,
});
export type RpcSpooledResult = z.infer<typeof RpcSpooledResult>;
export const RpcRememberResult = z.discriminatedUnion("state", [RpcCommittedResult, RpcSpooledResult]);
export type RpcRememberResult = z.infer<typeof RpcRememberResult>;
export const RpcStorageState = z.enum(["available", "unavailable"]);
export type RpcStorageState = z.infer<typeof RpcStorageState>;
export const RpcIngestStatusResult = z.discriminatedUnion("state", [
  RpcCommittedResult,
  RpcSpooledResult,
  /** `storage` is the daemon's observation while it answered: `unknown` rules out a committed delivery only while
   * storage was available, so a client never combines this verdict with an earlier, possibly stale `status`. */
  z.strictObject({ state: z.literal("unknown"), ...deliveryIdentity, storage: RpcStorageState }),
  z.strictObject({
    state: z.literal("blocked"), ...deliveryIdentity,
    fs_epoch: z.uuid(), spool_seq: positiveCounter,
    expected_previous_revision_key: RpcHash.nullable(),
    reason: z.enum(["missing_predecessor", "dependency_cycle", "stale_revision", "revision_conflict", "storage_unavailable"]),
  }),
  z.strictObject({ state: z.literal("quarantined"), ...deliveryIdentity, reason: RpcErrorCode }),
]);
export type RpcIngestStatusResult = z.infer<typeof RpcIngestStatusResult>;

/** Capability names are operational, not promises about unimplemented stages. */
export const RpcCapabilities = z.strictObject({
  methods: z.array(RpcMethod).max(RPC_METHODS.length).refine(
    (methods) => new Set(methods).size === methods.length,
    "duplicate capability method",
  ),
  recall: z.boolean(),
  commit: z.boolean(),
  policy: z.boolean(),
  extraction: z.boolean(),
  embeddings: z.boolean(),
  writer_fence: z.enum(["database", "local_only"]),
});
export type RpcCapabilities = z.infer<typeof RpcCapabilities>;
/** Provider pacing on the extraction lane: at most `max_in_flight` pipelines, and consecutive provider calls
 * (claims, judges, retries) spaced by `min_interval_ms * (1 + jitter_fraction * random())` through one FIFO gate.
 * The knobs echo the daemon's environment; `calls_total` and `waited_total_ms` are per process lifetime. */
export const RpcExtractionPacing = z.strictObject({
  max_in_flight: z.number().int().min(1).max(16),
  min_interval_ms: z.number().int().min(0).max(600000),
  jitter_fraction: z.number().min(0).max(1),
  calls_total: counter,
  waited_total_ms: counter,
});
export type RpcExtractionPacing = z.infer<typeof RpcExtractionPacing>;
/** Background workers owned by the daemon's single writer. Counters are per process lifetime. */
export const RpcWorkersStatus = z.strictObject({
  embedding: z.strictObject({
    /** Null while storage is unavailable; never a fabricated zero. */
    pending: counter.nullable(),
    drained_total: counter,
    quarantined_total: counter,
    last_error: z.string().max(512).nullable(),
  }),
  extraction: z.discriminatedUnion("state", [
    z.strictObject({ state: z.literal("unconfigured") }),
    /** Configured, but no turn has attached a generation yet (startup, or storage unavailable since startup). */
    z.strictObject({ state: z.literal("starting"), pacing: RpcExtractionPacing }),
    /** The single writable generation the scheduler feeds. Watermarks are the last observed values; counters are per process lifetime. */
    z.strictObject({
      state: z.enum(["catching_up", "active"]), generation_id: z.uuidv7(), covered_ingest_seq: counter, live_ingest_seq: counter,
      in_flight: counter.max(16), completed_total: counter, failed_total: counter, last_error: z.string().max(512).nullable(),
      pacing: RpcExtractionPacing,
    }),
  ]),
});
export type RpcWorkersStatus = z.infer<typeof RpcWorkersStatus>;
export const RpcStatusResult = z.strictObject({
  version: z.literal(RPC_VERSION),
  state: z.enum(["starting", "ready", "degraded", "stopping"]),
  storage: RpcStorageState,
  data_incarnation: z.uuid(),
  fs_epoch: z.uuid(),
  queue: z.strictObject({ pending: counter.max(RPC_LIMITS.queued_requests), capacity: z.literal(RPC_LIMITS.queued_requests) }),
  spool: z.strictObject({ pending: counter, blocked: counter, quarantined: counter, bytes: counter.max(RPC_LIMITS.spool_bytes) }),
  outbox_pending: counter.nullable(),
  capabilities: RpcCapabilities,
  workers: RpcWorkersStatus,
});
export type RpcStatusResult = z.infer<typeof RpcStatusResult>;

const responseEnvelope = {
  jsonrpc: z.literal("2.0"),
  id: requestId,
  /** Null means unobserved/unavailable; never substitute a fabricated zero. */
  structure_revision: counter.nullable(),
  policy_revision: counter.nullable(),
  server_time: counter,
};
function success<const Method extends RpcMethod, Result extends z.ZodType>(method: Method, result: Result) {
  return z.strictObject({ ...responseEnvelope, method: z.literal(method), result });
}
export const RpcSuccessResponse = z.discriminatedUnion("method", [
  success("hello", z.strictObject({
    version: z.literal(RPC_VERSION),
    principal: z.literal("installation"),
    commit_mode: z.enum(["auto", "receipt"]),
    data_incarnation: z.uuid(),
    fs_epoch: z.uuid(),
    capabilities: RpcCapabilities,
    limits: z.strictObject({
      frame_bytes: z.literal(RPC_LIMITS.frame_bytes),
      chunk_bytes: z.literal(RPC_LIMITS.chunk_bytes),
      object_bytes: z.literal(RPC_LIMITS.object_bytes),
      content_bytes: z.literal(RPC_LIMITS.content_bytes),
    }),
  })),
  success("status", RpcStatusResult),
  success("shutdown", z.strictObject({ state: z.literal("stopping") })),
  success("object.begin", z.discriminatedUnion("state", [
    z.strictObject({ state: z.literal("uploading"), upload_id: z.uuid(), next_seq: counter, chunk_bytes_max: z.literal(RPC_LIMITS.chunk_bytes) }),
    z.strictObject({ state: z.literal("committed"), object: RpcObjectMetadata }),
  ])),
  success("object.chunk", z.strictObject({ upload_id: z.uuid(), next_seq: positiveCounter })),
  success("object.commit", RpcObjectMetadata),
  success("remember", RpcRememberResult),
  success("ingest.status", RpcIngestStatusResult),
  success("commit", z.strictObject({ operation_id: z.uuidv7(), recall_id: z.uuidv7(), adopted: z.array(z.uuidv7()).max(64), reward: z.number().finite().min(-1).max(1).nullable(), applied: z.boolean() })),
  success("hit-cache.verify", z.strictObject({ state: z.literal("verified"), hits: counter, issues: z.array(z.strictObject({ code: boundedString(128), id: boundedString(128) })).max(1024) })),
  success("hit-cache.rebuild", z.strictObject({ state: z.literal("rebuilt"), hits: counter, created: counter, removed: counter })),
  success("graph.envelope", z.strictObject({ nodes: z.array(z.uuidv7()).max(2000), arcs: z.array(ConductingArcRow).max(20000),
    probes: z.array(ConductingArcProbeResult).max(128), truncated: z.boolean(),
    pin: z.strictObject({ policy_revision: counter, generation_id: z.uuidv7(), coverage_revision: counter, covered_ingest_seq: counter, T: counter }),
    overflow: z.strictObject({ nodes: counter, arcs: counter, saturated_sources: counter.max(128) }) })),
  success("recall", RpcRecallResult),
  success("extraction.audit.create", ModelTask),
  success("extraction.audit.run", ExtractionPipeline),
  success("extraction.audit.status", ExtractionPipeline),
  success("dream.admit", RpcDreamJob),
  success("dream.status", RpcDreamJob),
  success("dream.lease", RpcDreamJob),
  success("dream.expire", RpcDreamJob),
  success("dream.execute", RpcDreamJob),
  success("embedding.recover", RpcEmbeddingAttempt),
  success("embedding.status", z.union([RpcEmbeddingAttempt, z.strictObject({ state: z.literal("unknown"), operation_id: z.uuidv7() })])),
  success("embedding.requeue", RpcEmbeddingRequeueResult),
  success("backup", z.strictObject({ state: z.literal("complete"), operation_id: z.uuidv7() })),
  success("restore", z.strictObject({ state: z.literal("complete"), operation_id: z.uuidv7(), manifest: z.unknown() })),
  success("backup.status", z.union([z.strictObject({ state: z.enum(["running", "complete", "failed"]), operation_id: z.uuidv7(), error: boundedString(1024).optional() }), z.strictObject({ state: z.literal("unknown"), operation_id: z.uuidv7(), reason: z.enum(["not_found", "adapter_unavailable"]) })])),
  success("restore.status", z.union([z.strictObject({ state: z.enum(["running", "complete", "failed"]), operation_id: z.uuidv7(), error: boundedString(1024).optional() }), z.strictObject({ state: z.literal("unknown"), operation_id: z.uuidv7(), reason: z.enum(["not_found", "adapter_unavailable"]) })])),
  success("policy.set", RpcPolicyResult),
  success("policy.revoke", RpcPolicyResult),
]);
export type RpcSuccessResponse = z.infer<typeof RpcSuccessResponse>;
export const RpcErrorResponse = z.strictObject({
  ...responseEnvelope,
  id: requestId.nullable(),
  error: RpcError,
});
export type RpcErrorResponse = z.infer<typeof RpcErrorResponse>;
export const RpcResponse = z.union([RpcSuccessResponse, RpcErrorResponse])
  .superRefine(validateFrame).meta({ "x-maxJsonBytes": RPC_LIMITS.frame_bytes });
export type RpcResponse = z.infer<typeof RpcResponse>;
