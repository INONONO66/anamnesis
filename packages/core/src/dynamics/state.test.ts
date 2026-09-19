import { describe, expect, test } from "bun:test";
import { replayDynamics, type DynamicsEvent } from "./state.ts";

describe("dynamics state replay", () => {
  test("replays retention and utility without mutating S on negative outcomes", () => {
    const events: DynamicsEvent[] = [
      { id: "outcome", at: 2, kind: "outcome", reward: -1, weight: 1 },
      { id: "hit", at: 1, kind: "recall_hit", kappa: 1 },
    ];
    const result = replayDynamics({ initialMass: 0.5, ingestedAt: 0, priorRewardSum: 0, priorWeight: 2 }, events);
    expect(result.eventIds).toEqual(["hit", "outcome"]);
    expect(result.stability).toBeGreaterThan(1.5);
    expect(result.utility).toBeCloseTo(-1 / 7);
    expect(result.weight).toBe(3);
  });

  test("rejects duplicate immutable event identities", () => {
    const event = { id: "same", at: 0, kind: "exposure" as const, weight: 1 };
    expect(() => replayDynamics({ initialMass: 0, ingestedAt: 0, priorRewardSum: 0, priorWeight: 0 }, [event, event])).toThrow(RangeError);
  });
});
