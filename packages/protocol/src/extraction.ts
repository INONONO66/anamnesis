import { createHash } from "node:crypto";
import { z } from "zod";

const hash = z.string().regex(/^[0-9a-f]{64}$/);
const id = z.uuidv7();
const bounded = (n: number) => z.string().max(n).refine(v => !/[\uD800-\uDFFF]/u.test(v) && Buffer.byteLength(v, "utf8") <= n, "invalid or oversized UTF-8");
const name = bounded(256).refine(v => v.length > 0, "empty name");
const timestamp = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
export const ExtractionSpan = z.strictObject({ start: timestamp, end: timestamp, text: bounded(8192) })
  .refine(v => v.end > v.start && v.end - v.start <= 8192 && Buffer.byteLength(v.text, "utf8") === v.end - v.start, "invalid span");
const spans = z.array(ExtractionSpan).max(64);
const lease = z.strictObject({ worker_id: name, epoch: id, writer_epoch: timestamp.positive(), expires_at: timestamp });
const policy = z.strictObject({ revision: timestamp, authority: z.literal("installation") });
const disposition = z.enum(["retain", "suppress", "correct", "unknown"]);
const modality = z.enum(["text", "code", "mixed", "unknown"]);
const terminal = z.enum(["succeeded", "failed", "cancelled", "expired", "worker_lost"]);
export const ExtractionFailure = z.enum(["provider_unavailable", "provider_rejected", "provider_mismatch", "output_too_large", "input_too_large", "policy_denied", "cancelled", "expired", "worker_lost", "premises_changed"]);

/** Bounded model-task audit ABI, not a Fact/graph materialization ABI. */
export const ExtractionModelOutput = z.discriminatedUnion("task", [
  z.strictObject({ task: z.literal("claim"), claims: z.array(z.strictObject({ text: bounded(8192).refine(v => v.length > 0), evidence: ExtractionSpan })).max(64), language: bounded(64), modality }),
  z.strictObject({ task: z.literal("judge"), disposition, spans, language: bounded(64), modality }),
  z.strictObject({ task: z.literal("judge_claims"), claim_body_digest: hash,
    decisions: z.array(z.strictObject({ claim_index: timestamp.max(63), disposition, evidence: ExtractionSpan })).max(64),
    language: bounded(64), modality }),
]);
export type ExtractionModelOutput = z.infer<typeof ExtractionModelOutput>;
export const ExtractionOutput = z.strictObject({ canonical_body: bounded(65536), body_digest: hash, spans, language: bounded(64), modality }).superRefine((v, ctx) => {
  try {
    const body: unknown = JSON.parse(v.canonical_body);
    if (canonicalExtractionBody(body) !== v.canonical_body || extractionBodyDigest(body) !== v.body_digest) throw new Error("digest");
    const parsed = ExtractionModelOutput.parse(body);
    const evidence = parsed.task === "claim" ? parsed.claims.map(c => c.evidence) : parsed.task === "judge_claims" ? parsed.decisions.map(d => d.evidence) : parsed.spans;
    if (canonicalExtractionBody(evidence) !== canonicalExtractionBody(v.spans) || parsed.language !== v.language || parsed.modality !== v.modality) throw new Error("metadata");
  } catch { ctx.addIssue({ code: "custom", message: "output ABI/canonical body/digest mismatch" }); }
});

export const Generation = z.strictObject({
  id, stream: name, incarnation: hash, state: z.enum(["active", "catching_up", "cutover", "retired"]),
  covered_ingest_seq: timestamp, created_at: timestamp, updated_at: timestamp,
}).refine(v => v.updated_at >= v.created_at, "backwards timestamps");
export type Generation = z.infer<typeof Generation>;

const source = { generation_id: id, source_id: id, source_revision: hash, body_digest: hash, source_ingest_seq: timestamp.positive() };
/** Only terminal attempts are immutable records. Mutable work lives in ModelTask. */
export const ExtractionAttempt = z.strictObject({
  id, task_id: id, ...source, state: terminal, reason: ExtractionFailure.nullable(), disposition: disposition.nullable(),
  created_at: timestamp, updated_at: timestamp, lease: lease.nullable(), output: ExtractionOutput.nullable(),
  policy_context: policy, spans,
}).superRefine((v, ctx) => {
  if (v.updated_at < v.created_at) ctx.addIssue({ code: "custom", message: "backwards timestamps" });
  if (v.state === "succeeded") {
    if (!v.output || v.reason !== null || v.disposition === null || !v.lease || canonicalExtractionBody(v.spans) !== canonicalExtractionBody(v.output.spans)) ctx.addIssue({ code: "custom", message: "invalid success" });
  } else if (v.output !== null || v.spans.length || v.disposition !== null || v.reason === null) ctx.addIssue({ code: "custom", message: "non-success must be content-free" });
  if (["cancelled", "expired", "worker_lost"].includes(v.state) && v.reason !== v.state && !(v.state === "cancelled" && v.reason === "policy_denied")) ctx.addIssue({ code: "custom", message: "outcome/reason mismatch" });
});
export type ExtractionAttempt = z.infer<typeof ExtractionAttempt>;

export const ModelTask = z.strictObject({
  id, ...source, attempt_id: id.nullable(), kind: z.enum(["claim", "judge", "judge_claims"]), model: name, model_incarnation: hash,
  pipeline: z.literal("claim-judge-audit-v1").optional(),
  state: z.enum(["queued", "leased", "succeeded", "failed", "expired", "cancelled", "worker_lost"]),
  lease: lease.nullable(), policy_context: policy.nullable(), version: timestamp,
  attempts: timestamp.max(1000), created_at: timestamp, updated_at: timestamp,
}).superRefine((v, ctx) => {
  if (v.updated_at < v.created_at) ctx.addIssue({ code: "custom", message: "backwards timestamps" });
  if ((v.state === "leased") !== (v.lease !== null)) ctx.addIssue({ code: "custom", message: "lease/state mismatch" });
  if (v.state === "leased" && (!v.lease || !v.attempt_id || !v.policy_context || v.attempts < 1 || v.lease.expires_at <= v.updated_at)) ctx.addIssue({ code: "custom", message: "invalid live lease" });
  if (v.state === "queued" && v.attempt_id !== null) ctx.addIssue({ code: "custom", message: "queued task has an attempt" });
  if (v.state !== "queued" && !v.attempt_id) ctx.addIssue({ code: "custom", message: "missing attempt" });
});
export type ModelTask = z.infer<typeof ModelTask>;

export const CreateModelTask = z.strictObject({ id, generation_id: id, source_id: id, kind: z.enum(["claim", "judge"]), model: name, model_incarnation: hash,
  pipeline: z.literal("claim-judge-audit-v1").optional(),
}).refine(v => v.pipeline === undefined || v.kind === "claim", "pipeline must start with claim");
export type CreateModelTask = z.infer<typeof CreateModelTask>;
export const ModelTaskCAS = z.strictObject({ task_id: id, expected_version: timestamp });
export type ModelTaskCAS = z.infer<typeof ModelTaskCAS>;
export const LeaseModelTask = ModelTaskCAS.extend({ worker_id: name, lease_ms: timestamp.positive().max(30000) });
export type LeaseModelTask = z.infer<typeof LeaseModelTask>;
export const SettleModelTask = ModelTaskCAS.extend({ lease_epoch: id, reason: z.enum(["expired", "worker_lost"]) });
export type SettleModelTask = z.infer<typeof SettleModelTask>;
export const CompleteExtractionAttempt = ModelTaskCAS.extend({
  id, lease_epoch: id, state: z.enum(["succeeded", "failed"]), reason: ExtractionFailure.nullable(), disposition: disposition.nullable(), output: ExtractionOutput.nullable(), spans,
}).refine(v => {
  if (v.state === "succeeded") return v.output !== null && v.disposition !== null && v.reason === null;
  return v.output === null && v.disposition === null && v.spans.length === 0 && v.reason !== null
    && (v.reason.startsWith("provider_") || ["input_too_large", "output_too_large"].includes(v.reason));
}, "invalid completion");
export type CompleteExtractionAttempt = z.infer<typeof CompleteExtractionAttempt>;
const partition = z.enum(["episodes", "active_extraction"]);
export const Coverage = z.strictObject({ generation_id: id, partition, required_ingest_seq: timestamp, covered_ingest_seq: timestamp, omission_digest: hash, updated_at: timestamp }).refine(v => v.covered_ingest_seq <= v.required_ingest_seq, "coverage exceeds required");
export type Coverage = z.infer<typeof Coverage>;
export const AdvanceExtractionCoverage = z.strictObject({ generation_id: id, partition, expected_covered_ingest_seq: timestamp, covered_ingest_seq: timestamp });
export type AdvanceExtractionCoverage = z.infer<typeof AdvanceExtractionCoverage>;

/** Server-owned selection epoch. Reopening a rollback target also invalidates
 * the epoch, even though it does not yet change the selected generation ID. */
export const ExtractionSelection = z.strictObject({ generation_id: id.nullable(), selector_version: timestamp });
export type ExtractionSelection = z.infer<typeof ExtractionSelection>;
export const SelectExtractionGeneration = z.strictObject({ generation_id: id, expected_generation_id: id.nullable(), expected_selector_version: timestamp });
export type SelectExtractionGeneration = z.infer<typeof SelectExtractionGeneration>;
/** Omit the expectation only when acquiring a new pin, never to validate one. */
export const ReadExtractionCoverage = z.strictObject({ generation_id: id, expected_selector_version: timestamp.optional() });
export type ReadExtractionCoverage = z.infer<typeof ReadExtractionCoverage>;
export const ExtractionCoverageRead = z.strictObject({ generation_id: id, selector_version: timestamp, required_ingest_seq: timestamp, covered_ingest_seq: timestamp, omission_digest: hash, policy_revision: timestamp, read_at: timestamp });
export type ExtractionCoverageRead = z.infer<typeof ExtractionCoverageRead>;

/** Finite JSON only; UTF-16 key ordering, no accessors, sparse arrays or cycles.
 * Depth/node bounds apply before recursive traversal of untrusted provider data. */
export function canonicalExtractionBody(value: unknown): string {
  let nodes = 0;
  const active = new Set<object>();
  const visit = (v: unknown, depth: number): string => {
    if (++nodes > 10000 || depth > 32) throw new Error("JSON complexity exceeded");
    if (v === null) return "null";
    if (typeof v === "string") {
      if (/[\uD800-\uDFFF]/u.test(v)) throw new Error("invalid Unicode");
      return JSON.stringify(v);
    }
    if (typeof v === "boolean") return String(v);
    if (typeof v === "number") { if (!Number.isFinite(v)) throw new Error("non-JSON number"); return JSON.stringify(v); }
    if (typeof v !== "object") throw new Error("value outside JSON domain");
    if (active.has(v)) throw new Error("cyclic JSON");
    if (!Array.isArray(v) && Object.getPrototypeOf(v) !== Object.prototype && Object.getPrototypeOf(v) !== null) throw new Error("non-JSON object");
    active.add(v);
    const keys = Reflect.ownKeys(v).filter(k => !(Array.isArray(v) && k === "length"));
    if (keys.some(k => typeof k !== "string" || !Object.getOwnPropertyDescriptor(v, k)?.enumerable || !("value" in Object.getOwnPropertyDescriptor(v, k)!))) throw new Error("non-JSON property");
    let result: string;
    if (Array.isArray(v)) {
      if (keys.length !== v.length || keys.some((k, i) => k !== String(i))) throw new Error("sparse array");
      result = `[${v.map(x => visit(x, depth + 1)).join(",")}]`;
    } else {
      result = `{${(keys as string[]).sort().map(k => `${visit(k, depth + 1)}:${visit((v as Record<string, unknown>)[k], depth + 1)}`).join(",")}}`;
    }
    active.delete(v);
    return result;
  };
  return visit(value, 0);
}
export function extractionBodyDigest(value: unknown): string {
  return createHash("sha256").update(canonicalExtractionBody(value)).digest("hex");
}
