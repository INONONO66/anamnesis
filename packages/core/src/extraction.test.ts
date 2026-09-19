import { describe, expect, test } from "bun:test";
import { DeterministicExtractionProvider, validateProviderOutput } from "./extraction.ts";

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
