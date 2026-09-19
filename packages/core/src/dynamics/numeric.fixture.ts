// Fixed shared inputs; this module is executed unchanged by Bun and Node.
import { replayDynamics, type DynamicsEvent } from "./state.ts";
export function numericFixture() {
  const states = [];
  for (let gap = 1; gap <= 4096; gap++) {
    states.push(replayDynamics({ initialMass: 0.5, ingestedAt: 0, priorRewardSum: 0, priorWeight: 0 },
      [{ id: "single", at: gap, kind: "recall_hit", kappa: 1 }]));
  }
  let seed = 0x7351a0cd;
  const random = () => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed; };
  for (let sequence = 0; sequence < 1000; sequence++) {
    const events: DynamicsEvent[] = [];
    for (let index = 0; index < 32; index++) {
      const at = random() % (90 * 86400000), id = String(index).padStart(3, "0");
      switch (index % 4) {
        case 0: events.push({ id, at, kind: "recall_hit", kappa: 1 / (1 + random() % 16) }); break;
        case 1: events.push({ id, at, kind: "outcome", reward: (random() % 3) - 1, weight: 1 / (1 + random() % 16) }); break;
        case 2: events.push({ id, at, kind: "exposure" }); break;
        default: events.push({ id, at, kind: "promotion" });
      }
    }
    states.push(replayDynamics({ initialMass: (random() % 101) / 100, ingestedAt: 1000, priorRewardSum: 0, priorWeight: 0 }, events));
  }
  return states;
}
if (process.argv[1]?.endsWith("numeric.fixture.ts")) console.log(JSON.stringify(numericFixture()));
