import { describe, expect, test } from "bun:test";
import { DeterministicExtractionProvider, ExtractionProviderError, validateModelOutput, validateProviderOutput } from "./extraction.ts";

describe("G004 provider wiring", () => {
  test("fixture is deterministic and canonical output is bounded", async () => {
    const p = new DeterministicExtractionProvider();
    const a = await p.extract({ text: "hello", task: "claim" });
    const b = await p.extract({ text: "hello", task: "claim" });
    expect(validateProviderOutput(a)).toEqual(validateProviderOutput(b));
  });
  test("rejects oversized retained output", () => {
    expect(() => validateProviderOutput("x".repeat(65537))).toThrow("output_too_large");
  });
});

test("judge_relations attempts validate as span-free unknown dispositions", () => {
  const output = { task: "judge_relations", relation_context_digest: "c".repeat(64), language: "en", modality: "text",
    judgements: [{ candidate_id: "01900000-0000-7000-8000-000000000003", relation: "duplicate", confidence: 0.8, reason: "same preference" }] };
  const validated = validateModelOutput(output, "judge_relations");
  expect(validated.disposition).toBe("unknown");
  expect(validated.spans).toEqual([]);
  expect(validated.output.spans).toEqual([]);
  expect(() => validateModelOutput(output, "judge_claims")).toThrow(ExtractionProviderError);
});
