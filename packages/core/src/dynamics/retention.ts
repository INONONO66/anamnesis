import { adoptionGain } from "./adoption-numeric.ts";

export const retention = (elapsedDays: number, stability: number): number => {
  if (!Number.isFinite(elapsedDays) || elapsedDays < 0 || !Number.isFinite(stability) || stability <= 0) throw new RangeError("invalid retention input");
  return Math.pow(1 + (19 / 81) * elapsedDays / stability, -0.5);
};
export const initialStability = (m0: number): number => {
  if (!Number.isFinite(m0) || m0 < 0 || m0 > 1) throw new RangeError("m0 must be in [0,1]");
  return 1 + m0;
};
export type State = { stability: number; lastHit: number; hitCount: number };
const DAY = 86_400_000;
export const adopt = (state: State, at: number, kappa: number, floor: number): State => {
  if (!Number.isFinite(at) || !Number.isFinite(kappa) || kappa < -1 || kappa > 1) throw new RangeError("invalid adoption");
  const gap = Math.max(0, at - state.lastHit) / DAY;
  const gain = adoptionGain(state.stability, gap, kappa);
  return { stability: Math.max(floor, Math.min(3650, state.stability + gain)), lastHit: Math.max(state.lastHit, at), hitCount: state.hitCount };
};
