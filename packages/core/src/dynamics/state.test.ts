import { expect, test } from "bun:test";
import { decayedUtility, replayDynamics } from "./state.ts";

const day = 86_400_000, initial = { initialMass: 0.5, ingestedAt: 0 };
test("utility clips at each ordered event and halves between reports and at read", () => {
  const events = Array.from({ length: 6 }, (_, i) => ({ id: String(i), at: day, credit: 0.2 }));
  events.push({ id: "6", at: day, credit: -0.2 }, { id: "7", at: 31 * day, credit: -0.1 });
  const state = replayDynamics(initial, events.reverse());
  expect(state.hitCount).toBe(8);
  expect(state.at).toBe(31 * day);
  expect(state.utility).toBeCloseTo(0.3, 14);
  expect(decayedUtility(state.utility, state.at, 61 * day)).toBeCloseTo(0.15, 14);
});
test("signed testing effect raises and lowers S without crossing S0; zero credit leaves the clock alone", () => {
  const positive = { id: "1", at: day, credit: 0.2 };
  const gain = replayDynamics(initial, [positive]);
  expect(gain.stability).toBeGreaterThan(1.5);
  const loss = replayDynamics(initial, [positive, { id: "2", at: 31 * day, credit: -0.2 }]);
  expect(loss.stability).toBe(1.5);
  expect(replayDynamics(initial, [{ ...positive, credit: 0 }]).lastHit).toBe(0);
});
