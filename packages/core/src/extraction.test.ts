import { describe, expect, test } from "bun:test";
import { DeterministicExtractionProvider, ExtractionProviderError, splitExtractionWindow, validateModelOutput, validateProviderOutput } from "./extraction.ts";

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

test("window output preserves target order and isolates missing or duplicate target IDs", async () => {
  const ids = ["01900000-0000-7000-8000-000000000001", "01900000-0000-7000-8000-000000000002"];
  const provider = new DeterministicExtractionProvider();
  const output = await provider.extract({ task: "claim_window", episodes: ids.map(id => ({ id, text: id })), context: [] });
  const splits = splitExtractionWindow(output, ids);
  expect(splits.map(value => validateModelOutput(value, "claim").disposition)).toEqual(["suppress", "suppress"]);
  const episode = { id: ids[0], claims: [], language: "en", modality: "text" };
  expect(splitExtractionWindow({ task: "claim_window", episodes: [episode] }, ids)[1]).toMatchObject({ reason: "provider_mismatch", detail: "window_shape" });
  expect(splitExtractionWindow({ task: "claim_window", episodes: [episode, episode] }, ids)).toEqual([
    expect.objectContaining({ detail: "window_shape" }), expect.objectContaining({ detail: "window_shape" }),
  ]);
  for (const value of [{ task: "claim", claims: [] }, { task: "claim_window", episodes: [{ ...episode, id: "unknown" }] }])
    expect(splitExtractionWindow(value, ids).every(result => result instanceof ExtractionProviderError && result.detail === "window_shape")).toBe(true);
});

test("window outputs apply the retained byte bound per Episode rather than to the combined envelope", () => {
  const text = "a".repeat(8000);
  const episodes = Array.from({ length: 8 }, (_, index) => ({
    id: `01900000-0000-7000-8000-${String(index + 1).padStart(12, "0")}`,
    claims: [{ text, evidence: { start: 0, end: text.length, text } }], language: "en", modality: "text",
  }));
  const split = splitExtractionWindow({ task: "claim_window", episodes }, episodes.map(episode => episode.id));
  expect(split.map(output => validateModelOutput(output, "claim").disposition)).toEqual(Array(8).fill("retain"));
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
