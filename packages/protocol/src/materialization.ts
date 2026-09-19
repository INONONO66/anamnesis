import { z } from "zod";
import { SemanticClaim, SemanticSourceContext } from "./semantic-claim.ts";
import { ExtractionSelection, ExtractionSpan } from "./extraction.ts";

const id = z.uuidv7(), hash = z.string().regex(/^[0-9a-f]{64}$/);
const counter = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const binding = { generation_id: id, source_episode_id: id, judge_attempt_id: id, claim_index: counter.max(31), semantic_claim: SemanticClaim };
/** Candidates are not approvals. Only the separately retained operator review
 * can consume a successful semantic proposal. No RPC exposure in this increment. */
export const ProposeRetainedClaim = z.strictObject({ proposal_id: id, ...binding });
export type ProposeRetainedClaim = z.infer<typeof ProposeRetainedClaim>;
export const MaterializeRetainedClaim = z.strictObject({ operation_id: id, proposal_id: id.optional(), ...binding });
export type MaterializeRetainedClaim = z.infer<typeof MaterializeRetainedClaim>;
export const ReviewRetainedClaim = z.strictObject({ review_id: id, proposal_id: id, action: z.enum(["accept", "reject"]), reason: z.string().min(1).max(2048) });
export type ReviewRetainedClaim = z.infer<typeof ReviewRetainedClaim>;
export const MaterializationResult = z.strictObject({ created: z.boolean(), fact_id: id, link_id: id });
export type MaterializationResult = z.infer<typeof MaterializationResult>;

/** Supplied by the installed semantic adapter, never by materialize's caller. */
export const SemanticResolution = SemanticSourceContext.pick({ entity_resolutions: true, attribution_speakers: true, allow_no_single_locus: true })
  .extend({ content_language: SemanticClaim.shape.content_language });
export type SemanticResolution = z.infer<typeof SemanticResolution>;
export const SemanticReviewOutput = z.strictObject({
  disposition: z.enum(["retain", "correct", "suppress"]),
  semantic_claim: SemanticClaim.nullable(), reason: z.string().min(1).max(2048),
}).refine(v => (v.disposition === "suppress") === (v.semantic_claim === null), "suppression must be content-free");
export type SemanticReviewOutput = z.infer<typeof SemanticReviewOutput>;

export const SemanticReviewPremises = z.strictObject({
  request: ProposeRetainedClaim, request_digest: hash, judge_profile_id: hash,
  source: SemanticSourceContext.shape.episode.omit({ content_language: true }),
  audit_evidence: ExtractionSpan, source_head_revision_key: hash, policy_revision: counter, generation_digest: hash,
  selection: ExtractionSelection, candidate_digest: hash,
  // New-occurrence only: a bounded, complete generation partition. Overflow
  // refuses admission; this is not a substitute for later serving indexes.
  candidates: z.array(z.strictObject({ id, digest: hash, content: z.string().min(1).max(8192) })).max(128),
});
export type SemanticReviewPremises = z.infer<typeof SemanticReviewPremises>;
export const RetainedSemanticProposal = z.strictObject({
  premises: SemanticReviewPremises, resolution: SemanticResolution, output: SemanticReviewOutput,
  proposed_claim_digest: hash, output_claim_digest: hash,
});
export type RetainedSemanticProposal = z.infer<typeof RetainedSemanticProposal>;

/** Server-installed L2/L3 resolver and independent L4 judge. Both calls happen
 * outside the writer transaction; retained premises are checked at completion
 * and again at consumption. A provider failure creates no proposal. */
export interface SemanticReviewProvider {
  readonly profileId: string;
  resolve(input: SemanticReviewPremises): Promise<SemanticResolution>;
  review(input: { premises: SemanticReviewPremises; resolution: SemanticResolution }): Promise<SemanticReviewOutput>;
}

/** Complete post-L2/L3 candidate custody (not a meaning-only dedup key). */
export function semanticReviewClaimBody(claim: SemanticClaim, source: SemanticReviewPremises["source"], resolution: SemanticResolution) {
  return {
    content: claim.content, content_language: claim.content_language, sub_kind: claim.sub_kind,
    modality: claim.modality, confidence: claim.confidence,
    evidence_quote: claim.evidence.kind === "source_locus" ? claim.evidence.quote ?? null : null,
    evidence_kind: claim.evidence.kind === "no_single_locus" ? "no_single_locus" : null,
    span: claim.evidence.kind === "source_locus" && claim.evidence.span ? [claim.evidence.span.start, claim.evidence.span.end] : null,
    time_value: claim.time.time_value, time_utc: claim.time.time_utc, time_precision: claim.time.time_precision,
    entities: resolution.entity_resolutions, speaker: source.speaker, subject_keys: claim.subject_keys,
    predicate_text: claim.predicate_text, scope: claim.scope, scope_complete: claim.scope_complete,
    corrects_local_claim_index: null, correction_scope_text: null, mode: null,
  };
}
