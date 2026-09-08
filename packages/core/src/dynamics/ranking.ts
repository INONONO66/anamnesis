export type Result = { id: string; rank: number; sources: string[] };
export const attributeOutcome = (items: Result[], selected: string[]): Map<string, number> => {
  const chosen = items.filter((x) => selected.includes(x.id));
  const denom = chosen.reduce((sum, x) => sum + 1 / (x.rank + 1), 0);
  const out = new Map<string, number>();
  for (const item of chosen) for (const source of item.sources) out.set(source, (out.get(source) ?? 0) + (1 / (item.rank + 1) / denom) / item.sources.length);
  return out;
};
export const utility = (nu: number, mu0: number, sumWr: number, sumW: number): number => (nu * mu0 + sumWr) / (nu + sumW);
const weights: Record<string, number> = { vector: 0.25, bm25: 0.25, ppr: 0.30, session: 0.15, identity: 0.05 };
export const normalizedRrf = (lists: Record<string, { id: string }[]>, id: string, rrfWeights = weights): number => {
  const present = Object.keys(rrfWeights).filter((name) => (lists[name]?.length ?? 0) > 0);
  const total = present.reduce((sum, name) => sum + (rrfWeights[name] ?? 0), 0);
  return present.reduce((sum, name) => { const rank = lists[name]?.findIndex((x) => x.id === id) ?? -1; return sum + (rank >= 0 ? (rrfWeights[name] ?? 0) / total / (60 + rank + 1) : 0); }, 0);
};
