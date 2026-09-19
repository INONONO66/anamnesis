import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import type { Arc, FixedCsr } from "../../packages/core/src/dynamics/ppr.ts";
import { ROLE_WEIGHTS } from "./g004-ppr-oracle.ts";

export const GENERATOR = "gds-solver-20-xorshift32-v1";
export const PINS = { policy_revision: 7, extraction_generation: 3, community_generation: 2, T: 1788220800000 };
export const LIMITS = { nodes: 300, inputArcs: 4096, augmentedRelationships: 100000, serializedBytes: 16 * 1024 * 1024, seeds: 128 };
export const sha256 = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
export const ascii = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
const roles = Object.keys(ROLE_WEIGHTS);
export type CapturedArc = Arc & { ordinal: number; generation: number; visibleFrom: number; resolution: string };
export type Edge = { from: string; to: string; weight: number };
export type Fixture = ReturnType<typeof generateFixture>;

/** Fixture truth is constructed without the production exporter or solver. */
export function generateFixture(caseNumber: number) {
  assert.ok(Number.isInteger(caseNumber) && caseNumber >= 1 && caseNumber <= 20);
  const sizes = [1, 2, 2, 12, 12, 20, 16, 64, 64, 40, 64, 64, 64, 64, 300, 300, 128, 128, 64, 257];
  const n = sizes[caseNumber - 1]!, id = `g${String(caseNumber).padStart(2, "0")}`;
  const nodeId = (i: number) => `${id}-n${String(i).padStart(4, "0")}`;
  const originalNodes = Array.from({ length: caseNumber === 19 ? 66 : n }, (_, i) => ({
    id: nodeId(i), kind: caseNumber === 19 && (i === 63 || i === 65) ? "Entity" : "ordinary",
    allowed: !(caseNumber === 19 && i === 64), visibleFrom: PINS.T,
    witness: caseNumber === 19 && (i === 63 || i === 65) ? {
      extraction_generation: 3, policy_revision: 7, earliest_allowed_from: PINS.T + Number(i === 65),
      deniedOldWitness: i === 65,
    } : null,
  }));
  const nodes = originalNodes.slice(0, n).map(node => node.id), rawArcs: CapturedArc[] = [];
  let x = caseNumber >>> 0;
  const next = () => { x ^= x << 13; x ^= x >>> 17; x ^= x << 5; return x >>>= 0; };
  const arc = (from: number, to: number) => {
    const ordinal = rawArcs.length;
    rawArcs.push({ from: nodeId(from), to: nodeId(to), role: roles[ordinal % 5]!, id: `a${String(ordinal).padStart(6, "0")}`,
      ordinal, generation: 3, visibleFrom: PINS.T, resolution: "valid" });
  };
  const ring = (a: number, b: number) => { for (let i = a; i < b; i++) arc(i, a + ((i - a + 1) % (b - a))); };
  const random = (count: number, degree: number) => {
    for (let i = 0; i < count; i++) for (let j = 0; j < degree;) { const target = next() % count; if (target !== i) { arc(i, target); j++; } }
  };
  if (caseNumber === 3) ring(0, 2);
  if (caseNumber === 4) ring(0, 12);
  if (caseNumber === 5) { ring(0, 4); ring(4, 8); ring(8, 12); }
  if (caseNumber === 6) random(n, 3);
  if (caseNumber === 7) { ring(0, n); arc(0, 1); arc(0, 1); arc(0, 2); }
  if (caseNumber === 8) random(n, 7);
  if (caseNumber === 9) random(n, 2);
  if (caseNumber === 15 || caseNumber === 16) {
    for (let i = 1; i < n; i++) arc(i, 0);
    for (let i = 1; i < n; i++) arc(0, i);
  }
  if (caseNumber === 17 || caseNumber === 18) random(n, 5);
  if (caseNumber === 19) random(66, 5);
  if (caseNumber === 20) random(n, 8);
  for (const a of rawArcs) {
    if (caseNumber === 17) a.generation = a.ordinal % 4 === 0 ? 4 : a.ordinal % 4 === 1 ? 2 : 3;
    if (caseNumber === 18 && a.ordinal % 3 === 0) a.visibleFrom++;
    if (caseNumber === 20) a.resolution = ["deleted_physical_link", "wrong_peer", "wrong_role", "mismatched_generation", "valid", "valid", "valid", "valid"][a.ordinal % 8]!;
  }
  const nodeIndex = new Map(originalNodes.map((node, i) => [node.id, i]));
  const exclusions: { id: string; reason: string }[] = [];
  const retained = rawArcs.filter(a => {
    const source = nodeIndex.get(a.from)!, target = nodeIndex.get(a.to)!;
    let reason = "";
    if (caseNumber === 8 && a.ordinal % 7 >= 1 + source % 5) reason = "asymmetric_row_cap";
    if (caseNumber === 9 && [0, 31, 63].includes(source)) reason = "outgoing_deleted";
    if ((caseNumber === 15 || caseNumber === 16) && source === 0 && (caseNumber === 16 || target > 32)) reason = caseNumber === 16 ? "missing_hub_shortlist" : "outside_maintained_shortlist";
    if (caseNumber === 17 && a.generation !== 3) reason = a.generation === 4 ? "hidden_generation" : "retired_generation";
    if (caseNumber === 18 && a.visibleFrom > PINS.T) reason = "future_visible_from";
    if (caseNumber === 19 && (source >= 64 || target >= 64)) reason = source === 64 || target === 64 ? "policy_denied_endpoint" : "future_only_allowed_witness";
    if (caseNumber === 20 && a.resolution !== "valid") reason = a.resolution;
    if (reason) exclusions.push({ id: a.id, reason });
    return !reason;
  });
  const arcs: Arc[] = retained.map(({ from, to, role, id }) => ({ from, to, role, id }));
  const rawSeeds: [string, number][] = caseNumber === 1 || caseNumber === 3 || caseNumber === 5 ? [[nodeId(0), 1]]
    : caseNumber === 2 ? [[nodeId(0), 3], [nodeId(1), 1]]
    : caseNumber === 10 || caseNumber === 11 ? nodes.map((node, i) => [node, 1 + (caseNumber === 11 && i % 2 === 0 ? 1e-6 : 0)])
    : caseNumber >= 12 && caseNumber <= 14 ? nodes.slice(0, [10, 20, 50][caseNumber - 12]!).map(node => [node, 1])
    : [[nodeId(0), 7], [nodeId(Math.floor(n / 2)), 2], [nodeId(n - 1), 1]];
  rawSeeds.sort(([a], [b]) => ascii(a, b));
  const seedTotal = rawSeeds.reduce((sum, [, weight]) => sum + weight, 0);
  const seeds: [string, number][] = rawSeeds.map(([node, weight]) => [node, weight / seedTotal]);
  const expectedRows = nodes.map(node => arcs.filter(a => a.from === node).sort((a, b) => ascii(a.role, b.role) || ascii(a.id, b.id) || ascii(a.to, b.to)));
  const offsets = [0];
  for (const row of expectedRows) offsets.push(offsets.at(-1)! + row.length);
  const expectedCsr: FixedCsr = { nodes, offsets, targets: expectedRows.flatMap(row => row.map(a => nodeIndex.get(a.to)!)), roles: expectedRows.flatMap(row => row.map(a => a.role)) };
  const dangling = nodes.filter((_, i) => expectedRows[i]!.length === 0);
  const capturedRows = originalNodes.map(node => {
    const rows = rawArcs.filter(a => a.from === node.id && !(caseNumber === 9 && [0, 31, 63].map(nodeId).includes(node.id)));
    const captured = rows.slice(0, 256);
    return { source: node.id, physicalDegree: rows.length, count: captured.length, saturated: captured.length === 256,
      raw: captured, shortlist: rows.length >= 256 ? (caseNumber === 16 ? null : rows.slice(0, 32)) : null,
      missingReason: caseNumber === 16 && node.id === nodeId(0) ? "missing_hub_shortlist" : null };
  });
  const physicalArcs = rawArcs.filter(a => a.resolution !== "deleted_physical_link" && !(caseNumber === 9 && [0, 31, 63].map(nodeId).includes(a.from))).map(a => ({ ...a,
    to: a.resolution === "wrong_peer" ? nodeId((nodeIndex.get(a.to)! + 1) % n) : a.to,
    role: a.resolution === "wrong_role" ? roles[(roles.indexOf(a.role) + 1) % 5]! : a.role,
    generation: a.resolution === "mismatched_generation" ? 4 : a.generation,
  }));
  return { id, generator: GENERATOR, seed: caseNumber, pins: PINS, nodes, originalNodes, rawArcs, physicalArcs, capturedRows,
    coverage: { ready: true, synthetic: true, activeGeneration: 3 }, cache: { candidate: "fixed-synthetic", shortlistGeneration: 3, configRevision: 7 },
    nodeExclusions: originalNodes.slice(n).map(node => ({ id: node.id, reason: node.allowed ? "future_only_allowed_witness" : "policy_denied" })),
    exclusions, arcs, retained, expectedRows, expectedCsr, dangling, rawSeeds, seeds, roleWeights: { ...ROLE_WEIGHTS } };
}

export function generateFixtures() {
  const fixtures = Array.from({ length: 20 }, (_, i) => generateFixture(i + 1));
  assert.equal(new Set(fixtures.map(f => sha256(JSON.stringify(f)))).size, 20);
  return fixtures;
}

export function validateInput(csr: FixedCsr, seeds: Map<string, number>, weights: Record<string, number>) {
  const n = csr.nodes.length;
  assert.ok(n > 0 && n <= LIMITS.nodes, "input_invalid: node resource limit / empty channel");
  assert.ok(new Set(csr.nodes).size === n && csr.nodes.every(id => /^g\d{2}-n\d{4}$/.test(id)), "input_invalid: nodes");
  assert.ok(csr.offsets.length === n + 1 && csr.offsets[0] === 0 && csr.offsets.at(-1) === csr.targets.length, "input_invalid: offsets");
  assert.ok(csr.targets.length <= LIMITS.inputArcs && csr.targets.length === csr.roles.length, "input_invalid: arcs");
  assert.ok(csr.offsets.every((v, i) => Number.isSafeInteger(v) && v >= 0 && v <= csr.targets.length && (i === 0 || v >= csr.offsets[i - 1]!)), "input_invalid: offsets");
  assert.ok(csr.targets.every(v => Number.isSafeInteger(v) && v >= 0 && v < n), "input_invalid: targets");
  assert.ok(Object.values(weights).every(w => Number.isFinite(w) && w > 0) && csr.roles.every(role => Object.hasOwn(weights, role)), "input_invalid: role weights");
  assert.ok(seeds.size <= LIMITS.seeds && [...seeds].every(([id, weight]) => csr.nodes.includes(id) && Number.isFinite(weight) && weight >= 0), "input_invalid: seeds");
  const total = [...seeds.values()].reduce((a, b) => a + b, 0);
  assert.ok(Number.isFinite(total) && total > 0, "input_invalid: seed total");
}

export function augmentedEdges(f: Fixture): Edge[] {
  validateInput(f.expectedCsr, new Map(f.rawSeeds), f.roleWeights);
  const edges = f.arcs.map(a => ({ from: a.from, to: a.to, weight: f.roleWeights[a.role as keyof typeof ROLE_WEIGHTS] }));
  for (const from of f.dangling) for (const to of f.nodes) edges.push({ from, to, weight: 1 / f.nodes.length });
  for (const [to, weight] of f.seeds) if (weight > 0) edges.push({ from: "__sigma__", to, weight });
  assert.ok(edges.length <= LIMITS.augmentedRelationships && Buffer.byteLength(JSON.stringify({ f, edges })) <= LIMITS.serializedBytes, "input_invalid: projection resource limit");
  assert.ok(edges.every(e => Number.isFinite(e.weight) && e.weight > 0), "input_invalid: edge weights");
  return edges;
}

export const sortedEdges = (edges: Edge[]) => [...edges].sort((a, b) => ascii(a.from, b.from) || ascii(a.to, b.to) || a.weight - b.weight);
export function checkProjection(expected: Edge[], actual: Edge[]) {
  assert.deepEqual(sortedEdges(actual), sortedEdges(expected), "projection_mismatch: augmented edge multiset");
}

/** Aggregate fixture destinations directly, independently of CSR/kernel helpers. */
export function residual(f: Fixture, values: ArrayLike<number>): number {
  assert.equal(values.length, f.nodes.length);
  const ix = new Map(f.nodes.map((id, i) => [id, i])), seed = new Map(f.seeds);
  const next = f.nodes.map(id => 0.15 * (seed.get(id) ?? 0));
  let danglingMass = 0;
  for (const [i, id] of f.nodes.entries()) {
    const row = f.arcs.filter(a => a.from === id), destinations = new Map<string, number>();
    for (const a of row) destinations.set(a.to, (destinations.get(a.to) ?? 0) + f.roleWeights[a.role as keyof typeof ROLE_WEIGHTS]);
    const total = [...destinations.values()].reduce((a, b) => a + b, 0);
    if (total === 0) danglingMass += values[i]!;
    else for (const [to, weight] of destinations) next[ix.get(to)!]! += 0.85 * values[i]! * weight / total;
  }
  return next.reduce((sum, value, i) => sum + Math.abs(value + 0.85 * danglingMass / f.nodes.length - values[i]!), 0);
}

export function analyticValues(f: Fixture): number[] | null {
  if (f.seed === 1) return [1];
  if (f.seed === 2) return [0.15 * 0.75 + 0.425, 0.15 * 0.25 + 0.425];
  if (f.seed === 3) return [1 / 1.85, 0.85 / 1.85];
  if (f.seed >= 10 && f.seed <= 14) { const s = new Map(f.seeds); return f.nodes.map(id => 0.15 * (s.get(id) ?? 0) + 0.85 / f.nodes.length); }
  return null;
}
