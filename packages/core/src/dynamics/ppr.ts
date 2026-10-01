export type Arc = { from: string; to: string; role: string; id: string; weight?: number };
export type PprInput = { nodes: string[]; arcs: Arc[]; seeds: Map<string, number>; alpha?: number; tolerance?: number; maxIter?: number; roleWeights?: Record<string, number> };
export type PprResult = { values: Float64Array<ArrayBufferLike>; rowSums: number[]; mass: number; iterations: number; iterateDeltaL1: number; residualL1: number; errorBoundL1: number };
export type FixedCsr = { nodes: string[]; offsets: number[]; targets: number[]; roles: string[] };

/** Deterministic graph snapshot used by both production solving and external
 * qualification. No native adjacency or caller-order dependence is allowed. */
export function exportFixedCsr(input: Pick<PprInput, "nodes" | "arcs">): FixedCsr {
  const nodes = [...input.nodes].sort();
  if (new Set(nodes).size !== nodes.length || nodes.some(node => typeof node !== "string")) throw new RangeError("invalid CSR nodes");
  const ix = new Map(nodes.map((node, i) => [node, i]));
  const arcs = [...input.arcs].sort(arcOrder);
  const offsets: number[] = Array(nodes.length + 1).fill(0), targets: number[] = [], roles: string[] = [];
  for (const arc of arcs) {
    const from = ix.get(arc.from), to = ix.get(arc.to);
    if (from === undefined || to === undefined || typeof arc.role !== "string" || !arc.role || typeof arc.id !== "string" || !arc.id || arc.weight !== undefined) throw new RangeError("invalid CSR arc");
    targets.push(to); roles.push(arc.role); offsets[from + 1]!++;
  }
  for (let i = 1; i < offsets.length; i++) offsets[i]! += offsets[i - 1]!;
  return { nodes, offsets, targets, roles };
}

export function solveFixedCsr(csr: FixedCsr, seeds: Map<string, number>, options: Omit<PprInput, "nodes" | "arcs" | "seeds"> = {}): PprResult {
  if (csr.offsets.length !== csr.nodes.length + 1 || csr.targets.length !== csr.roles.length || csr.offsets[0] !== 0 || csr.offsets.at(-1) !== csr.targets.length) throw new RangeError("invalid CSR");
  if (csr.offsets.some((offset, i) => !Number.isSafeInteger(offset) || offset < 0 || offset > csr.targets.length || (i > 0 && offset < csr.offsets[i - 1]!)) ||
      csr.targets.some(target => !Number.isSafeInteger(target) || target < 0 || target >= csr.nodes.length) ||
      csr.roles.some(role => typeof role !== "string" || !role)) throw new RangeError("invalid CSR");
  const arcs = csr.nodes.flatMap((from, i) => csr.targets.slice(csr.offsets[i]!, csr.offsets[i + 1]!).map((to, j) => ({ from, to: csr.nodes[to]!, role: csr.roles[csr.offsets[i]! + j]!, id: String(j).padStart(16, "0") })));
  return solvePpr({ ...options, nodes: csr.nodes, arcs, seeds });
}
type Link = { to: number; weight: number };
type Vector = Float64Array<ArrayBufferLike>;
/** Deterministic arc order: from, role, id, to. Shared by the CSR export and the solver so both see one adjacency. */
const ARC_KEYS = ["from", "role", "id", "to"] as const;
const arcOrder = (x: Arc, y: Arc): number => {
  for (const key of ARC_KEYS) { if (x[key] < y[key]) return -1; if (x[key] > y[key]) return 1; }
  return 0;
};
const positiveFinite = (value: number): boolean => value > 0 && Number.isFinite(value);

function indexNodes(input: PprInput): Map<string, number> {
  if (!input || !Array.isArray(input.nodes) || !Array.isArray(input.arcs) || !(input.seeds instanceof Map)) throw new RangeError("malformed input");
  if (input.nodes.length === 0) throw new RangeError("empty graph");
  if (input.nodes.some((node) => typeof node !== "string")) throw new RangeError("invalid node");
  const nodes = [...input.nodes].sort();
  if (new Set(nodes).size !== nodes.length) throw new RangeError("duplicate node");
  return new Map(nodes.map((node, i) => [node, i]));
}
function buildRows(input: PprInput, ix: Map<string, number>): Link[][] {
  const roleWeights = input.roleWeights ?? {};
  for (const weight of Object.values(roleWeights)) if (!positiveFinite(weight)) throw new RangeError("invalid role weight");
  const rows = Array.from({ length: ix.size }, () => [] as Link[]);
  if (input.arcs.some((arc) => !arc || typeof arc.from !== "string" || typeof arc.to !== "string" || typeof arc.role !== "string" || typeof arc.id !== "string")) throw new RangeError("malformed arc");
  for (const arc of [...input.arcs].sort(arcOrder)) {
    const from = ix.get(arc.from), to = ix.get(arc.to);
    if (from === undefined || to === undefined) throw new RangeError("invalid arc");
    if (arc.weight !== undefined) throw new RangeError("per-link weight is not supported");
    const weight = roleWeights[arc.role] ?? 1;
    if (!positiveFinite(weight)) throw new RangeError("invalid role weight");
    rows[from]!.push({ to, weight });
  }
  return rows;
}
const rowSum = (row: Link[]): number => { let sum = 0; for (const arc of row) { sum += arc.weight; if (!Number.isFinite(sum)) throw new RangeError("row weight overflow"); } return sum; };
function normalizedSeeds(seeds: Map<string, number>, ix: Map<string, number>): Vector {
  const seed = new Float64Array(ix.size); let seedSum = 0;
  for (const [id, value] of [...seeds].sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)) {
    const i = ix.get(id);
    if (i === undefined || !Number.isFinite(value) || value < 0) throw new RangeError("invalid seed");
    seed[i]! += value; seedSum += value;
  }
  if (!positiveFinite(seedSum)) throw new RangeError("empty seeds");
  for (let i = 0; i < seed.length; i++) seed[i]! /= seedSum;
  return seed;
}
function solveOptions(input: PprInput): { alpha: number; tolerance: number; maxIter: number } {
  const alpha = input.alpha ?? 0.85, tolerance = input.tolerance ?? 1e-4, maxIter = input.maxIter ?? 64;
  if (!(alpha >= 0 && alpha < 1) || !Number.isFinite(alpha)) throw new RangeError("invalid alpha");
  if (!positiveFinite(tolerance)) throw new RangeError("invalid tolerance");
  if (!Number.isSafeInteger(maxIter) || maxIter < 1) throw new RangeError("invalid maxIter");
  return { alpha, tolerance, maxIter };
}
export const solvePpr = (input: PprInput): PprResult => {
  const ix = indexNodes(input), n = ix.size;
  const rows = buildRows(input, ix);
  const rowSums = rows.map(rowSum);
  const seed = normalizedSeeds(input.seeds, ix);
  const { alpha, tolerance, maxIter } = solveOptions(input);
  const apply = (p: Vector): Vector => { const next = new Float64Array(n); let dangling = 0; for (let i = 0; i < n; i++) { const current = p[i]!, sum = rowSums[i]!; if (sum === 0) dangling += current; else for (const arc of rows[i]!) next[arc.to]! += current * (arc.weight / sum); } for (let i = 0; i < n; i++) next[i]! = (1 - alpha) * seed[i]! + alpha * (next[i]! + dangling / n); return next; };
  let p: Vector = seed; let delta = Infinity; let iterations = 0;
  while (iterations < maxIter && delta >= tolerance) { const next = apply(p); delta = 0; for (let i = 0; i < n; i++) delta += Math.abs(next[i]! - p[i]!); p = next; iterations++; }
  const fixed = apply(p); let residual = 0, mass = 0; for (let i = 0; i < n; i++) { residual += Math.abs(fixed[i]! - p[i]!); mass += p[i]!; }
  return { values: p, rowSums, mass, iterations, iterateDeltaL1: delta, residualL1: residual, errorBoundL1: alpha / (1 - alpha) * delta };
};
