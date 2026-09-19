import { z } from "zod";
import { ExtractionModelOutput, ExtractionSpan } from "../../protocol/src/extraction.ts";
import { countBudget } from "./dynamics/budget.ts";
import { validateModelOutput, validateSourceSpans, type ExtractionProviderInput } from "./extraction.ts";

const [claim, judge, judgeClaims] = ExtractionModelOutput.options;
const quote = ExtractionSpan.shape.text.min(1);
function withEvidence<T extends z.ZodType>(evidence: T) {
  return z.discriminatedUnion("task", [
    claim.extend({ claims: claim.shape.claims.element.extend({ evidence }).array().max(64) }),
    judge.extend({ spans: evidence.array().max(64) }),
    judgeClaims.extend({ decisions: judgeClaims.shape.decisions.element.extend({ evidence }).array().max(64) }),
  ]);
}
// Models copy quotes; only trusted local code calculates byte offsets.
export const chatExtractionSchema = z.toJSONSchema(withEvidence(quote), { target: "draft-2020-12" });
// Accept the prior wire shape as well, but never trust model-generated offsets.
const receivedOutput = withEvidence(z.union([quote, z.strictObject({
  text: quote, start: z.number().optional(), end: z.number().optional(),
})]));

export function normalizeChatExtraction(value: unknown, input: ExtractionProviderInput): ExtractionModelOutput {
  const output = receivedOutput.parse(value);
  countBudget(input.text, "utf8_bytes"); // Reject invalid Unicode before Buffer replacement encoding.
  const source = Buffer.from(input.text, "utf8");
  const span = (evidence: string | { text: string; start?: number | undefined }, claimIndex?: number) => {
    const text = typeof evidence === "string" ? evidence : evidence.text;
    const quote = Buffer.from(text, "utf8"), claimed = typeof evidence === "string" ? 0 : evidence.start ?? 0;
    let start = source.indexOf(quote);
    if (start < 0) return null;
    for (let next = source.indexOf(quote, start + 1); next >= 0; next = source.indexOf(quote, next + 1)) {
      if (Math.abs(next - claimed) < Math.abs(start - claimed)) start = next;
    }
    const located = { text, start, end: start + quote.length };
    // An audit judge must preserve its parent's exact occurrence of a repeated quote.
    const parent = claimIndex === undefined ? undefined : input.claim_context?.claims[claimIndex]?.evidence;
    if (parent && parent.text !== text) throw new Error("judge evidence differs from parent");
    const result = parent ?? located;
    validateSourceSpans(input.text, [result]);
    return result;
  };
  const standaloneSpans = output.task === "judge" ? output.spans.flatMap(evidence => { const anchored = span(evidence); return anchored ? [anchored] : []; }) : [];
  const normalized = ExtractionModelOutput.parse(output.task === "claim"
    ? { ...output, claims: output.claims.flatMap(claim => { const evidence = span(claim.evidence); return evidence ? [{ ...claim, evidence }] : []; }) }
    : output.task === "judge"
      ? { ...output, spans: standaloneSpans, disposition: standaloneSpans.length ? output.disposition : "suppress" }
      : { ...output, decisions: output.decisions.flatMap(decision => { const evidence = span(decision.evidence, decision.claim_index); return evidence ? [{ ...decision, evidence }] : []; }) });
  validateModelOutput(normalized, input.task);
  return normalized;
}
