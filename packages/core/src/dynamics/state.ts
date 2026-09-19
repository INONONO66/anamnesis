import { adopt, initialStability, type State as RetentionState } from "./retention.ts";
import { utility } from "./ranking.ts";

export type DynamicsEvent =
  | { id: string; at: number; kind: "recall_hit"; kappa: number }
  | { id: string; at: number; kind: "outcome"; reward: number; weight: number }
  | { id: string; at: number; kind: "exposure" | "re_mention" | "promotion"; weight?: number };

export type DynamicsInput = {
  initialMass: number;
  ingestedAt: number;
  priorRewardSum: number;
  priorWeight: number;
};

export type DynamicsState = RetentionState & {
  utility: number;
  weight: number;
  eventIds: string[];
};

const finite = (value: number, name: string): void => {
  if (!Number.isFinite(value)) throw new RangeError(`${name} must be finite`);
};

/** Deterministically folds immutable events into S and U. Events are copied,
 * sorted, and never modified; only recall_hit can change retention S. */
export const replayDynamics = (input: DynamicsInput, events: readonly DynamicsEvent[]): DynamicsState => {
  finite(input.initialMass, "initialMass");
  finite(input.ingestedAt, "ingestedAt");
  finite(input.priorRewardSum, "priorRewardSum");
  finite(input.priorWeight, "priorWeight");
  if (input.initialMass < 0 || input.initialMass > 1 || input.priorWeight < 0) throw new RangeError("invalid dynamics input");
  const ordered = [...events].sort((a, b) => {
    const time = a.at - b.at;
    return time !== 0 ? time : a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  });
  const ids = new Set<string>();
  let state: RetentionState = { stability: initialStability(input.initialMass), lastHit: input.ingestedAt, hitCount: 0 };
  let sumWeight = input.priorWeight;
  let sumReward = input.priorRewardSum;
  for (const event of ordered) {
    if (typeof event.id !== "string" || event.id.length === 0 || ids.has(event.id) || !Number.isFinite(event.at)) throw new RangeError("invalid immutable event identity");
    ids.add(event.id);
    if (event.kind === "recall_hit") {
      state = adopt(state, event.at, event.kappa);
    } else if (event.kind === "outcome") {
      finite(event.reward, "reward"); finite(event.weight, "weight");
      if (event.weight < 0) throw new RangeError("weight must be non-negative");
      sumReward += event.reward * event.weight;
      sumWeight += event.weight;
    }
    state = { ...state, hitCount: state.hitCount + 1 };
  }
  return { ...state, utility: utility(4, 0, sumReward, sumWeight), weight: sumWeight, eventIds: ordered.map((event) => event.id) };
};
