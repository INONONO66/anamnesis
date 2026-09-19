import { expect, test } from "bun:test";
import { createLlmPacer } from "./e2e-real.ts";

test("claim, judge and retry calls share pacing before lease acquisition", async () => {
  let now = 0;
  const waits: number[] = [], calls: number[] = [];
  const pacer = createLlmPacer(3000, () => now, async ms => { waits.push(ms); now += ms; });
  for (const _stage of ["claim", "judge", "retry"]) {
    await pacer.waitForTurn();
    now += 10; // Deterministic lease acquisition, after pacing has completed.
    pacer.markCall(); calls.push(now);
    now += 1000; // Deterministic provider work; no wall-clock sleeping.
  }
  expect(waits).toEqual([2000, 2000]);
  expect(calls).toEqual([10, 3020, 6030]);
});

test("elapsed provider work needs no extra wait and rejected admissions do not mark a call", async () => {
  let now = 0;
  const waits: number[] = [];
  const pacer = createLlmPacer(3000, () => now, async ms => { waits.push(ms); now += ms; });
  await pacer.waitForTurn(); // A failed lease makes no provider call.
  await pacer.waitForTurn(); expect(waits).toEqual([]);
  pacer.markCall(); now += 5000;
  await pacer.waitForTurn(); expect(waits).toEqual([]);
});

test("pacing configuration is finite, integral and bounded", () => {
  for (const value of [-1, 0.5, Infinity, NaN, 60001]) expect(() => createLlmPacer(value)).toThrow("invalid_llm_min_interval");
  expect(() => createLlmPacer(0)).not.toThrow();
  expect(() => createLlmPacer(60000)).not.toThrow();
});
