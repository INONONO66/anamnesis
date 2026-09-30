import { describe, expect, test } from "bun:test";
import { backoffMs, managedRestartDelayMs, modelTaskRetryDelayMs } from "./backoff.ts";

describe("backoffMs", () => {
  test("zero for the first attempt and any non-positive or non-finite retry count", () => {
    for (const n of [0, -1, Number.NaN, Number.NEGATIVE_INFINITY]) expect(backoffMs(n, 100, 1000)).toBe(0);
  });
  test("doubles from the base and stops at the cap", () => {
    expect([1, 2, 3, 4, 5].map(n => backoffMs(n, 100, 1000))).toEqual([100, 200, 400, 800, 1000]);
    expect(backoffMs(1_000, 100, 1000)).toBe(1000);
  });
  test("the callers' schedules", () => {
    expect([1, 2, 3, 6].map(modelTaskRetryDelayMs)).toEqual([1_000, 2_000, 4_000, 30_000]);
    expect([1, 2, 3].map(managedRestartDelayMs)).toEqual([1_000, 2_000, 4_000]);
  });
});
