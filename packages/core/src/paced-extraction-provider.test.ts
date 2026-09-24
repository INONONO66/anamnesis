// The gate is exercised with a hand-cranked clock and hand-released waits: every
// assertion follows an observed event (a wait request, an inner call, a
// settlement), never a real timer.
import { expect, test } from "bun:test";
import type { ExtractionProvider, ExtractionProviderInput } from "./extraction.ts";
import { PacedExtractionProvider } from "./paced-extraction-provider.ts";

interface WaitRequest { ms: number; release: () => void }
/** Fake time: `wait` parks a request and announces it to whoever subscribed through `nextWait` beforehand. */
function fakeTime() {
  let now = 0;
  const requests: WaitRequest[] = [];
  let subscribers: Array<(request: WaitRequest) => void> = [];
  return {
    clock: () => now,
    advance(ms: number) { now += ms; },
    requests,
    wait(ms: number): Promise<void> {
      return new Promise<void>(release => {
        const request = { ms, release };
        requests.push(request);
        const waiting = subscribers; subscribers = [];
        for (const notify of waiting) notify(request);
      });
    },
    nextWait(): Promise<WaitRequest> { return new Promise(resolve => subscribers.push(resolve)); },
  };
}
const neverWait = (): Promise<void> => Promise.reject(new Error("unexpected wait"));
/** Inner provider that records inputs and settles in call order; `fail` makes a specific text reject. */
function recordingProvider(fail?: string) {
  const calls: string[] = [];
  const provider: ExtractionProvider = {
    model: "haiku-fixture", modelIncarnation: "a".repeat(64),
    async extract(input: ExtractionProviderInput) {
      calls.push(input.text);
      if (input.text === fail) throw new Error(`provider_rejected:${input.text}`);
      return { echoed: input.text };
    },
  };
  return { provider, calls };
}
const claim = (text: string): ExtractionProviderInput => ({ text, task: "claim" });

test("identity passes through and options are validated at construction", () => {
  const { provider } = recordingProvider();
  const paced = new PacedExtractionProvider(provider, { minIntervalMs: 250, jitterFraction: 0.5 });
  expect(paced.model).toBe("haiku-fixture");
  expect(paced.modelIncarnation).toBe("a".repeat(64));
  expect(paced.stats()).toEqual({ calls_total: 0, waited_total_ms: 0, last_call_at: null });
  for (const minIntervalMs of [-1, 0.5, 600001, Number.NaN, Number.POSITIVE_INFINITY]) {
    expect(() => new PacedExtractionProvider(provider, { minIntervalMs })).toThrow(RangeError);
  }
  for (const jitterFraction of [-0.01, 1.01, Number.NaN]) {
    expect(() => new PacedExtractionProvider(provider, { minIntervalMs: 0, jitterFraction })).toThrow(RangeError);
  }
  expect(() => new PacedExtractionProvider(provider, { minIntervalMs: 600000, jitterFraction: 1 })).not.toThrow();
});

test("interval 0 passes every call through immediately without waiting", async () => {
  const time = fakeTime();
  const { provider, calls } = recordingProvider();
  const paced = new PacedExtractionProvider(provider, { minIntervalMs: 0, clock: time.clock, wait: neverWait, random: () => 1 });
  const results = await Promise.all([paced.extract(claim("a")), paced.extract(claim("b")), paced.extract(claim("c"))]);
  expect(results).toEqual([{ echoed: "a" }, { echoed: "b" }, { echoed: "c" }]);
  expect(calls).toEqual(["a", "b", "c"]);
  expect(paced.stats()).toEqual({ calls_total: 3, waited_total_ms: 0, last_call_at: 0 });
});

test("a second back-to-back call waits exactly the minimum interval when random() is 0", async () => {
  const time = fakeTime();
  const { provider, calls } = recordingProvider();
  const paced = new PacedExtractionProvider(provider, { minIntervalMs: 1000, jitterFraction: 0.5, clock: time.clock, wait: time.wait, random: () => 0 });
  const first = paced.extract(claim("a"));
  const parked = time.nextWait();
  const second = paced.extract(claim("b"));
  expect(await first).toEqual({ echoed: "a" });
  const request = await parked;
  expect(request.ms).toBe(1000);
  expect(calls).toEqual(["a"]);
  expect(paced.stats()).toEqual({ calls_total: 1, waited_total_ms: 0, last_call_at: 0 });
  time.advance(1000); request.release();
  expect(await second).toEqual({ echoed: "b" });
  expect(calls).toEqual(["a", "b"]);
  expect(time.requests).toHaveLength(1);
  expect(paced.stats()).toEqual({ calls_total: 2, waited_total_ms: 1000, last_call_at: 1000 });
});

test("jitter stretches the spacing by jitterFraction when random() is 1", async () => {
  const time = fakeTime();
  const { provider } = recordingProvider();
  const paced = new PacedExtractionProvider(provider, { minIntervalMs: 1000, jitterFraction: 0.5, clock: time.clock, wait: time.wait, random: () => 1 });
  await paced.extract(claim("a"));
  const parked = time.nextWait();
  const second = paced.extract(claim("b"));
  const request = await parked;
  expect(request.ms).toBe(1500);
  time.advance(1500); request.release();
  await second;
  expect(paced.stats()).toEqual({ calls_total: 2, waited_total_ms: 1500, last_call_at: 1500 });
});

test("elapsed time since the last start is credited against the spacing", async () => {
  const time = fakeTime();
  const { provider } = recordingProvider();
  const paced = new PacedExtractionProvider(provider, { minIntervalMs: 1000, clock: time.clock, wait: time.wait, random: () => 0 });
  await paced.extract(claim("a"));
  time.advance(400);
  const parked = time.nextWait();
  const second = paced.extract(claim("b"));
  expect((await parked).ms).toBe(600);
  time.advance(600); time.requests[0]!.release();
  await second;
  time.advance(5000);
  await paced.extract(claim("c"));
  expect(time.requests).toHaveLength(1);
  expect(paced.stats()).toEqual({ calls_total: 3, waited_total_ms: 600, last_call_at: 6000 });
});

test("three concurrent calls leave the gate in FIFO order, one parked wait at a time", async () => {
  const time = fakeTime();
  const { provider, calls } = recordingProvider();
  const paced = new PacedExtractionProvider(provider, { minIntervalMs: 1000, clock: time.clock, wait: time.wait, random: () => 0 });
  const parkedSecond = time.nextWait();
  const settled: string[] = [];
  const track = (text: string) => paced.extract(claim(text)).then(() => { settled.push(text); });
  const [a, b, c] = [track("a"), track("b"), track("c")];
  // Two independent events: the head call settles, and the next call is parked in the gate.
  const [, second] = await Promise.all([a, parkedSecond]);
  expect(second.ms).toBe(1000);
  expect(calls).toEqual(["a"]);
  expect(time.requests).toHaveLength(1); // "c" is queued behind "b", not sleeping on its own timer.
  const parkedThird = time.nextWait();
  time.advance(1000); second.release();
  const [, third] = await Promise.all([b, parkedThird]);
  expect(third.ms).toBe(1000);
  expect(calls).toEqual(["a", "b"]);
  time.advance(1000); third.release();
  await c;
  expect(calls).toEqual(["a", "b", "c"]);
  expect(settled).toEqual(["a", "b", "c"]);
  expect(paced.stats()).toEqual({ calls_total: 3, waited_total_ms: 3000, last_call_at: 2000 });
});

test("a rejected inner call still releases the gate for the next waiter", async () => {
  const time = fakeTime();
  const { provider, calls } = recordingProvider("bad");
  const paced = new PacedExtractionProvider(provider, { minIntervalMs: 1000, clock: time.clock, wait: time.wait, random: () => 0 });
  const failing = paced.extract(claim("bad"));
  const parked = time.nextWait();
  const next = paced.extract(claim("good"));
  await expect(failing).rejects.toThrow("provider_rejected:bad");
  const request = await parked;
  time.advance(1000); request.release();
  expect(await next).toEqual({ echoed: "good" });
  expect(calls).toEqual(["bad", "good"]);
  expect(paced.stats().calls_total).toBe(2);
});
