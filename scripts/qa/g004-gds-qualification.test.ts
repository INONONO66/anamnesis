import { expect, test } from "bun:test";
import { exportFixedCsr, solveFixedCsr } from "../../packages/core/src/dynamics/ppr.ts";
import { compareScores, exactReference, envelopeRefusal, FIXED20, ROLE_WEIGHTS, SEEDS20 } from "./g004-ppr-oracle.ts";

test("direct oracle agrees with a hand-solved two-cycle", () => {
  const exact = exactReference({ nodes: ["a", "b"], offsets: [0, 1, 2], targets: [1, 0], roles: ["X", "X"] }, new Map([["a", 1]]), {});
  expect(exact.values[0]).toBeCloseTo(1 / 1.85, 14);
  expect(exact.values[1]).toBeCloseTo(0.85 / 1.85, 14);
  expect(exact.residual(exact.values)).toBeLessThan(1e-14);
});

test("fixed capture export retains the independent operator and rejects per-link weights", () => {
  const arcs = FIXED20.nodes.flatMap((from, row) => FIXED20.targets.slice(FIXED20.offsets[row], FIXED20.offsets[row + 1]).map((to, j) => ({ from, to: FIXED20.nodes[to]!, role: FIXED20.roles[FIXED20.offsets[row]! + j]!, id: String(j) })));
  const exported = exportFixedCsr({ nodes: [...FIXED20.nodes].reverse(), arcs: [...arcs].reverse() });
  const reference = exactReference(FIXED20, SEEDS20, ROLE_WEIGHTS);
  expect(exactReference(exported, SEEDS20, ROLE_WEIGHTS).transition).toEqual(reference.transition);
  expect(exportFixedCsr({ nodes: FIXED20.nodes, arcs })).toEqual(exported);
  expect(() => exportFixedCsr({ nodes: FIXED20.nodes, arcs: [{ ...arcs[0]!, weight: 1 }] })).toThrow(RangeError);
});

test("comparison rejects wrong operators and permits near-tied rank differences", () => {
  const exact = exactReference(FIXED20, SEEDS20, ROLE_WEIGHTS);
  const wrong = [...exact.values]; wrong[0]! += 0.01; wrong[1]! -= 0.01;
  expect(() => compareScores(FIXED20.nodes, wrong, exact.values, exact.residual(wrong), exact.residual(exact.values))).toThrow();
  const uniform = Array<number>(20).fill(0.05), close = uniform.map((value, i) => value + (i < 10 ? -1e-6 : 1e-6));
  const report = compareScores(FIXED20.nodes, close, uniform, 3e-6, 0);
  expect(report.topK[0]!.overlap).toBe(0);
  expect(report.topK[0]!.gap).toBe(0);
});

test("absent, partial and unpinned envelope access refuses qualification", () => {
  expect(envelopeRefusal(null)).toBe("envelope_unavailable");
  expect(envelopeRefusal({ error: { code: "degree_probe_unavailable" } })).toBe("degree_probe_unavailable");
  for (const value of [{ nodes: [], arcs: [], probes: [] }, { nodes: FIXED20.nodes, truncated: true }, { policy_revision: 0, generation: null }]) {
    expect(envelopeRefusal(value)).toBe("envelope_provenance_unavailable");
  }
});

// A real pre-implementation admission check, not a fabricated failing assertion.
test("independent fixed-20 qualification is available and meets numeric gates", async () => {
  const oracle = await import("./g004-ppr-oracle.ts");
  const report = oracle.qualifyFixed20();
  expect(report.qualified).toBe(true);
  expect(report.nodes).toBe(20);
  expect(report.sensitivity.map(control => control.name)).toEqual(["uniform-role-weights", "changed-seeds", "redirected-arc", "dangling-row-filled"]);
  expect(report.sensitivity.every(control => control.rejectedAgainstOriginal)).toBe(true);
});

test("CSR export preserves empty rows before and between populated rows", () => {
  const csr = exportFixedCsr({ nodes: ["d", "c", "b", "a"], arcs: [
    { from: "d", to: "a", role: "MENTIONS", id: "2" },
    { from: "b", to: "c", role: "MENTIONS", id: "1" },
  ] });
  expect(csr.offsets).toEqual([0, 0, 1, 1, 2]);
  expect(csr.targets).toEqual([2, 0]);
});

test("CSR rejects fractional/decreasing offsets and invalid targets before slicing", () => {
  const csr = { nodes: ["a", "b"], offsets: [0, 0, 1], targets: [0], roles: ["MENTIONS"] };
  for (const offsets of [[0, -1, 1], [0, 0.5, 1], [0, 2, 1]]) {
    expect(() => solveFixedCsr({ ...csr, offsets }, new Map([["a", 1]]))).toThrow(RangeError);
  }
  for (const target of [-1, 0.5, 2, NaN]) {
    expect(() => solveFixedCsr({ ...csr, targets: [target] }, new Map([["a", 1]]))).toThrow(RangeError);
  }
});
