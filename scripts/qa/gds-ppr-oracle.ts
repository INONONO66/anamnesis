import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { solveFixedCsr, type FixedCsr } from "../../packages/core/src/dynamics/ppr.ts";

export const ROLE_WEIGHTS = { DERIVED_FROM: 1, HAS_MEMBER: 2, MENTIONS: 3, NEXT_EPISODE: 0.5, RELATES_TO: 4 };
const roleNames = Object.keys(ROLE_WEIGHTS);
export const FIXED20: FixedCsr = {
  nodes: Array.from({ length: 20 }, (_, i) => `01900000-0000-7000-8000-${i.toString(16).padStart(12, "0")}`),
  offsets: [0, 0, 3, 5, 7, 9, 9, 12, 14, 16, 18, 20, 20, 22, 24, 26, 28, 29, 30, 31, 31],
  // Leading/interior/trailing dangling rows, parallel arcs, asymmetric rows,
  // a disconnected three-cycle, and nonuniform role weights and seeds.
  targets: [2, 2, 3, 1, 4, 4, 5, 1, 6, 7, 8, 0, 6, 9, 9, 10, 10, 11, 6, 12, 13, 14, 12, 15, 15, 19, 1, 12, 17, 18, 16],
  roles: Array.from({ length: 31 }, (_, i) => roleNames[i % roleNames.length]!),
};
export const SEEDS20 = new Map([[FIXED20.nodes[1]!, 7], [FIXED20.nodes[10]!, 2], [FIXED20.nodes[16]!, 1]]);
export const PIN20 = { policy_revision: 0, extraction_generation: null, community_generation: null, T: 1788220800000, capture: "synthetic-fixed20-v1" };
export const THRESHOLDS = { mass: 1e-12, referenceResidual: 1e-8, l1: 7e-4, roundoff: 1e-12 };

/** Independent dense direct solve, not iteration and not production row helpers.
 * Assemble (I - alpha P^T)x = (1-alpha)s, partial-pivot elimination,
 * then back substitution. "Exact" means full operator, binary64 arithmetic.
 */
export function exactReference(csr: FixedCsr, seeds: Map<string, number>, weights: Record<string, number>, alpha = 0.85) {
  const n = csr.nodes.length;
  assert.ok(n > 0 && n <= 20, "offline dense oracle bound");
  const transition = Array.from({ length: n }, () => Array<number>(n).fill(0));
  for (let row = 0; row < n; row++) {
    const begin = csr.offsets[row]!, end = csr.offsets[row + 1]!;
    if (begin === end) { transition[row]!.fill(1 / n); continue; }
    // Aggregate parallel destinations before normalization, unlike the sparse kernel.
    for (let arc = begin; arc < end; arc++) transition[row]![csr.targets[arc]!]! += weights[csr.roles[arc]!] ?? 1;
    const total = transition[row]!.reduce((a, b) => a + b, 0);
    transition[row] = transition[row]!.map(value => value / total);
  }
  const seed = csr.nodes.map(id => seeds.get(id) ?? 0), total = seed.reduce((a, b) => a + b, 0);
  const rhs = seed.map(value => (1 - alpha) * value / total);
  const matrix = Array.from({ length: n }, (_, row) => Array.from({ length: n }, (_, col) => Number(row === col) - alpha * transition[col]![row]!));
  for (let col = 0; col < n; col++) {
    let pivot = col;
    for (let row = col + 1; row < n; row++) if (Math.abs(matrix[row]![col]!) > Math.abs(matrix[pivot]![col]!)) pivot = row;
    assert.ok(Math.abs(matrix[pivot]![col]!) > Number.EPSILON, "singular reference system");
    [matrix[col], matrix[pivot]] = [matrix[pivot]!, matrix[col]!];
    [rhs[col], rhs[pivot]] = [rhs[pivot]!, rhs[col]!];
    for (let row = col + 1; row < n; row++) {
      const factor = matrix[row]![col]! / matrix[col]![col]!;
      matrix[row]![col] = 0;
      for (let j = col + 1; j < n; j++) matrix[row]![j]! -= factor * matrix[col]![j]!;
      rhs[row]! -= factor * rhs[col]!;
    }
  }
  const values = Array<number>(n).fill(0);
  for (let row = n - 1; row >= 0; row--) {
    let value = rhs[row]!;
    for (let col = row + 1; col < n; col++) value -= matrix[row]![col]! * values[col]!;
    values[row] = value / matrix[row]![row]!;
  }
  const residual = (vector: ArrayLike<number>) => csr.nodes.reduce((sum, _, destination) => {
    let value = (1 - alpha) * seed[destination]! / total;
    for (let source = 0; source < n; source++) value += alpha * transition[source]![destination]! * vector[source]!;
    return sum + Math.abs(value - vector[destination]!);
  }, 0);
  return { values, residual, transition };
}

export function compareScores(nodes: string[], local: ArrayLike<number>, reference: ArrayLike<number>, localResidual: number, referenceResidual: number) {
  for (const vector of [local, reference]) {
    assert.equal(vector.length, nodes.length);
    assert.ok(Array.from(vector).every(value => Number.isFinite(value) && value >= 0));
    assert.ok(Math.abs(Array.from(vector).reduce((a, b) => a + b, 0) - 1) <= THRESHOLDS.mass);
  }
  assert.ok(referenceResidual <= THRESHOLDS.referenceResidual);
  const l1 = nodes.reduce((sum, _, i) => sum + Math.abs(local[i]! - reference[i]!), 0);
  assert.ok(l1 <= THRESHOLDS.l1);
  const residualBound = (localResidual + referenceResidual) / 0.15;
  assert.ok(l1 <= residualBound + THRESHOLDS.roundoff);
  const order = (vector: ArrayLike<number>) => nodes.map((id, i) => ({ id, score: vector[i]!, i })).sort((a, b) => b.score - a.score || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const actual = order(local), expected = order(reference);
  const topK = [10, 20, 50].map(k => {
    const effectiveK = Math.min(k, nodes.length), chosen = actual.slice(0, effectiveK), wanted = expected.slice(0, effectiveK);
    const c = wanted.at(-1)!.score, gap = effectiveK < nodes.length ? c - expected[effectiveK]!.score : null;
    const selected = new Set(chosen.map(item => item.id));
    const overlap = wanted.filter(item => selected.has(item.id)).length / effectiveK;
    if (gap !== null && gap > 2 * THRESHOLDS.l1) assert.equal(overlap, 1);
    assert.ok(expected.filter(item => item.score > c + 2 * THRESHOLDS.l1).every(item => selected.has(item.id)));
    assert.ok(chosen.every(item => reference[item.i]! >= c - 2 * THRESHOLDS.l1));
    const gain = chosen.reduce((sum, item, rank) => sum + reference[item.i]! / Math.log2(rank + 2), 0);
    const ideal = wanted.reduce((sum, item, rank) => sum + item.score / Math.log2(rank + 2), 0);
    return { k, effectiveK, gap, overlap, ndcg: gain / ideal, boundaryBand: expected.filter(item => Math.abs(item.score - c) <= 2 * THRESHOLDS.l1).length, local: chosen.map(item => item.id), reference: wanted.map(item => item.id) };
  });
  return { l1, residualBound, topK };
}

/** Qualification admission only: raw online output without pinned completeness
 * proof is unavailable, never a zero-degree or fabricated synthetic capture.
 * This does not implement or enable the resident envelope/recall path. */
export function envelopeRefusal(reply: unknown): string {
  if (!reply || typeof reply !== "object") return "envelope_unavailable";
  if ("error" in reply) return "degree_probe_unavailable";
  return "envelope_provenance_unavailable"; // Current surface has no authoritative policy/generation/coverage capture.
}

export function qualifyFixed20() {
  const oracle = exactReference(FIXED20, SEEDS20, ROLE_WEIGHTS);
  const local = solveFixedCsr(FIXED20, SEEDS20, { roleWeights: ROLE_WEIGHTS });
  const localResidual = oracle.residual(local.values), referenceResidual = oracle.residual(oracle.values);
  assert.ok(Math.abs(localResidual - local.residualL1) <= THRESHOLDS.roundoff);
  assert.ok(local.iterateDeltaL1 < 1e-4 && local.iterations <= 64);
  const comparison = compareScores(FIXED20.nodes, local.values, oracle.values, localResidual, referenceResidual);
  // Mutate one operator input at a time. A mass-preserving wrong answer must
  // fail against the ORIGINAL reference, not a reference rebuilt to match it.
  const controls = [
    { name: "uniform-role-weights", csr: FIXED20, seeds: SEEDS20, weights: {} },
    { name: "changed-seeds", csr: FIXED20, seeds: new Map([[FIXED20.nodes[0]!, 1]]), weights: ROLE_WEIGHTS },
    { name: "redirected-arc", csr: { ...FIXED20, targets: [19, ...FIXED20.targets.slice(1)] }, seeds: SEEDS20, weights: ROLE_WEIGHTS },
    { name: "dangling-row-filled", csr: { ...FIXED20, offsets: FIXED20.offsets.map((offset, i) => offset + Number(i > 0)), targets: [1, ...FIXED20.targets], roles: ["MENTIONS", ...FIXED20.roles] }, seeds: SEEDS20, weights: ROLE_WEIGHTS },
  ];
  const sensitivity = controls.map(control => {
    const changed = solveFixedCsr(control.csr, control.seeds, { roleWeights: control.weights });
    const changedReference = exactReference(control.csr, control.seeds, control.weights);
    compareScores(FIXED20.nodes, changed.values, changedReference.values, changedReference.residual(changed.values), changedReference.residual(changedReference.values));
    const l1 = oracle.values.reduce((sum, value, i) => sum + Math.abs(value - changed.values[i]!), 0);
    assert.ok(l1 > THRESHOLDS.l1, `${control.name} must affect the answer`);
    assert.throws(() => compareScores(FIXED20.nodes, changed.values, oracle.values, oracle.residual(changed.values), referenceResidual));
    return { name: control.name, l1AgainstOriginal: l1, rejectedAgainstOriginal: true, mass: changed.mass, csr: control.csr, seeds: [...control.seeds], roleWeights: control.weights };
  });
  return { qualified: true, scope: "same-operator-synthetic-only", oracle: "dense-partial-pivot-linear-solve-binary64-v1", gdsExecuted: false, sensitivity,
    nodes: FIXED20.nodes.length, arcs: FIXED20.targets.length, csr: FIXED20, seeds: [...SEEDS20], roleWeights: ROLE_WEIGHTS, pins: PIN20, thresholds: THRESHOLDS,
    fixtureSha256: createHash("sha256").update(JSON.stringify({ csr: FIXED20, seeds: [...SEEDS20], weights: ROLE_WEIGHTS, pins: PIN20 })).digest("hex"),
    local: { ...local, values: [...local.values], independentlyMeasuredResidual: localResidual }, reference: { values: oracle.values, mass: oracle.values.reduce((a, b) => a + b, 0), residual: referenceResidual }, ...comparison,
    derivedRecallEnabled: false, extractionEnabled: false };
}
