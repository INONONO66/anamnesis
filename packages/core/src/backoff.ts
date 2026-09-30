/** Exponential backoff with a cap: base * 2^(n-1) for the n-th retry (n >= 1), 0 for n <= 0. Deterministic;
 * no jitter, because both callers are single lanes that never contend for the same resource. */
export function backoffMs(retry: number, baseMs: number, capMs: number): number {
  if (!Number.isFinite(retry) || retry <= 0) return 0;
  return Math.min(capMs, baseMs * 2 ** Math.min(retry - 1, 30));
}
/** A failed ModelTask on attempt n retries after 1s, 2s, 4s ... capped at 30s. */
export const modelTaskRetryDelayMs = (attempts: number): number => backoffMs(attempts, 1_000, 30_000);
/** A crashed managed daemon restarts after 1s, 2s, 4s (three restarts, then the budget is exhausted). */
export const managedRestartDelayMs = (restarts: number): number => backoffMs(restarts, 1_000, 30_000);
