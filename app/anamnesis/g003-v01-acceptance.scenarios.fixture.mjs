// Direct-executable adapter for the existing publication scenario bodies.
// No node:test import/run: registration is synchronous and execution is serial.
import assert from 'node:assert/strict';
const scenarios = [];
export default function scenario(name, { timeout }, run) {
  scenarios.push({ name, timeout, run });
}
export async function runScenarios() {
  assert.equal(scenarios.length, 8, 'all publication scenarios must be registered');
  for (const { name, timeout, run } of scenarios) {
    let timer;
    try {
      await Promise.race([
        run(),
        new Promise((_, reject) => { timer = setTimeout(() => reject(Error(`publication scenario deadline: ${name}`)), timeout); }),
      ]);
      // Emitted only after the real assertions and the scenario's cleanup finish.
      console.log(JSON.stringify({ checkpoint: 'publication-scenario-pass', name }));
    } finally { clearTimeout(timer); }
  }
  console.log(JSON.stringify({ checkpoint: 'publication-scenarios-complete', passed: scenarios.length, failed: 0, skipped: 0, concurrency: 1 }));
}
