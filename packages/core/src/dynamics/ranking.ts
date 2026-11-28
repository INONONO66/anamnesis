export const attributeOutcome = (items: readonly { sources: readonly string[] }[]): Map<string, number> => {
  const denom = items.reduce((sum, _, j) => sum + 1 / (j + 1), 0);
  const weights = new Map<string, number>();
  for (const [j, item] of items.entries()) for (const source of item.sources)
    weights.set(source, Math.min(1, (weights.get(source) ?? 0) + 1 / (j + 1) / denom / item.sources.length));
  return weights;
};
const weights: Record<string, number> = { vector: 0.25, bm25: 0.25, ppr: 0.30, session: 0.15, identity: 0.05 };
export const normalizedRrf = (lists: Record<string, { id: string }[]>, id: string, rrfWeights = weights): number => {
  const present = Object.keys(rrfWeights).filter((name) => (lists[name]?.length ?? 0) > 0);
  const total = present.reduce((sum, name) => sum + (rrfWeights[name] ?? 0), 0);
  return present.reduce((sum, name) => { const rank = lists[name]?.findIndex((x) => x.id === id) ?? -1; return sum + (rank >= 0 ? (rrfWeights[name] ?? 0) / total / (60 + rank + 1) : 0); }, 0);
};
