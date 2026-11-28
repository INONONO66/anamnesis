import { z } from "zod";
import { SemanticClaim, SemanticSourceContext } from "./semantic-claim.ts";
import { FactRelationKind } from "./extraction.ts";

const id = z.uuidv7();

/** One applied relation verdict. `outcome` is the applier's decision: a link was
 * written, or the verdict was held back for a stated mechanical reason. */
export const FactRelationDecision = z.strictObject({
  candidate_id: id, relation: FactRelationKind, confidence: z.number().min(0).max(1), reason: z.string().min(1).max(512),
  outcome: z.enum(["linked", "unrelated", "low_confidence", "chain_refused", "stale_candidate", "duplicate"]), link_id: id.optional(),
});
export type FactRelationDecision = z.infer<typeof FactRelationDecision>;
export const MaterializationResult = z.strictObject({ created: z.boolean(), fact_id: id, link_id: id, duplicate_of: id.optional(), relations: z.array(FactRelationDecision).max(16).optional() });
export type MaterializationResult = z.infer<typeof MaterializationResult>;

/** Resolution used by extraction to validate a claim against its source. */
export const SemanticResolution = SemanticSourceContext.pick({ entity_resolutions: true, attribution_speakers: true, allow_no_single_locus: true })
  .extend({ content_language: SemanticClaim.shape.content_language });
export type SemanticResolution = z.infer<typeof SemanticResolution>;
