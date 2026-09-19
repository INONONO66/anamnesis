import { describe, expect, test } from "bun:test";
import { ExtractionAttempt, Generation, ModelTask, canonicalExtractionBody } from "./extraction.ts";

describe("G004 extraction contract", () => {
  test("canonicalizes JSON domain with UTF-16 ordering and rejects invalid values", () => {
    expect(canonicalExtractionBody({ "\uffff": 1, "𐀀": 2 })).toBe('{"𐀀":2,"￿":1}');
    expect(() => canonicalExtractionBody({ x: undefined })).toThrow();
    expect(() => canonicalExtractionBody(NaN)).toThrow();
    expect(() => canonicalExtractionBody([,])).toThrow();
    expect(() => canonicalExtractionBody("\ud800")).toThrow();
  });
  test("parses a content-free terminal attempt record", () => {
    const attempt = ExtractionAttempt.parse({
      id: "018f5b5e-7b1e-7abc-8def-123456789012", generation_id: "018f5b5e-7b1e-7abc-8def-123456789013",
      task_id: "018f5b5e-7b1e-7abc-8def-123456789015", source_ingest_seq: 1,
      source_id: "018f5b5e-7b1e-7abc-8def-123456789014", source_revision: "b".repeat(64), body_digest: "a".repeat(64),
      state: "cancelled", reason: "cancelled", disposition: null, created_at: 1, updated_at: 1, lease: null,
      output: null, policy_context: { revision: 1, authority: "installation" }, spans: [],
    });
    expect(attempt.id).toMatch(/^018/);
    expect(canonicalExtractionBody({ claim: "x" })).toBe('{"claim":"x"}');
  });

  test("rejects incomplete task and generation records", () => {
    expect(() => ModelTask.parse({ id: "bad" })).toThrow();
    expect(() => Generation.parse({ id: "bad" })).toThrow();
  });
});
