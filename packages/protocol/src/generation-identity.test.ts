import { describe, expect, test } from "bun:test";
import { bindGenerationProfile, type GenerationIdentity } from "./generation-identity.ts";

const generation: GenerationIdentity = {
  generation_id: "018f5b5e-7b1e-7abc-8def-123456789012",
  generation_version: "g004-extraction-v1",
};

describe("G004 generation identity contract", () => {
  test("RED: numeric ordered-regression profile cannot be coerced into UUID identity", () => {
    expect(() => bindGenerationProfile(generation, {
      generation: 7,
      profile_version: "ordered-regression-v1",
      coverage_generation_id: generation.generation_id,
    })).toThrow("generation_identity_refused");
  });

  test("binds exact generation and coverage identity and emits a receipt", () => {
    const bound = bindGenerationProfile(generation, {
      generation_id: generation.generation_id,
      profile_version: "ordered-regression-v1",
      coverage_generation_id: generation.generation_id,
    });
    expect(bound.identity).toEqual(generation);
    expect(bound.receipt).toMatchObject({ generation_id: generation.generation_id, generation_version: generation.generation_version });
    expect(bound.receipt.digest).toMatch(/^[0-9a-f]{64}$/);
  });

  test("refuses ABA and mismatched coverage without changing identity", () => {
    expect(() => bindGenerationProfile(generation, {
      generation_id: "018f5b5e-7b1e-7abc-8def-123456789013",
      profile_version: "ordered-regression-v1",
      coverage_generation_id: generation.generation_id,
    })).toThrow("generation_identity_mismatch");
    expect(() => bindGenerationProfile(generation, {
      generation_id: generation.generation_id,
      profile_version: "ordered-regression-v1",
      coverage_generation_id: "018f5b5e-7b1e-7abc-8def-123456789013",
    })).toThrow("coverage_identity_mismatch");
  });
});
