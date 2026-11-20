import { expect, test } from "bun:test";
import { EmptyClaimJudgeProvider } from "./empty-claim-judge-provider.ts";
import { validateModelOutput, type ExtractionProvider, type ExtractionProviderInput } from "./extraction.ts";

function recordingProvider() {
  const calls: ExtractionProviderInput[] = [];
  const provider: ExtractionProvider = {
    model: "judge-fixture", modelIncarnation: "b".repeat(64), reportedModelIncarnation: "judge-fixture-20260101",
    async extract(input: ExtractionProviderInput) { calls.push(input); return { forwarded: input.task }; },
  };
  return { calls, provider };
}
const context = (claims: number) => ({
  task_id: "0196e0f0-0000-7000-8000-000000000001", attempt_id: "0196e0f0-0000-7000-8000-000000000002", body_digest: "c".repeat(64),
  claims: Array.from({ length: claims }, (_, index) => ({ text: `claim ${index}`, confidence: 0.9, evidence: { text: "quote", start: 0, end: 5 } })),
});

test("a judge over zero claims is answered locally with a valid empty judgement", async () => {
  const { calls, provider } = recordingProvider();
  const judge = new EmptyClaimJudgeProvider(provider);
  const output = await judge.extract({ task: "judge_claims", text: "an episode with nothing memorable", claim_context: context(0) });
  expect(calls).toEqual([]);
  expect(output).toEqual({ task: "judge_claims", claim_body_digest: "c".repeat(64), decisions: [], language: "und", modality: "unknown" });
  expect(validateModelOutput(output, "judge_claims").disposition).toBe("unknown");
});

test("judges with claims and every other task reach the inner provider under its identity", async () => {
  const { calls, provider } = recordingProvider();
  const judge = new EmptyClaimJudgeProvider(provider);
  await judge.extract({ task: "judge_claims", text: "episode", claim_context: context(1) });
  await judge.extract({ task: "claim", text: "episode" });
  await judge.extract({ task: "claim_window", episodes: [{ id: "e", text: "episode" }], context: [] });
  expect(calls.map(call => call.task)).toEqual(["judge_claims", "claim", "claim_window"]);
  expect([judge.model, judge.modelIncarnation, judge.reportedModelIncarnation]).toEqual(["judge-fixture", "b".repeat(64), "judge-fixture-20260101"]);
});
