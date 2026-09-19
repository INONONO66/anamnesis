import { expect, test } from "bun:test";
import { ExtractionSelection, ReadExtractionCoverage, SelectExtractionGeneration } from "./extraction.ts";
const id = "018f5b5e-7b1e-7abc-8def-123456789012";
const request = { generation_id: id, expected_generation_id: null, expected_selector_version: 0 };
test("selector expectations are mandatory strict safe integers, never client-owned state", () => {
  expect(SelectExtractionGeneration.parse(request)).toEqual(request);
  const { expected_selector_version: _, ...legacy } = request;
  expect(SelectExtractionGeneration.safeParse(legacy).success).toBe(false);
  for (const value of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1, null, "0"]) {
    expect(SelectExtractionGeneration.safeParse({ ...request, expected_selector_version: value }).success).toBe(false);
  }
  for (const forged of [{ selector_version: 1 }, { readiness: true }, { source_high_watermark: 0 }]) {
    expect(SelectExtractionGeneration.safeParse({ ...request, ...forged }).success).toBe(false);
  }
});
test("reader revalidation keeps explicit epoch and server selection is versioned", () => {
  expect(ReadExtractionCoverage.parse({ generation_id: id, expected_selector_version: 3 })).toEqual({ generation_id: id, expected_selector_version: 3 });
  expect(ReadExtractionCoverage.safeParse({ generation_id: id, selector_version: 3 }).success).toBe(false);
  expect(ExtractionSelection.safeParse({ generation_id: id }).success).toBe(false);
  expect(ExtractionSelection.parse({ generation_id: null, selector_version: 0 }).selector_version).toBe(0);
});
