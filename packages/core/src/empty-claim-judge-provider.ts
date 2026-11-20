import type { ExtractionProvider, ExtractionProviderInput } from "./extraction.ts";

/** Answers a claim judge that has nothing to judge without a provider call. A judge owes one decision per claim, so
 * for an empty claim set the only valid answer is no decisions, whatever model is asked; every other call, including
 * a judge with claims, goes to the inner provider (the pacer), so a local answer never spends a paced slot. */
export class EmptyClaimJudgeProvider implements ExtractionProvider {
  readonly model: string;
  readonly modelIncarnation: string;

  constructor(private readonly inner: ExtractionProvider) {
    this.model = inner.model; this.modelIncarnation = inner.modelIncarnation;
  }

  get reportedModelIncarnation(): string | undefined { return this.inner.reportedModelIncarnation; }

  async extract(input: ExtractionProviderInput): Promise<unknown> {
    if (input.task === "judge_claims" && input.claim_context && input.claim_context.claims.length === 0) {
      return { task: "judge_claims", claim_body_digest: input.claim_context.body_digest, decisions: [], language: "und", modality: "unknown" };
    }
    return this.inner.extract(input);
  }
}
