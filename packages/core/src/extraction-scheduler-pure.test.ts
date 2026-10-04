import { expect, spyOn, test } from "bun:test";
import { modelTaskRetryDelayMs } from "./backoff.ts";
import { ExtractionScheduler, type SchedulerEngine } from "./extraction-scheduler.ts";
import type { ExtractionProvider } from "./extraction.ts";
import { GenerationReadinessError, type InstallationContext } from "./store.ts";
import type { Coverage, ExtractionPipeline, Generation, ModelTask } from "@anamnesis/protocol";

const uuid = (n: number) => `01993000-0000-7000-8000-${String(n).padStart(12, "0")}`;
const HEX = "a".repeat(64), GEN = uuid(1);
const context: InstallationContext = { principal: "installation", commit_mode: "receipt", client_binding: uuid(2) };
const provider: ExtractionProvider = { model: "qa-scheduler", modelIncarnation: HEX, extract: () => Promise.reject(new Error("provider must not be called")) };
const PARTITIONS = ["episodes", "active_extraction"] as const;
const WINDOW = 256;

type Known = Extract<ExtractionPipeline, { state: "known" }>;
type Patch = Partial<Pick<ModelTask, "attempts" | "lost_leases" | "updated_at" | "kind">> & { expires_at?: number };

function task(id: string, state: ModelTask["state"], patch: Patch = {}): ModelTask {
  const { expires_at, ...rest } = patch;
  const leased = state === "leased";
  return { id, generation_id: GEN, source_id: uuid(9), source_revision: HEX, body_digest: HEX, source_ingest_seq: 1,
    attempt_id: state === "queued" ? null : uuid(7), kind: "claim", model: provider.model, model_incarnation: HEX, state,
    lease: leased ? { worker_id: "other-writer", epoch: uuid(8), writer_epoch: 1, expires_at: expires_at ?? 1 } : null,
    policy_context: leased ? { revision: 1, authority: "installation" } : null, version: 3, attempts: leased ? 1 : 0,
    created_at: 0, updated_at: 0, ...rest };
}
function known(claim: ModelTask, judge: ModelTask | null = null, relation_judge?: Known["relation_judge"]): Known {
  return { state: "known", pipeline_id: claim.id, mode: "claim-judge-audit-v1", semantic_writes: false, claim, claim_attempt: null, judge, judge_attempt: null, decisions: [], relation_judge };
}
let judges = 500;
const done = (claim: ModelTask, relation: Known["relation_judge"] = "complete") => known(task(claim.id, "succeeded"), task(uuid(++judges), "succeeded", { kind: "judge" }), relation);
function generation(state: Generation["state"], covered_ingest_seq = 0): Generation {
  return { id: GEN, stream: "extraction", incarnation: HEX, state, covered_ingest_seq, created_at: 0, updated_at: 0 };
}

interface Episode { id: string; seq: number; task?: ModelTask }

class Harness {
  readonly calls: string[] = [];
  readonly engine: SchedulerEngine;
  selection: { generation_id: string | null; selector_version: number } = { generation_id: null, selector_version: 1 };
  readonly generations = new Map<string, Generation>();
  catchingUp: Generation[] = [];
  episodes: Episode[] = [];
  live = 0;
  now = 1000;
  readonly coverage = new Map<string, number>();
  readonly relationFailures = new Map<string, number>();
  readonly staleLeases = new Set<string>();
  readonly exploding = new Set<string>();
  cutover: () => Generation = () => { throw new GenerationReadinessError("coverage_incomplete"); };
  onCreate: () => void = () => {};
  private wakes = 0;
  private readonly waiters: { ready: () => boolean; resolve: () => void }[] = [];
  private readonly holds = new Map<string, Promise<void>>();
  private readonly pipelines = new Map<string, ExtractionPipeline>();
  private readonly scripts = new Map<string, ExtractionPipeline[]>();
  private readonly owner = new Map<string, string>();
  private readonly sources = new Map<string, ExtractionPipeline[]>();

  constructor() {
    const record = (name: string) => { this.calls.push(name); this.settle(); };
    this.engine = {
      readExtractionSelection: async () => { record("selection"); return { ...this.selection }; },
      cutoverExtractionGeneration: async input => { record(`cutover:${input.expected_generation_id}:${input.expected_selector_version}`); return this.cutover(); },
      createExtractionPipeline: async input => {
        record(`create:${input.source_id}`); this.onCreate();
        const states = this.sources.get(input.source_id), episode = this.episodes.find(e => e.id === input.source_id);
        if (!states || !episode) throw new Error(`no script for source ${input.source_id}`);
        const { claim } = this.register(states);
        episode.task = claim;
        return claim;
      },
      runExtractionPipeline: async input => {
        record(`run:${input.task_id}@${input.expected_version}`);
        if (this.exploding.has(input.task_id)) throw new Error("boom");
        await this.holds.get(input.task_id);
        return this.next(this.owner.get(input.task_id) ?? input.task_id);
      },
      store: {
        getExtractionGeneration: async id => { record(`generation:${id}`); const found = this.generations.get(id); if (!found) throw new Error("unknown generation"); return found; },
        getExtractionTaskByWorkKey: async workKey => {
          const episode = this.episodes.find(e => `${GEN}:${e.id}` === workKey);
          if (!episode?.task) return null;
          const pipeline = this.pipelines.get(episode.task.id);
          return pipeline?.state === "known" ? pipeline.claim : episode.task;
        },
        createExtractionGeneration: async input => { record(`createGeneration:${input.state}:${input.created_at}`); this.generations.set(input.id, input); return input; },
        recordExtractionCoverage: async input => {
          record(`cover:${input.partition}:${input.expected_covered_ingest_seq}->${input.covered_ingest_seq}`);
          const key = `${input.generation_id}:${input.partition}`;
          if ((this.coverage.get(key) ?? 0) !== input.expected_covered_ingest_seq) throw new Error("coverage_conflict");
          this.coverage.set(key, input.covered_ingest_seq);
          const cursor = Math.min(...PARTITIONS.map(partition => this.coverage.get(`${input.generation_id}:${partition}`) ?? 0));
          for (const g of [...this.generations.values(), ...this.catchingUp]) if (g.id === input.generation_id) g.covered_ingest_seq = cursor;
          const coverage: Coverage = { generation_id: input.generation_id, partition: input.partition, required_ingest_seq: input.covered_ingest_seq, covered_ingest_seq: input.covered_ingest_seq, omission_digest: HEX, updated_at: this.now };
          return coverage;
        },
        readExtractionPipeline: async id => this.pipelines.get(id) ?? { state: "unknown", pipeline_id: id },
        factRelationFailures: async id => this.relationFailures.get(id) ?? 0,
        sealFactRelationOmission: async request => { record(`seal:${request.min_failures}`); this.next(request.pipeline_id); return { sealed: true, failures: request.min_failures }; },
        retryModelTask: async input => { record(`retry:${input.task_id}@${input.expected_version}`); return this.advance(input.task_id).claim; },
        cancelModelTask: async input => { record(`cancel:${input.task_id}`); return this.advance(input.task_id).claim; },
        settleModelTask: async input => {
          record(`settle:${input.reason}:${input.task_id}`);
          if (input.reason === "worker_lost" && this.staleLeases.has(input.task_id)) throw new Error("lease epoch mismatch");
          return this.advance(input.task_id).claim;
        },
      },
    };
  }

  /** Scripts the pipeline states a source moves through: the first is observed after create, each later one after a write. */
  script(source: Episode, ...states: ExtractionPipeline[]): void {
    if (source.task) this.register(states, source.task.id); else this.sources.set(source.id, states);
  }
  private register(states: ExtractionPipeline[], id = states[0]?.pipeline_id): Known {
    const [first, ...rest] = states;
    if (!first || first.state !== "known" || !id) throw new Error("a script starts with a known pipeline");
    this.pipelines.set(id, first); this.scripts.set(id, rest);
    for (const state of states) if (state.state === "known") for (const t of [state.claim, state.judge]) if (t) this.owner.set(t.id, id);
    return first;
  }
  private next(id: string): ExtractionPipeline {
    const following = this.scripts.get(id)?.shift();
    if (following) this.pipelines.set(id, following);
    return this.pipelines.get(id) ?? { state: "unknown", pipeline_id: id };
  }
  private advance(taskId: string): Known {
    const state = this.next(this.owner.get(taskId) ?? taskId);
    if (state.state !== "known") throw new Error(`task ${taskId} advanced into an unknown pipeline`);
    return state;
  }

  readonly read = async <Row extends Record<string, unknown>>(query: string, params: Record<string, unknown>): Promise<Row[]> => {
    let rows: Record<string, unknown>[];
    if (query.includes("state:'catching_up'")) rows = this.catchingUp.map(body => ({ body: JSON.stringify(body) }));
    else if (query.includes("ExtractionCoverage")) rows = PARTITIONS.map(partition => ({ partition, covered: this.coverage.get(`${params.generation}:${partition}`) ?? null }));
    else {
      const from = Number(params.from);
      rows = this.episodes.filter(e => e.seq > from && e.seq <= this.live).sort((a, b) => a.seq - b.seq).slice(0, WINDOW)
        .map(e => ({ live: this.live, id: e.id, seq: e.seq }));
      if (rows.length === 0) rows = [{ live: this.live, id: null, seq: null }];
    }
    // The daemon's reader is generic over the row shape; the scripted rows carry exactly the columns each query names.
    return rows as Row[];
  };

  readonly wake = (): void => { this.wakes++; this.settle(); };
  /** deferWake reads the clock immediately before arming its timer, so a check queued from that read observes the timer. */
  readonly clock = (): number => { this.settle(); return this.now; };
  private settle(): void {
    queueMicrotask(() => { for (const waiter of this.waiters.splice(0)) if (waiter.ready()) waiter.resolve(); else this.waiters.push(waiter); });
  }
  until(ready: () => boolean): Promise<void> {
    return ready() ? Promise.resolve() : new Promise(resolve => { this.waiters.push({ ready, resolve }); });
  }
  /** Resolves once the lane has been re-armed `count` times in total. */
  woken(count: number): Promise<void> { return this.until(() => this.wakes >= count); }
  called(prefix: string, count: number): Promise<void> { return this.until(() => this.calls.filter(c => c.startsWith(prefix)).length >= count); }
  /** Parks the next run of `taskId` until the returned release is called, so a drive stays in flight across turns. */
  hold(taskId: string): () => void {
    let release: () => void = () => {};
    this.holds.set(taskId, new Promise(resolve => { release = resolve; }));
    return release;
  }

  scheduler(options: { maxInFlight?: number; maxAttempts?: number; maxLostLeases?: number } = {}): ExtractionScheduler {
    return new ExtractionScheduler(this.engine, { provider, context, read: this.read, wake: this.wake, clock: this.clock, ...options });
  }
  episode(seq: number, task?: ModelTask): Episode {
    const episode: Episode = { id: uuid(100 + seq), seq, ...(task ? { task } : {}) };
    this.episodes.push(episode);
    this.live = Math.max(this.live, seq);
    return episode;
  }
}

test("creates a catching-up generation, seeds both partitions, and cuts over once nothing is pending", async () => {
  const h = new Harness();
  h.cutover = () => generation("active");
  const scheduler = h.scheduler();
  expect(scheduler.status()).toEqual({ state: "starting" });
  expect(await scheduler.turn()).toBe("idle");
  expect(h.calls).toEqual(["selection", "createGeneration:catching_up:1000", "cover:episodes:0->0", "cover:active_extraction:0->0", "selection", "cutover:null:1"]);
  expect(scheduler.status()).toMatchObject({ state: "active", generation_id: GEN, covered_ingest_seq: 0, live_ingest_seq: 0, in_flight: 0, completed_total: 0, failed_total: 0, last_error: null });
});

test("adopts the catching-up row, retries the cutover while coverage is incomplete, then follows the selection", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up", 3)];
  h.live = 3;
  h.coverage.set(`${GEN}:episodes`, 5);
  const scheduler = h.scheduler();
  expect(await scheduler.turn()).toBe("more");
  expect(h.calls).toEqual(["selection", "cover:active_extraction:0->3", "selection", "cutover:null:1"]);
  expect(scheduler.status()).toMatchObject({ state: "catching_up", covered_ingest_seq: 3, live_ingest_seq: 3 });
  h.selection = { generation_id: GEN, selector_version: 2 };
  h.generations.set(GEN, generation("active", 3));
  expect(await scheduler.turn()).toBe("idle");
  expect(h.calls.slice(4)).toEqual(["selection", `generation:${GEN}`]);
  expect(scheduler.status()).toMatchObject({ state: "active" });
});

test("drives a fresh pipeline through claim and judge, seals coverage, and cuts over", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const e1 = h.episode(1), claim = task(uuid(300), "queued");
  h.script(e1, known(claim), known(task(claim.id, "succeeded")), done(claim));
  const scheduler = h.scheduler();
  expect(await scheduler.turn()).toBe("waiting");
  expect(scheduler.inFlightCount).toBe(1);
  await h.woken(1);
  expect(h.calls).toEqual(["selection", `create:${e1.id}`, `run:${claim.id}@3`, `run:${claim.id}@3`]);
  expect(scheduler.status()).toMatchObject({ completed_total: 1, failed_total: 0, in_flight: 0 });
  expect(await scheduler.turn()).toBe("more");
  expect(h.calls.slice(4)).toEqual(["selection", "cover:episodes:0->1", "cover:active_extraction:0->1", "selection", "cutover:null:1"]);
  h.cutover = () => generation("active", 1);
  expect(await scheduler.turn()).toBe("idle");
  expect(h.calls.slice(9)).toEqual(["selection", "selection", "cutover:null:1"]);
  expect(scheduler.status()).toMatchObject({ state: "active", covered_ingest_seq: 1, live_ingest_seq: 1, completed_total: 1 });
});

test("adopts persisted outcomes without counting them and counts omitted or cancelled pipelines as failures", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const adopted = task(uuid(301), "succeeded"), omitted = task(uuid(302), "queued"), cancelled = task(uuid(303), "queued");
  h.script(h.episode(1, adopted), done(adopted));
  h.script(h.episode(2), done(omitted, "omitted"));
  h.script(h.episode(3), known(task(cancelled.id, "cancelled")));
  const scheduler = h.scheduler();
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(3);
  expect(scheduler.status()).toMatchObject({ completed_total: 0, failed_total: 2, in_flight: 0 });
  expect(h.calls.filter(c => c.startsWith("run:"))).toEqual([]);
  expect(await scheduler.turn()).toBe("more");
  expect(h.calls.filter(c => c.startsWith("cover:"))).toEqual(["cover:episodes:0->3", "cover:active_extraction:0->3"]);
});

test("retries a failed claim only after its backoff elapses, waking the lane at that moment", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const claim = task(uuid(304), "failed", { attempts: 1, updated_at: h.now - modelTaskRetryDelayMs(1) + 1 });
  h.script(h.episode(1), known(claim), known(task(claim.id, "queued")), done(claim));
  const scheduler = h.scheduler();
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(1);
  expect(h.calls.filter(c => !c.startsWith("selection"))).toEqual([`create:${uuid(101)}`]);
  h.now += 1;
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(2);
  expect(h.calls.slice(-2)).toEqual([`retry:${claim.id}@3`, `run:${claim.id}@3`]);
  expect(scheduler.status()).toMatchObject({ completed_total: 1, failed_total: 0 });
});

test("settles expired leases as worker_lost, falling back to expired when the lease epoch is stale", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const held = task(uuid(305), "leased", { expires_at: h.now + 1 }), stale = task(uuid(306), "leased", { expires_at: h.now - 1 });
  h.staleLeases.add(stale.id);
  h.script(h.episode(1), known(held), known(task(held.id, "queued")), done(held));
  h.script(h.episode(2), known(stale), known(task(stale.id, "queued")), done(stale));
  const scheduler = h.scheduler();
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(2);
  expect(h.calls.filter(c => c.startsWith("settle:"))).toEqual([`settle:worker_lost:${stale.id}`, `settle:expired:${stale.id}`]);
  expect(scheduler.status()).toMatchObject({ completed_total: 1 });
  h.now += 1;
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(3);
  expect(h.calls.filter(c => c.startsWith("settle:")).slice(2)).toEqual([`settle:worker_lost:${held.id}`]);
  expect(scheduler.status()).toMatchObject({ completed_total: 2, failed_total: 0 });
});

test("spends the lost-lease budget on retries, then cancels; exhausted attempts and a queued judge are classified too", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const retry = task(uuid(307), "expired"), cancel = task(uuid(308), "worker_lost", { lost_leases: 1 });
  const writesInAnyOrder = (calls: string[]) => calls.filter(c => /^(retry|cancel|run):/.test(c)).sort();
  const spent = task(uuid(309), "failed", { attempts: 4 }), judged = task(uuid(310), "succeeded");
  h.script(h.episode(1), known(retry), known(task(retry.id, "queued")), done(retry));
  h.script(h.episode(2), known(cancel), known(task(cancel.id, "cancelled")));
  h.script(h.episode(3), known(spent));
  h.script(h.episode(4), known(judged, task(uuid(400), "queued", { kind: "judge" })), done(judged));
  const scheduler = h.scheduler({ maxLostLeases: 1 });
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(4);
  expect(writesInAnyOrder(h.calls)).toEqual(writesInAnyOrder([`retry:${retry.id}@3`, `run:${retry.id}@3`, `cancel:${cancel.id}`, `run:${judged.id}@3`]));
  expect(scheduler.status()).toMatchObject({ completed_total: 2, failed_total: 2 });
});

test("runs pending relation verdicts, and seals the source as an omission once the premise failures exhaust the budget", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const pending = task(uuid(311), "succeeded"), exhausted = task(uuid(312), "succeeded");
  h.script(h.episode(1), done(pending, "pending"), done(pending));
  h.script(h.episode(2), done(exhausted, "pending"), done(exhausted, "omitted"));
  h.relationFailures.set(exhausted.id, 4);
  const scheduler = h.scheduler();
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(2);
  expect(h.calls.filter(c => /^(seal|run):/.test(c))).toEqual([`run:${pending.id}@3`, "seal:4"]);
  expect(scheduler.status()).toMatchObject({ completed_total: 1, failed_total: 1 });
});

test("leaves unknown pipelines and a closing scheduler alone, and reports a pipeline that never settles", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const vanishing = task(uuid(313), "queued"), stuck = task(uuid(314), "queued");
  h.script(h.episode(1), known(vanishing), { state: "unknown", pipeline_id: vanishing.id });
  h.script(h.episode(2), known(stuck));
  const scheduler = h.scheduler({ maxAttempts: 1, maxLostLeases: 0 });
  expect(await scheduler.turn()).toBe("waiting");
  await h.called("run:", 1 + 7);
  await scheduler.close();
  expect(h.calls.filter(c => c.startsWith("run:")).length).toBe(1 + 7);
  expect(scheduler.status()).toMatchObject({ completed_total: 0, failed_total: 0, in_flight: 0, last_error: `Error: pipeline ${stuck.id} did not settle within 7 steps` });
  expect(await scheduler.turn()).toBe("idle");
});

test("records a thrown drive and re-arms the lane so the retryable task is settled later", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const claim = task(uuid(315), "queued");
  h.exploding.add(claim.id);
  h.script(h.episode(1), known(claim));
  const scheduler = h.scheduler();
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(1);
  expect(scheduler.status()).toMatchObject({ completed_total: 0, failed_total: 0, in_flight: 0, last_error: "Error: boom" });
});

test("close clears the deferred wake and stops a drive before it classifies", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const far = task(uuid(316), "leased", { expires_at: h.now + 100000 });
  h.script(h.episode(1), known(far));
  const armed = spyOn(globalThis, "setTimeout"), cleared = spyOn(globalThis, "clearTimeout");
  try {
    const first = h.scheduler();
    expect(await first.turn()).toBe("waiting");
    await h.until(() => armed.mock.calls.length === 1);
    await first.close();
    expect(first.status()).toMatchObject({ in_flight: 0, last_error: null });
    expect(cleared).toHaveBeenCalledTimes(1);
    expect(armed.mock.results[0]?.value).toBe(cleared.mock.calls[0]?.[0]);
  } finally { armed.mockRestore(); cleared.mockRestore(); }
  const h2 = new Harness();
  h2.catchingUp = [generation("catching_up")];
  const claim = task(uuid(317), "queued");
  h2.script(h2.episode(1), known(claim), done(claim));
  const second = h2.scheduler();
  h2.onCreate = () => { void second.close(); };
  expect(await second.turn()).toBe("waiting");
  await second.close();
  expect(h2.calls.filter(c => c.startsWith("run:"))).toEqual([]);
  expect(second.status()).toMatchObject({ completed_total: 0, in_flight: 0 });
});

test("bounds in-flight pipelines, skips keys already in flight, and reports a sealed full window as more work", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  const first = task(uuid(318), "queued"), second = task(uuid(319), "queued");
  h.script(h.episode(1), known(first), done(first));
  h.script(h.episode(2), known(second), done(second));
  const release = h.hold(first.id);
  const scheduler = h.scheduler({ maxInFlight: 1 });
  expect(await scheduler.turn()).toBe("waiting");
  expect(await scheduler.turn()).toBe("waiting");
  expect(h.calls.filter(c => c.startsWith("create:"))).toEqual([`create:${uuid(101)}`]);
  release();
  await h.woken(1);
  expect(await scheduler.turn()).toBe("waiting");
  await h.woken(2);
  expect(h.calls.filter(c => c.startsWith("create:"))).toEqual([`create:${uuid(101)}`, `create:${uuid(102)}`]);
  expect(scheduler.status()).toMatchObject({ covered_ingest_seq: 1, completed_total: 2 });
  expect(await scheduler.turn()).toBe("more");
  expect(h.calls.slice(-4)).toEqual(["cover:episodes:1->2", "cover:active_extraction:1->2", "selection", "cutover:null:1"]);
  expect(scheduler.status()).toMatchObject({ covered_ingest_seq: 2, live_ingest_seq: 2, completed_total: 2 });

  const wide = new Harness();
  wide.catchingUp = [generation("catching_up")];
  for (let seq = 1; seq <= WINDOW; seq++) { const adopted = task(uuid(1000 + seq), "succeeded"); wide.script(wide.episode(seq, adopted), done(adopted)); }
  wide.live = WINDOW + 1;
  const sealed = wide.scheduler({ maxInFlight: WINDOW });
  expect(await sealed.turn()).toBe("waiting");
  await wide.woken(WINDOW);
  expect(await sealed.turn()).toBe("more");
  expect(sealed.status()).toMatchObject({ covered_ingest_seq: WINDOW, live_ingest_seq: WINDOW + 1, completed_total: 0 });
});

test("propagates cutover failures other than incomplete coverage", async () => {
  const h = new Harness();
  h.catchingUp = [generation("catching_up")];
  h.cutover = () => { throw new GenerationReadinessError("selector_conflict"); };
  await expect(h.scheduler().turn()).rejects.toMatchObject({ code: "selector_conflict" });
  h.cutover = () => { throw new Error("transport"); };
  await expect(h.scheduler().turn()).rejects.toThrow("transport");
});
