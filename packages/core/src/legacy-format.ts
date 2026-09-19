import { z } from "zod";
import { ElementSchemaId, Origin, TimePoint, validateElementSemantics } from "@anamnesis/protocol";

/** Original-format inventory, reconstructed from c8a075f^ (post #167, pre #194).
 * Required mass/properties were materialized by historical append. Decoding
 * must not synthesize defaults or confer modern semantic eligibility. */
export const HistoricalElement = z.object({
  schema: ElementSchemaId,
  time: TimePoint.optional(),
  content: z.string().min(1),
  origin: Origin,
  mass: z.number().min(0).max(1),
  properties: z.record(z.string(), z.json()),
}).strict();

const SemanticElement = HistoricalElement.strip().superRefine(validateElementSemantics);
export type EligibilityReason = "missing-time" | "invalid-sub-kind";

/** Call only after structural decoding; these are eligibility, not hash errors. */
export function historicalEligibility(element: z.infer<typeof HistoricalElement>): EligibilityReason[] {
  const result = SemanticElement.safeParse(element);
  if (result.success) return [];
  return result.error.issues.map(issue => {
    if (issue.path[0] === "time") return "missing-time";
    if (issue.path[0] === "properties" && issue.path[1] === "sub_kind") return "invalid-sub-kind";
    throw result.error;
  });
}
