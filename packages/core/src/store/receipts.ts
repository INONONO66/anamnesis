import { RecallLineageSelection } from "@anamnesis/protocol";
import { z } from "zod";
import { ADOPTION_NUMERIC_VERSION } from "../dynamics/adoption-numeric.ts";
import { RpcRecallResult } from "@anamnesis/protocol";

export function luceneQuery(raw: string): string {
  return raw
    .replace(/[+\-&|!(){}[\]^"~*?:\\/]/g, " ")
    .split(/\s/)
    .filter(Boolean)
    .join(" ");
}

export const receiptTime = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
export const receiptHash = z.string().regex(/^[0-9a-f]{64}$/);
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
export const RecallTransport = RecallTransportInput.extend({
  created_at: receiptTime, principal: z.literal("installation"),
  commit_mode: z.enum(["auto", "receipt"]), boundary: z.literal("node-write-callback-v1"),
});
/** Feedback contains no derived state; attribution is resolved from the receipt. In-process only until the commit RPC ships. */
export const CommitReceiptInput = z.strictObject({
  operation_id: z.uuidv7(), recall_id: z.uuidv7(),
  adopted: z.array(z.uuidv7()).max(64).optional(),
  reward: z.number().finite().min(-1).max(1).optional(),
}).superRefine((value, context) => {
  if (value.adopted === undefined && value.reward === undefined) context.addIssue({ code: "custom", message: "adopted or reward is required" });
  if (value.adopted && new Set(value.adopted).size !== value.adopted.length) context.addIssue({ code: "custom", message: "adopted IDs must be distinct" });
});
export type CommitReceiptInput = z.infer<typeof CommitReceiptInput>;
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
