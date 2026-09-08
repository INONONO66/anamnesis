export const retention = (elapsedDays: number, stability: number): number => {
  if (!Number.isFinite(elapsedDays) || elapsedDays < 0 || !Number.isFinite(stability) || stability <= 0) throw new RangeError("invalid retention input");
  return Math.pow(1 + (19 / 81) * elapsedDays / stability, -0.5);
};
export const initialStability = (m0: number): number => {
  if (!Number.isFinite(m0) || m0 < 0 || m0 > 1) throw new RangeError("m0 must be in [0,1]");
  return 1 + m0;
};
export type Hit = { id: string; at: number; kind: "recall_hit" | "outcome" | "exposure" | "re_mention" | "promotion"; kappa: number };
export type State = { stability: number; lastHit: number; hitCount: number };
const DAY = 86_400_000;
export const adopt = (state: State, at: number, kappa: number): State => {
  if (!Number.isFinite(at) || !Number.isFinite(kappa) || kappa < 0 || kappa > 1) throw new RangeError("invalid adoption");
  const gap = Math.max(0, at - state.lastHit) / DAY;
  const r = retention(gap, state.stability);
  const gain = state.stability * 5 * kappa * (Math.exp(1 - r) - 1) * Math.pow(state.stability, -0.1);
  return { stability: Math.min(3650, state.stability + gain), lastHit: Math.max(state.lastHit, at), hitCount: state.hitCount };
};
export const replay = (m0: number, ingestedAt: number, hits: Hit[]): State => {
  let state: State = { stability: initialStability(m0), lastHit: ingestedAt, hitCount: 0 };
  for (const hit of [...hits].sort((a, b) => a.at - b.at || a.id.localeCompare(b.id))) {
    state = hit.kind === "recall_hit" ? adopt(state, hit.at, hit.kappa) : state;
    state = { ...state, hitCount: state.hitCount + 1 };
  }
  return state;
};
