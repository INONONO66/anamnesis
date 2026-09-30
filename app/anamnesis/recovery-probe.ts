/** Re-arms one bounded timer while storage is unavailable. Each tick hands
 * control back to the caller's serial lane, which decides whether recovery
 * succeeded; the probe never touches storage itself. */
export interface RecoveryTimers<Handle> {
  set(callback: () => void, ms: number): Handle;
  clear(handle: Handle): void;
}
export const RECOVERY_PROBE_MS = 5000;
export const nodeTimers: RecoveryTimers<ReturnType<typeof setTimeout>> = {
  set(callback, ms) { const handle = setTimeout(callback, ms); handle.unref(); return handle; },
  clear(handle) { clearTimeout(handle); },
};
export class RecoveryProbe<Handle> {
  private handle: Handle | undefined;
  private stopped = false;
  constructor(private readonly tick: () => void, private readonly intervalMs: number, private readonly timers: RecoveryTimers<Handle>) {}
  /** Idempotent while a tick is pending; a no-op after stop(). */
  arm(): void {
    if (this.stopped || this.handle !== undefined) return;
    this.handle = this.timers.set(() => { this.handle = undefined; this.tick(); }, this.intervalMs);
  }
  disarm(): void {
    if (this.handle === undefined) return;
    this.timers.clear(this.handle); this.handle = undefined;
  }
  /** Final: no tick fires after stop, even one already armed. */
  stop(): void { this.disarm(); this.stopped = true; }
  get armed(): boolean { return this.handle !== undefined; }
}
