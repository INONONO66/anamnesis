import { describe, expect, test } from "bun:test";
import { RECOVERY_PROBE_MS, RecoveryProbe, nodeTimers, type RecoveryTimers } from "./recovery-probe.ts";

function fakeTimers() {
  const pending = new Map<number, { callback: () => void; ms: number }>();
  let next = 1;
  const timers: RecoveryTimers<number> = {
    set(callback, ms) { pending.set(next, { callback, ms }); return next++; },
    clear(handle) { pending.delete(handle); },
  };
  return { timers, pending, fire() { for (const [id, { callback }] of [...pending]) { pending.delete(id); callback(); } } };
}

describe("RecoveryProbe", () => {
  test("arm schedules exactly one tick at the configured interval", () => {
    const t = fakeTimers(); let ticks = 0;
    const probe = new RecoveryProbe(() => { ticks++; }, 250, t.timers);
    probe.arm(); probe.arm();
    expect(t.pending.size).toBe(1);
    expect([...t.pending.values()][0]!.ms).toBe(250);
    expect(probe.armed).toBe(true);
    t.fire();
    expect(ticks).toBe(1);
    expect(probe.armed).toBe(false);
  });
  test("a tick may re-arm; disarm cancels a pending tick", () => {
    const t = fakeTimers(); let ticks = 0;
    const probe: RecoveryProbe<number> = new RecoveryProbe(() => { ticks++; probe.arm(); }, 10, t.timers);
    probe.arm(); t.fire(); expect(ticks).toBe(1); expect(probe.armed).toBe(true);
    probe.disarm(); expect(probe.armed).toBe(false); expect(t.pending.size).toBe(0);
    probe.disarm();
    t.fire(); expect(ticks).toBe(1);
  });
  test("stop is final: nothing fires and arm becomes a no-op", () => {
    const t = fakeTimers(); let ticks = 0;
    const probe = new RecoveryProbe(() => { ticks++; }, 10, t.timers);
    probe.arm(); probe.stop(); probe.arm();
    expect(probe.armed).toBe(false); expect(t.pending.size).toBe(0);
    t.fire(); expect(ticks).toBe(0);
  });
  test("default timers use an unref'd node timeout at the shipped interval", async () => {
    const probe = new RecoveryProbe(() => {}, RECOVERY_PROBE_MS, nodeTimers);
    expect(RECOVERY_PROBE_MS).toBe(5000);
    probe.arm(); expect(probe.armed).toBe(true); probe.disarm(); expect(probe.armed).toBe(false);
  });
});
