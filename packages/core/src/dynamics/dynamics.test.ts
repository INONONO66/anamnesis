import { describe, expect, test } from "bun:test";
import { retention, initialStability, adopt, replay } from "./retention.ts";
import { attributeOutcome, utility, normalizedRrf } from "./ranking.ts";
import { countBudget, validateBudget } from "./budget.ts";
import { solvePpr } from "./ppr.ts";

describe("retention", () => {
  test("power-law and initialization", () => {
    expect(retention(1, 1)).toBeCloseTo(0.9, 12);
    expect(initialStability(0)).toBe(1);
    expect(initialStability(0.5)).toBe(1.5);
  });
  test("only adoption changes state and replay can lower cache", () => {
    const start = { stability: 1.5, lastHit: 0, hitCount: 0 };
    expect(adopt(start, 86_400_000, 0).stability).toBe(start.stability);
    const changed = adopt(start, 86_400_000, 1);
    expect(changed.stability).toBeGreaterThan(start.stability);
    expect(replay(0.5, 0, [{ id: "b", at: 86_400_000, kind: "recall_hit", kappa: 1 }, { id: "a", at: 0, kind: "outcome", kappa: 0 }]).hitCount).toBe(2);
  });
});

describe("ranking", () => {
  test("outcome weights normalize without a second cap", () => {
    const weights = attributeOutcome([{ id: "a", rank: 0, sources: ["E1"] }, { id: "b", rank: 1, sources: ["E1", "E2"] }], ["a", "b"]);
    expect(weights.get("E1")).toBeCloseTo(5 / 6);
    expect(weights.get("E2")).toBeCloseTo(1 / 6);
    expect([...weights.values()].reduce((a, b) => a + b, 0)).toBeCloseTo(1);
    expect(attributeOutcome([{ id: "a", rank: 4, sources: ["E1"] }], ["a"]).get("E1")).toBe(1);
  });
  test("RRF redistributes absent lists", () => {
    expect(normalizedRrf({ vector: [{ id: "x" }] }, "x")).toBeCloseTo(1 / 61);
    expect(normalizedRrf({ vector: [{ id: "x" }], bm25: [] }, "x")).toBeCloseTo(1 / 61);
  });
  test("zero reward differs from missing", () => { expect(utility(4, 0, 0, 0)).toBe(0); expect(utility(4, 0, 0, 1)).toBe(0); });
});

describe("budget", () => {
  test("exact units and surrogate rejection", () => {
    expect(countBudget("A\né🙂", "utf8_bytes")).toBe(8);
    expect(countBudget("A\né🙂", "unicode_scalars")).toBe(4);
    expect(() => countBudget("\ud800", "utf8_bytes")).toThrow();
    expect(() => validateBudget({ unit: "tokens", limit: 1 })).toThrow();
  });
});

describe("PPR", () => {
  test("dangling singleton and normalized parallel rows", () => {
    const one = solvePpr({ nodes: ["a"], arcs: [], seeds: new Map([["a", 2]]) });
    expect(one.values[0]).toBe(1); expect(one.mass).toBeCloseTo(1);
    const two = solvePpr({ nodes: ["b", "a"], arcs: [{ from: "a", to: "b", role: "X", id: "2" }, { from: "a", to: "b", role: "X", id: "1" }], seeds: new Map([["a", 1]]) });
    expect(two.rowSums.every((x) => Math.abs(x - 1) < 1e-12)).toBe(true);
    expect(two.values.reduce((a, b) => a + b, 0)).toBeCloseTo(1);
  });
});
