import { describe, expect, test } from "bun:test";
import { retention, initialStability, adopt, replay } from "./retention.ts";
import { attributeOutcome, utility, normalizedRrf } from "./ranking.ts";
import { countBudget, validateBudget } from "./budget.ts";
import { solvePpr } from "./ppr.ts";
import type { PprInput } from "./ppr.ts";

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
    expect(two.rowSums).toEqual([2, 0]);
    expect(two.values.reduce((a, b) => a + b, 0)).toBeCloseTo(1);
  });
  test("uses role weights for retained parallel arcs", () => {
    const result = solvePpr({ nodes: ["a", "b", "c"], arcs: [{ from: "a", to: "b", role: "light", id: "1" }, { from: "a", to: "c", role: "heavy", id: "2" }, { from: "a", to: "c", role: "heavy", id: "3" }], seeds: new Map([["a", 1]]), roleWeights: { light: 1, heavy: 3 }, maxIter: 1 });
    expect(result.values[1]).toBeCloseTo(0.85 / 7);
    expect(result.values[2]).toBeCloseTo(0.85 * 6 / 7);
  });
  test("reports the actual residual and exact update count", () => {
    const result = solvePpr({ nodes: ["a", "b"], arcs: [{ from: "a", to: "b", role: "X", id: "1" }], seeds: new Map([["a", 1]]), maxIter: 1 });
    expect(result.iterations).toBe(1); expect(result.residualL1).toBeGreaterThan(0); expect(result.residualL1).toBeCloseTo(0.7225); expect(result.iterateDeltaL1).toBeCloseTo(1.7);
  });
  test("rejects invalid options and role weights", () => {
    const graph = { nodes: ["a"], arcs: [], seeds: new Map([["a", 1]]) };
    for (const options of [{ alpha: Number.NaN }, { tolerance: 0 }, { maxIter: 0 }, { roleWeights: { X: 0 } }, { roleWeights: { X: Infinity } }]) expect(() => solvePpr({ ...graph, ...options })).toThrow(RangeError);
  });
  test("rejects duplicate node IDs instead of losing seed mass", () => {
    expect(() => solvePpr({ nodes: ["a", "a"], arcs: [], seeds: new Map([["a", 1]]) })).toThrow(RangeError);
  });
  test("rejects malformed graph collections and arc fields", () => {
    expect(() => Reflect.apply(solvePpr, undefined, [{ nodes: ["a", 1], arcs: [], seeds: new Map([["a", 1]]) }])).toThrow(RangeError);
    expect(() => Reflect.apply(solvePpr, undefined, [{ nodes: ["a"], arcs: [null], seeds: new Map([["a", 1]]) }])).toThrow(RangeError);
    expect(() => Reflect.apply(solvePpr, undefined, [{ nodes: ["a"], arcs: [{ from: "a", to: "a", role: 1, id: "1" }], seeds: new Map([["a", 1]]) }])).toThrow(RangeError);
    expect(() => Reflect.apply(solvePpr, undefined, [{ nodes: ["a"], arcs: [], seeds: [] }])).toThrow(RangeError);
  });
  test.each([0, -1, 2, Number.NaN, Infinity])("rejects per-link weight %s rather than silently ignoring it", (weight) => {
    const arc = { from: "a", to: "a", role: "X", id: "1", weight };
    expect(() => solvePpr({ nodes: ["a"], arcs: [arc], seeds: new Map([["a", 1]]) })).toThrow(RangeError);
  });
  test("rejects overflow in a retained row total", () => {
    const arcs = ["1", "2"].map((id) => ({ from: "a", to: "b", role: "X", id }));
    expect(() => solvePpr({ nodes: ["a", "b"], arcs, seeds: new Map([["a", 1]]), roleWeights: { X: Number.MAX_VALUE } })).toThrow(RangeError);
  });
  test("normalizes subnormal role weights before multiplying by rank", () => {
    const arcs = ["a", "b"].map((id) => ({ from: id, to: id, role: "X", id }));
    const result = solvePpr({ nodes: ["a", "b"], arcs, seeds: new Map([["a", 1], ["b", 1]]), roleWeights: { X: Number.MIN_VALUE } });
    expect([...result.values]).toEqual([0.5, 0.5]);
    expect(result.rowSums).toEqual([Number.MIN_VALUE, Number.MIN_VALUE]);
    expect(result.iterations).toBe(1);
  });
  test("normalizes seeds in node order, not Map insertion order", () => {
    const graph = { nodes: ["c", "a", "b"], arcs: [], alpha: 0 };
    const first = solvePpr({ ...graph, seeds: new Map([["a", 2 ** 53], ["b", 1], ["c", 1]]) });
    const shuffled = solvePpr({ ...graph, seeds: new Map([["c", 1], ["b", 1], ["a", 2 ** 53]]) });
    expect(shuffled).toEqual(first);
    expect([...first.values]).toEqual([1, 2 ** -53, 2 ** -53]);
  });
  test("accumulates retained arcs in byte order rather than locale order", () => {
    const arcs = ["Z", "a", "b"].map((role) => ({ from: "a", to: "b", role, id: role }));
    const input = { nodes: ["a", "b"], arcs, seeds: new Map([["a", 1]]), roleWeights: { Z: 2 ** 53, a: 1, b: 1 }, maxIter: 1 };
    const result = solvePpr(input);
    expect(result.rowSums).toEqual([2 ** 53, 0]);
    expect(solvePpr({ ...input, arcs: [...arcs].reverse(), nodes: ["b", "a"] })).toEqual(result);
  });
  const invalidOptions: Partial<PprInput>[] = [
    ...[Number.NaN, Infinity, -Infinity, -0.1, 1].map((alpha) => ({ alpha })),
    ...[Number.NaN, Infinity, -Infinity, 0, -1].map((tolerance) => ({ tolerance })),
    ...[Number.NaN, Infinity, -Infinity, 0, -1, 1.5, 2 ** 53].map((maxIter) => ({ maxIter })),
    ...[Number.NaN, Infinity, -Infinity, 0, -1].map((weight) => ({ roleWeights: { unused: weight } })),
  ];
  test.each(invalidOptions)("rejects invalid solver options %j", (options) => {
    expect(() => solvePpr({ nodes: ["a"], arcs: [], seeds: new Map([["a", 1]]), ...options })).toThrow(RangeError);
  });
  test.each([Number.NaN, Infinity, -Infinity, -1, 0])("rejects invalid singleton seed %s", (value) => {
    expect(() => solvePpr({ nodes: ["a"], arcs: [], seeds: new Map([["a", value]]) })).toThrow(RangeError);
  });
  test("rejects empty, unknown and overflowing seed distributions", () => {
    const graph = { nodes: ["a", "b"], arcs: [] };
    for (const seeds of [new Map<string, number>(), new Map([["c", 1]]), new Map([["a", Number.MAX_VALUE], ["b", Number.MAX_VALUE]])]) {
      expect(() => solvePpr({ ...graph, seeds })).toThrow(RangeError);
    }
    expect(() => solvePpr({ nodes: [], arcs: [], seeds: new Map() })).toThrow(RangeError);
  });
  test.each([{ from: "missing", to: "a" }, { from: "a", to: "missing" }])("rejects unknown arc endpoints %j", (endpoints) => {
    expect(() => solvePpr({ nodes: ["a"], arcs: [{ ...endpoints, role: "X", id: "1" }], seeds: new Map([["a", 1]]) })).toThrow(RangeError);
  });
  test("redistributes all-dangling mass uniformly with unequal seeds", () => {
    const result = solvePpr({ nodes: ["b", "a"], arcs: [], seeds: new Map([["b", 1], ["a", 3]]) });
    expect([...result.values]).toEqual([0.5375, 0.4625]);
    expect(result.rowSums).toEqual([0, 0]);
    expect(result.mass).toBe(1);
    expect(result.iterations).toBe(2);
    expect(result.iterateDeltaL1).toBe(0);
    expect(result.residualL1).toBe(0);
    expect(result.errorBoundL1).toBe(0);
  });
  test("requires a strict delta threshold and counts only completed updates", () => {
    const arcs = [{ from: "a", to: "b", role: "X", id: "1" }, { from: "b", to: "a", role: "X", id: "1" }];
    const result = solvePpr({ nodes: ["a", "b"], arcs, seeds: new Map([["a", 1]]), alpha: 0.5, tolerance: 1 });
    expect([...result.values]).toEqual([0.75, 0.25]);
    expect(result.iterations).toBe(2);
    expect(result.iterateDeltaL1).toBe(0.5);
    expect(result.residualL1).toBe(0.25);
    expect(result.errorBoundL1).toBe(0.5);
  });
  test("matches an independent retained-row operator and its fixed point", () => {
    // a -> b with weight 1, a -> c with two weight-3 links; b -> a; c dangling.
    const arcs = [
      { from: "a", to: "b", role: "light", id: "1" },
      { from: "a", to: "c", role: "heavy", id: "2" },
      { from: "a", to: "c", role: "heavy", id: "3" },
      { from: "b", to: "a", role: "light", id: "1" },
    ];
    const graph = { nodes: ["c", "b", "a"], arcs, seeds: new Map([["b", 1], ["a", 3]]), roleWeights: { light: 1, heavy: 3 } };
    const apply = ([a, b, c]: readonly [number, number, number]): [number, number, number] => [
      0.15 * 0.75 + 0.85 * (b + c / 3),
      0.15 * 0.25 + 0.85 * (a / 7 + c / 3),
      0.85 * (6 * a / 7 + c / 3),
    ];
    const l1 = (left: readonly number[], right: readonly number[]): number => left.reduce((sum, value, i) => sum + Math.abs(value - (right.at(i) ?? 0)), 0);
    let previous: [number, number, number] = [0.75, 0.25, 0];
    for (const maxIter of [1, 2, 3]) {
      const expected = apply(previous);
      const result = solvePpr({ ...graph, maxIter });
      result.values.forEach((value, i) => expect(value).toBeCloseTo(expected.at(i) ?? 0, 14));
      expect(result.rowSums).toEqual([7, 1, 0]);
      expect(result.mass).toBeCloseTo(1, 14);
      expect(result.iterations).toBe(maxIter);
      expect(result.iterateDeltaL1).toBeCloseTo(l1(expected, previous), 14);
      expect(result.residualL1).toBeCloseTo(l1(apply(expected), expected), 14);
      expect(result.errorBoundL1).toBeCloseTo(0.85 / 0.15 * l1(expected, previous), 13);
      previous = expected;
    }
    const result = solvePpr(graph);
    // Exact rational solution of this three-node operator (not solver-generated).
    const fixedPoint = [23177 / 58420, 11681 / 58420, 11781 / 29210];
    expect(l1([...result.values], fixedPoint)).toBeLessThanOrEqual(result.errorBoundL1 + 1e-14);
    expect(result.iterateDeltaL1).toBeLessThan(1e-4);
    expect(result.iterations).toBeLessThanOrEqual(61);
  });
  test("converges on the default worst-case two-cycle without reporting the residual evaluation as an update", () => {
    const arcs = [{ from: "a", to: "b", role: "X", id: "1" }, { from: "b", to: "a", role: "X", id: "1" }];
    const result = solvePpr({ nodes: ["a", "b"], arcs, seeds: new Map([["a", 1]]) });
    expect(result.iterations).toBe(61);
    expect(result.iterateDeltaL1).toBeLessThan(1e-4);
    expect(result.residualL1).toBeGreaterThan(0);
    expect(Math.abs((result.values.at(0) ?? 0) - 20 / 37) + Math.abs((result.values.at(1) ?? 0) - 17 / 37)).toBeLessThanOrEqual(result.errorBoundL1 + 1e-14);
  });
});
