import { expect, test } from "bun:test";
import { z } from "zod";
import { chatExtractionSchema, normalizeChatExtraction } from "./extraction-chat-output.ts";

const text = "Fact. Fact.";
const evidence = { start: 6, end: 11, text: "Fact." };
const claim_context = { task_id: "01900000-0000-7000-8000-000000000001", attempt_id: "01900000-0000-7000-8000-000000000002", body_digest: "a".repeat(64), claims: [{ text: "Fact.", evidence }] };

test("chat schema requests quotes rather than model-generated offsets", () => {
  const schema = z.fromJSONSchema(structuredClone(chatExtractionSchema));
  const output = { task: "claim", claims: [{ text: "Fact.", evidence: "Fact." }], language: "en", modality: "text" };
  expect(schema.safeParse(output).success).toBe(true);
  expect(schema.safeParse({ ...output, claims: [{ text: "Fact.", evidence }] }).success).toBe(false);
});
test("standalone judge quotes become exact byte spans", () => {
  expect(normalizeChatExtraction({ task: "judge", disposition: "retain", spans: ["Fact."], language: "en", modality: "text" }, { task: "judge", text })).toEqual({
    task: "judge", disposition: "retain", spans: [{ start: 0, end: 5, text: "Fact." }], language: "en", modality: "text",
  });
});
test("claim judge preserves the parent's occurrence of repeated evidence", () => {
  const output = { task: "judge_claims", claim_body_digest: claim_context.body_digest, decisions: [{ claim_index: 0, disposition: "retain", evidence: "Fact." }], language: "en", modality: "text" } as const;
  expect(normalizeChatExtraction(output, { task: "judge_claims", text, claim_context })).toEqual({ ...output, decisions: [{ ...output.decisions[0], evidence }] });
  expect(() => normalizeChatExtraction({ ...output, decisions: [{ claim_index: 0, disposition: "retain", evidence: "Fact" }] }, { task: "judge_claims", text, claim_context })).toThrow();
});
test("unsupported judge evidence is removed without fabricating a quote", () => {
  expect(normalizeChatExtraction({ task: "judge", disposition: "retain", spans: ["absent"], language: "en", modality: "text" }, { task: "judge", text })).toEqual({
    task: "judge", disposition: "suppress", spans: [], language: "en", modality: "text",
  });
  const output = { task: "judge_claims", claim_body_digest: claim_context.body_digest, decisions: [{ claim_index: 0, disposition: "retain", evidence: "absent" }], language: "en", modality: "text" } as const;
  expect(normalizeChatExtraction(output, { task: "judge_claims", text, claim_context })).toEqual({ ...output, decisions: [] });
});
test("normalization never invents fields or admits invalid Unicode", () => {
  for (const value of [null, { claims: [] }, { task: "claim", claims: [], language: "en", modality: "text", extra: true }]) {
    expect(() => normalizeChatExtraction(value, { task: "claim", text })).toThrow();
  }
  expect(() => normalizeChatExtraction({ task: "claim", claims: [{ text: "Fact.", evidence: "Fact." }], language: "en", modality: "text" }, { task: "claim", text: "\ud800Fact." })).toThrow();
});
