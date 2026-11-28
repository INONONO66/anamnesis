import { adopt, initialStability } from "./retention.ts";

export type DynamicsEvent = { id: string; at: number; credit: number };
export const decayedUtility = (anchor: number, at: number, now: number): number => anchor * 2 ** (-Math.max(0, now - at) / (30 * 86_400_000));

export function replayDynamics(input: { initialMass: number; ingestedAt: number }, events: readonly DynamicsEvent[]) {
  const floor = initialStability(input.initialMass);
  let state = { stability: floor, lastHit: input.ingestedAt, hitCount: 0 };
  let utility = 0, at = input.ingestedAt;
  for (const event of [...events].sort((a, b) => a.at - b.at || (a.id < b.id ? -1 : 1))) {
    utility = Math.max(-1, Math.min(1, decayedUtility(utility, at, event.at) + event.credit));
    at = event.at;
    if (event.credit !== 0) state = adopt(state, at, Math.max(-1, Math.min(1, event.credit / 0.2)), floor);
    state.hitCount++;
  }
  return { ...state, utility, at };
}
