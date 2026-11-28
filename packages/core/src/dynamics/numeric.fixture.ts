// Fixed shared inputs; this module is executed unchanged by Bun and Node.
import { replayDynamics, type DynamicsEvent } from "./state.ts";
export function numericFixture() {
  const states = [];
  for (let gap = 1; gap <= 4096; gap++) {
    states.push(replayDynamics({ initialMass: 0.5, ingestedAt: 0 },
      [{ id: "single", at: gap, credit: 0.2 }]));
  }
  let seed = 0x7351a0cd;
  const random = () => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed; };
  for (let sequence = 0; sequence < 1000; sequence++) {
    const events: DynamicsEvent[] = [];
    for (let index = 0; index < 32; index++) {
      const at = random() % (90 * 86400000), id = String(index).padStart(3, "0");
      events.push({ id, at, credit: 0.2 * ((random() % 3) - 1) / (1 + random() % 16) });
    }
    states.push(replayDynamics({ initialMass: (random() % 101) / 100, ingestedAt: 1000 }, events));
  }
  return states.map(({ stability, lastHit, hitCount }) => ({ stability, lastHit, hitCount }));
}
if (process.argv[1]?.endsWith("numeric.fixture.ts")) console.log(JSON.stringify(numericFixture()));
