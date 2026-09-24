// Paced extraction provider: a decorator that meters every provider call so the
// daemon's model traffic blends into a shared token budget instead of bursting.
//
// All `extract()` calls (claims, judges, relation judges and their retries)
// pass through one FIFO gate. A call may start no earlier than
// `lastStart + minIntervalMs * (1 + jitterFraction * random())`, where
// `lastStart` is when the previous call was released. Only start times are
// spaced: once released, the inner call runs concurrently with earlier ones,
// so the scheduler's in-flight bound still governs overlap. Waiters leave the
// gate strictly in arrival order; a rejected inner call never jams the gate.
import type { ExtractionProvider, ExtractionProviderInput } from "./extraction.ts";

export interface PacedExtractionProviderOptions {
  /** Minimum spacing between call starts, integer 0..600000. Zero passes calls through immediately. */
  minIntervalMs: number;
  /** Stretches the spacing by up to this fraction (0..1) using `random`; default 0 (no jitter). */
  jitterFraction?: number;
  /** Monotonic-enough millisecond clock; the daemon uses Date.now so `last_call_at` is an epoch. */
  clock?: () => number;
  /** Resolves once `ms` have passed; the daemon uses setTimeout, tests hand-release it. */
  wait?: (ms: number) => Promise<void>;
  /** Uniform draw in [0, 1); default Math.random. */
  random?: () => number;
}
export interface PacedExtractionProviderStats {
  calls_total: number;
  /** Time calls spent parked in the gate (queue plus spacing), summed over the process lifetime. */
  waited_total_ms: number;
  /** Clock reading when the most recent call was released, or null before the first call. */
  last_call_at: number | null;
}

const MAX_INTERVAL_MS = 600000;
const realWait = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));

export class PacedExtractionProvider implements ExtractionProvider {
  readonly model: string;
  readonly modelIncarnation: string;
  private readonly minIntervalMs: number;
  private readonly jitterFraction: number;
  private readonly clock: () => number;
  private readonly wait: (ms: number) => Promise<void>;
  private readonly random: () => number;
  /** Settles when the most recently admitted call has been released; the next call chains behind it. */
  private gate: Promise<void> = Promise.resolve();
  private lastStart: number | null = null;
  private callsTotal = 0;
  private waitedTotalMs = 0;

  constructor(private readonly inner: ExtractionProvider, options: PacedExtractionProviderOptions) {
    const { minIntervalMs, jitterFraction = 0 } = options;
    if (!Number.isSafeInteger(minIntervalMs) || minIntervalMs < 0 || minIntervalMs > MAX_INTERVAL_MS) {
      throw new RangeError(`minIntervalMs must be an integer between 0 and ${MAX_INTERVAL_MS}`);
    }
    if (!Number.isFinite(jitterFraction) || jitterFraction < 0 || jitterFraction > 1) throw new RangeError("jitterFraction must be between 0 and 1");
    this.minIntervalMs = minIntervalMs; this.jitterFraction = jitterFraction;
    this.clock = options.clock ?? Date.now; this.wait = options.wait ?? realWait; this.random = options.random ?? Math.random;
    this.model = inner.model; this.modelIncarnation = inner.modelIncarnation;
  }

  async extract(input: ExtractionProviderInput): Promise<unknown> {
    await this.turn();
    return this.inner.extract(input);
  }

  stats(): PacedExtractionProviderStats {
    return { calls_total: this.callsTotal, waited_total_ms: Math.round(this.waitedTotalMs), last_call_at: this.lastStart };
  }

  /** Resolves once this call may start. Arrival order is release order: each call waits for its predecessor's release
   * before measuring its own spacing, so a jittered gap always separates two consecutive starts. */
  private turn(): Promise<void> {
    const enqueuedAt = this.clock();
    const predecessor = this.gate;
    let release!: () => void;
    this.gate = new Promise<void>(resolve => { release = resolve; });
    return predecessor.then(async () => {
      try {
        if (this.lastStart !== null) {
          const spacing = this.minIntervalMs * (1 + this.jitterFraction * this.random());
          const delay = this.lastStart + spacing - this.clock();
          if (delay > 0) await this.wait(delay);
        }
        const startedAt = this.clock();
        this.waitedTotalMs += Math.max(0, startedAt - enqueuedAt);
        this.lastStart = startedAt; this.callsTotal++;
      } finally { release(); }
    });
  }
}
