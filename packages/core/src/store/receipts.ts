import { RecallLineageSelection } from "@anamnesis/protocol";
import { z } from "zod";

export function luceneQuery(raw: string): string {
  return raw
    .replace(/[+\-&|!(){}[\]^"~*?:\\/]/g, " ")
    .split(/\s/)
    .filter(Boolean)
    .join(" ");
}

export const receiptTime = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
export const receiptHash = z.string().regex(/^[0-9a-f]{64}$/);
const selection = z.array(z.strictObject({ id: z.uuidv7(), sources: z.array(z.uuidv7()).min(1).max(16) })).max(64);
export const Usage = z.strictObject({ usage_id: z.uuidv7(), created_at: receiptTime, policy_revision: receiptTime,
  selection, lineage_selection: RecallLineageSelection, selection_digest: receiptHash, client_binding: z.uuid().optional() });
export type Usage = z.infer<typeof Usage>;
export interface HitCacheIssue { code: "hit_cache_mismatch" | "invalid_hit_evidence"; id: string }
export interface HitCacheVerification { state: "verified"; hits: number; issues: HitCacheIssue[] }
export interface HitCacheRebuild { state: "rebuilt"; hits: number; created: number; removed: number }
export interface HitCache {
  episode_id: string; s: number; t_last_hit: number; hit_count: number;
  u_anchor: number; u_at: number;
}
export const OutcomeHit = z.strictObject({ id: z.uuidv7(), episode_id: z.uuidv7(), usage_id: z.uuidv7(), t: receiptTime,
  kind: z.literal("outcome"), credit: z.number().min(-0.2).max(0.2), idem_key: receiptHash });
export type OutcomeHit = z.infer<typeof OutcomeHit>;
export class ReceiptError extends Error {
  constructor(readonly code: "unknown_usage" | "outcome_already_reported" | "idempotency_conflict" | "invalid_selection" | "invalid_hit_evidence" | "unsupported_policy"
    | "policy_denied" | "policy_unavailable" | "unknown_policy" | "resource_exhausted" | "unauthenticated", detail = code) {
    super(`${code}: ${detail}`);
  }
}
