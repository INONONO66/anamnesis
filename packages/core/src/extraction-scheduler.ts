// Extraction scheduler (#209 part B): feeds committed Episodes through the
// claim/judge audit pipeline into exactly one writable generation, seals
// terminal outcomes as coverage, and cuts a caught-up generation over.
//
// One `turn()` is one bounded unit of database work owned by the daemon's
// serial writer: it never awaits a provider call. Pipelines run as tracked
// in-flight promises (at most `maxInFlight`); a terminal settlement re-arms
// the lane through `wake`, so the loop only advances on real events and an
// unresolved or failing pipeline waits for the next remember/recovery wake.
import { modelTaskRetryDelayMs } from "./backoff.ts";
import { v7 as uuidv7 } from "uuid";
import type { Engine } from "./engine.ts";
import type { ExtractionProvider } from "./extraction.ts";
import { ExtractionGateError, type ExtractionGate } from "./extraction-gate.ts";
import { GenerationReadinessError, type InstallationContext } from "./store.ts";
import { Generation, type ModelTask } from "@anamnesis/protocol";
import type { ExtractionPipeline } from "@anamnesis/protocol";

/** "waiting": pipelines are in flight and their settlement wakes the lane; "more": re-arm immediately. */
export type ExtractionTurn = "more" | "waiting" | "idle";
export type ExtractionSchedulerStatus = { state: "starting" } | {
  state: "catching_up" | "active"; generation_id: string; covered_ingest_seq: number; live_ingest_seq: number;
  in_flight: number; completed_total: number; failed_total: number; gated_total: number; last_error: string | null;
};
export interface ExtractionSchedulerOptions {
  provider: ExtractionProvider;
  context: InstallationContext;
  /** Bounded schedule scan on the reader; all writes go through the engine's writer barrier. */
  read<Row extends Record<string, unknown>>(query: string, params: Record<string, unknown>): Promise<Row[]>;
  /** Re-arms the lane once an in-flight pipeline reaches a terminal outcome. */
  wake(): void;
  workerId?: string;
  maxInFlight?: number;
  gate?: ExtractionGate | undefined;
  gateThreshold?: number | undefined;
  windowMax?: number;
  contextMax?: number;
  /** First attempt plus retries; a task failing this often is a terminal omission. */
  maxAttempts?: number;
  /** Leases lost without a provider outcome (a restart, an overrun) that a task may recover from; they never spend
   * `maxAttempts`. Losing this many is cancelled into a durable omission (default 3). */
  maxLostLeases?: number;
  /** Same clock as the engine's store, so lease expiry is judged once; tests inject it, the daemon uses Date.now. */
  clock?: () => number;
}

/** Three times the daemon's 30 s provider timeout: the lease is taken before the HTTP call and checked when the attempt is
 * recorded, so a lease equal to the timeout turned every slow-but-successful call into lease_expired (E2E run 4 sealed two sources). */
const LEASE_MS = 90000;

type Outcome = "completed" | "failed";
type Known = Extract<ExtractionPipeline, { state: "known" }>;
type Action = Outcome | "run" | "pending" | "unresolved" | { kind: "retry" | "settle" | "cancel"; task: ModelTask };
interface ScanRow extends Record<string, unknown> { live: number; id: string | null; seq: number | null; session: string | null }
type WindowRow = ScanRow & { id: string; seq: number };
type Driven = { outcome: Outcome; fresh: boolean } | null;

/** The engine surface the scheduler drives: the daemon passes its Engine, pure tests a scripted fake. */
export type SchedulerEngine = Pick<Engine, "readExtractionSelection" | "cutoverExtractionGeneration" | "createExtractionPipeline" | "runExtractionPipeline" | "runExtractionWindow"> & {
  readonly store: Pick<Engine["store"], "getExtractionGeneration" | "createExtractionGeneration" | "recordExtractionCoverage" | "readExtractionPipeline"
    | "factRelationFailures" | "sealFactRelationOmission" | "retryModelTask" | "cancelModelTask" | "settleModelTask" | "getExtractionTaskByWorkKey" | "extractionContextInput">;
};

const PARTITIONS = ["episodes", "active_extraction"] as const;
/** Scan window and coverage step; recordExtractionCoverage refuses larger batches. */
const COVERAGE_STEP = 256;

/** Balanced partitions keep a ten-Episode session at 5+5 rather than 8+2. */
function extractionWindows(rows: WindowRow[], max: number): WindowRow[][] {
  const sessions = new Map<string, WindowRow[]>();
  for (const row of rows) {
    const key = row.session ?? row.id;
    const group = sessions.get(key) ?? [];
    group.push(row);
    sessions.set(key, group);
  }
  return [...sessions.values()].flatMap(group => {
    const count = Math.ceil(group.length / max), size = Math.floor(group.length / count), remainder = group.length % count;
    let offset = 0;
    return Array.from({ length: count }, (_, index) => {
      const window = group.slice(offset, offset + size + (index < remainder ? 1 : 0));
      offset += window.length;
      return window;
    });
  });
}

export class ExtractionScheduler {
  private readonly provider: ExtractionProvider;
  private readonly context: InstallationContext;
  private readonly read: ExtractionSchedulerOptions["read"];
  private readonly wake: () => void;
  private readonly workerId: string;
  private readonly maxInFlight: number;
  private readonly maxAttempts: number;
  private readonly maxLostLeases: number;
  private readonly clock: () => number;
  private readonly gate: ExtractionGate | undefined;
  private readonly gateThreshold: number;
  private readonly windowMax: number;
  private readonly contextMax: number;
  /** work_key -> settlement; a drive removes itself before waking. */
  private readonly inFlight = new Map<string, Promise<void>>();
  /** work_key -> terminal outcome awaiting coverage. Dropped once sealed. */
  private readonly outcomes = new Map<string, Outcome>();
  private generation: Generation | undefined;
  private live = 0;
  private completed_total = 0;
  private failed_total = 0;
  private gated_total = 0;
  /** Oversized windows retry as single targets, including after adopting a persisted failure. */
  private readonly singletons = new Set<string>();
  private readonly rewindow = new Set<string>();
  private last_error: string | null = null;
  private closed = false;
  /** Earliest lease expiry the lane must revisit (a lease held by a lost writer or a previous incarnation). */
  private deferred: { at: number; timer: ReturnType<typeof setTimeout> } | undefined;

  constructor(private readonly engine: SchedulerEngine, options: ExtractionSchedulerOptions) {
    this.provider = options.provider; this.context = options.context; this.read = options.read; this.wake = options.wake;
    this.workerId = options.workerId ?? "daemon-extraction";
    this.maxInFlight = options.maxInFlight ?? 4;
    this.maxAttempts = options.maxAttempts ?? 4;
    this.maxLostLeases = options.maxLostLeases ?? 3;
    this.clock = options.clock ?? Date.now;
    this.gate = options.gate;
    this.gateThreshold = options.gateThreshold ?? 0.2;
    this.windowMax = options.windowMax ?? 8;
    this.contextMax = options.contextMax ?? 3;
    if (!Number.isInteger(this.windowMax) || this.windowMax < 1 || this.windowMax > 8) throw new TypeError("windowMax must be 1..8");
    if (!Number.isInteger(this.contextMax) || this.contextMax < 0 || this.contextMax > 3) throw new TypeError("contextMax must be 0..3");
    if (!Number.isFinite(this.gateThreshold) || this.gateThreshold < 0 || this.gateThreshold > 1) throw new TypeError("gateThreshold must be 0..1");
  }

  get inFlightCount(): number { return new Set(this.inFlight.values()).size; }

  status(): ExtractionSchedulerStatus {
    if (!this.generation) return { state: "starting" };
    return { state: this.generation.state === "active" ? "active" : "catching_up", generation_id: this.generation.id,
      covered_ingest_seq: this.generation.covered_ingest_seq, live_ingest_seq: this.live, in_flight: this.inFlightCount,
      completed_total: this.completed_total, failed_total: this.failed_total, gated_total: this.gated_total, last_error: this.last_error };
  }

  recordError(error: unknown): void { this.last_error = String(error).slice(0, 512); }

  /** Stops starting provider work and waits for in-flight pipelines to settle. */
  async close(): Promise<void> {
    this.closed = true;
    if (this.deferred) { clearTimeout(this.deferred.timer); this.deferred = undefined; }
    await Promise.allSettled([...this.inFlight.values()]);
  }

  async turn(): Promise<ExtractionTurn> {
    if (this.closed) return "idle";
    const generation = await this.writableGeneration();
    const rows = await this.read<ScanRow>(`MATCH (m:Meta {key:'meta'})
      OPTIONAL MATCH (e:Element:Episode) WHERE e.ingest_seq > $from AND e.ingest_seq <= m.ingest_seq
      RETURN m.ingest_seq AS live,e.id AS id,e.ingest_seq AS seq,e.session_key AS session ORDER BY seq LIMIT ${COVERAGE_STEP}`,
      { from: generation.covered_ingest_seq, generation: generation.id });
    this.live = rows[0]?.live ?? generation.covered_ingest_seq;
    const { prefix, open } = this.scanWindow(generation, rows);
    if (prefix > generation.covered_ingest_seq) await this.advanceCoverage(generation, rows, prefix);
    if (generation.state === "catching_up" && generation.covered_ingest_seq === this.live && this.inFlight.size === 0 && !await this.tryCutover(generation)) return "more";
    if (this.inFlight.size) return "waiting";
    // A full window that sealed completely may hide more terminal work behind it.
    return open && rows.length === COVERAGE_STEP && generation.covered_ingest_seq < this.live ? "more" : "idle";
  }
  /** Contiguous terminal prefix beyond the cursor; behind it, start whatever is neither settled nor in flight. */
  private scanWindow(generation: Generation, rows: ScanRow[]): { prefix: number; open: boolean } {
    let prefix = generation.covered_ingest_seq, open = true;
    const pending: WindowRow[] = [];
    for (const row of rows) {
      if (row.id === null || row.seq === null) break;
      const key = `${generation.id}:${row.id}`;
      if (open && row.seq === prefix + 1 && this.outcomes.has(key)) { prefix = row.seq; continue; }
      open = false;
      if (this.inFlight.has(key) || this.outcomes.has(key)) continue;
      pending.push({ ...row, id: row.id, seq: row.seq });
    }
    const windows = extractionWindows(pending, this.windowMax).flatMap(window =>
      window.some(row => this.singletons.has(`${generation.id}:${row.id}`)) ? window.map(row => [row]) : [window]);
    for (const window of windows) {
      if (this.inFlightCount >= this.maxInFlight) break;
      this.start(generation, window);
    }
    return { prefix, open };
  }
  private async advanceCoverage(generation: Generation, rows: ScanRow[], prefix: number): Promise<void> {
    await this.cover(generation, prefix);
    for (const row of rows) if (row.seq !== null && row.seq <= prefix) {
      const key = `${generation.id}:${row.id}`;
      this.outcomes.delete(key); this.singletons.delete(key);
    }
    generation.covered_ingest_seq = prefix;
  }
  /** False when an Episode committed between the scan and the cutover; the next turn covers it first. */
  private async tryCutover(generation: Generation): Promise<boolean> {
    await this.cover(generation, generation.covered_ingest_seq); // Both partitions must exist, even when nothing was ever covered.
    const selection = await this.engine.readExtractionSelection(this.context);
    try {
      this.generation = await this.engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id,
        expected_selector_version: selection.selector_version }, this.context);
      return true;
    } catch (error) {
      if (!(error instanceof GenerationReadinessError && error.code === "coverage_incomplete")) throw error;
      return false;
    }
  }

  /** The active generation, else the one catching up, else a fresh catching_up generation for this provider. */
  private async writableGeneration(): Promise<Generation> {
    const selection = await this.engine.readExtractionSelection(this.context);
    if (selection.generation_id) return this.generation = await this.engine.store.getExtractionGeneration(selection.generation_id, this.context);
    const rows = await this.read<{ body: string }>(`MATCH (g:ExtractionGeneration {state:'catching_up'}) RETURN g.body AS body ORDER BY g.id LIMIT 1`, {});
    if (rows[0]) return this.generation = Generation.parse(JSON.parse(rows[0].body));
    const now = this.clock();
    return this.generation = await this.engine.store.createExtractionGeneration({ id: uuidv7(), stream: "extraction", incarnation: this.provider.modelIncarnation,
      state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now }, this.context);
  }

  private async cover(generation: Generation, target: number): Promise<void> {
    const partitions = await this.read<{ partition: string; covered: number | null }>(`UNWIND $partitions AS partition
      OPTIONAL MATCH (c:ExtractionCoverage {key:$generation+':'+partition})
      RETURN partition,c.covered_ingest_seq AS covered`, { partitions: [...PARTITIONS], generation: generation.id });
    for (const partition of PARTITIONS) {
      const prior = partitions.find(row => row.partition === partition)?.covered ?? null;
      if (prior !== null && prior >= target) continue;
      const covered = prior ?? 0;
      await this.engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: covered, covered_ingest_seq: target }, this.context);
    }
  }

  private start(generation: Generation, rows: WindowRow[]): void {
    const keys = rows.map(row => `${generation.id}:${row.id}`);
    const drive = this.driveWindow(generation, rows)
      .catch(error => { this.recordError(error); return true; })
      .then(settled => {
        for (const key of keys) { this.inFlight.delete(key); if (this.rewindow.delete(key)) settled = true; }
        if (settled) this.wake();
      });
    for (const key of keys) this.inFlight.set(key, drive);
  }

  private recordOutcome(key: string, result: Driven): boolean {
    if (!result) return false;
    this.outcomes.set(key, result.outcome);
    if (result.fresh) { if (result.outcome === "completed") this.completed_total++; else this.failed_total++; }
    return true;
  }

  private async driveWindow(generation: Generation, rows: WindowRow[]): Promise<boolean> {
    const queued: ModelTask[] = [];
    let settled = false;
    for (const row of rows) {
      const key = `${generation.id}:${row.id}`;
      const existing = await this.engine.store.getExtractionTaskByWorkKey(key, this.context);
      const claim = existing ?? await this.engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: row.id }, this.context);
      if (this.closed) return settled;
      if (claim.state === "queued") queued.push(claim);
      else settled = this.recordOutcome(key, await this.drive(claim, existing === null)) || settled;
    }
    if (!queued.length || this.closed) return settled;
    if (await this.skipWindow(queued)) return true;
    // A byte-bound retry has no context: otherwise a preceding large Episode could make even a small lone target fail forever.
    const context_ids = queued.some(task => this.singletons.has(`${generation.id}:${task.source_id}`))
      ? [] : await this.contextIds(rows.filter(row => queued.some(task => task.source_id === row.id)));
    const attempts = await this.engine.runExtractionWindow({ targets: queued.map(task => ({ task_id: task.id, expected_version: task.version })),
      context_ids, worker_id: this.workerId, lease_ms: LEASE_MS }, this.context);
    for (const attempt of attempts) if (attempt.reason === "input_too_large") this.singletons.add(`${generation.id}:${attempt.source_id}`);
    for (const claim of queued) settled = this.recordOutcome(`${generation.id}:${claim.source_id}`, await this.drive(claim, true)) || settled;
    return settled;
  }

  /** Same-session means the stored (source, session) key: two sources that reuse a session string never share a window or context. */
  private async contextIds(rows: WindowRow[]): Promise<string[]> {
    const first = rows[0];
    if (!first || first.session === null || this.contextMax === 0) return [];
    const preceding = await this.read<{ id: string }>(`MATCH (e:Element:Episode)
      WHERE e.session_key=$session AND e.ingest_seq < $before AND NOT e.id IN $targets
      RETURN e.id AS id ORDER BY e.ingest_seq DESC LIMIT toInteger($limit)`,
    { session: first.session, before: first.seq, targets: rows.map(row => row.id), limit: this.contextMax });
    return preceding.map(row => row.id).reverse();
  }

  private async skipWindow(tasks: ModelTask[]): Promise<boolean> {
    const gate = this.gate;
    if (!gate) return false;
    let scores: number[];
    try {
      const episodes = await this.engine.store.extractionContextInput(tasks.map(task => task.source_id), this.context);
      scores = await gate.score(episodes);
      if (episodes.length !== tasks.length || scores.length !== tasks.length || scores.some(score => !Number.isFinite(score) || score < 0 || score > 1))
        throw new ExtractionGateError("gate_mismatch");
    } catch (error) { this.recordError(error); return false; }
    const window_score = Math.max(...scores);
    if (window_score >= this.gateThreshold) return false;
    const decided_at = this.clock();
    for (const [index, task] of tasks.entries()) {
      const score = scores[index];
      if (score === undefined) throw new ExtractionGateError("gate_mismatch");
      await this.engine.store.cancelModelTask({ task_id: task.id, expected_version: task.version, reason: "gate_skipped",
        gate: { model: gate.model, score, window_score, threshold: this.gateThreshold, decided_at } }, this.context);
      this.outcomes.set(`${task.generation_id}:${task.source_id}`, "failed");
      this.gated_total++;
    }
    return true;
  }

  /** Runs one Episode's pipeline to a terminal outcome. `fresh` is false when the outcome was already
   * persisted (adopted after a restart); null leaves unresolved work for a later wake. */
  private async drive(claim: ModelTask, fresh: boolean): Promise<Driven> {
    let pipeline = await this.engine.store.readExtractionPipeline(claim.id, this.context);
    // Every stage is attempt-bounded: claim and judge each need at most one run plus one retry per attempt and a settle,
    // retry and run per lost lease; the relation stage at most one run per failure plus the seal. A pipeline that is
    // still open past this bound is a bug.
    const steps = this.maxAttempts * 5 + this.maxLostLeases * 6 + 2;
    for (let step = 0; step < steps; step++) {
      if (pipeline.state === "unknown" || this.closed) return null;
      // Queued claims always return to the window scan, including retries after backoff or a lost lease.
      if (this.queueClaimWindow(pipeline)) return null;
      const action = this.classify(pipeline);
      if (action === "completed" || action === "failed") return { outcome: action, fresh };
      // A live lease belongs to another writer or a previous incarnation: nothing settles it now, its expiry re-arms the lane.
      if (action === "unresolved") { this.deferWake(Math.min(...[pipeline.claim, pipeline.judge].map(task => task?.state === "leased" && task.lease ? task.lease.expires_at : Infinity))); return null; }
      // Validated claims still owe relation verdicts. Every run asks the provider once per pending premise; a premise
      // that has failed maxAttempts times seals the source as a terminal omission (D53) instead of parking the pipeline.
      if (action === "pending" && await this.engine.store.factRelationFailures(claim.id, this.context) >= this.maxAttempts) {
        await this.engine.store.sealFactRelationOmission({ pipeline_id: claim.id, min_failures: this.maxAttempts }, this.context);
        pipeline = await this.engine.store.readExtractionPipeline(claim.id, this.context);
        fresh = true;
        continue;
      }
      fresh = true;
      if (action === "run" || action === "pending") {
        pipeline = await this.engine.runExtractionPipeline({ task_id: pipeline.claim.id, expected_version: pipeline.claim.version, worker_id: this.workerId, lease_ms: LEASE_MS }, this.context);
        continue;
      }
      if (action.kind === "retry") await this.engine.store.retryModelTask({ task_id: action.task.id, expected_version: action.task.version }, this.context);
      else if (action.kind === "settle") await this.settle(action.task);
      // An expired or lost lease is not an outcome the store will cover; with the budget spent it is cancelled into one.
      else await this.engine.store.cancelModelTask({ task_id: action.task.id, expected_version: action.task.version }, this.context);
      pipeline = await this.engine.store.readExtractionPipeline(claim.id, this.context);
    }
    // The last step may itself have settled the pipeline; classify it before judging the budget.
    if (pipeline.state === "unknown") return null;
    const final = this.classify(pipeline);
    if (final === "completed" || final === "failed") return { outcome: final, fresh };
    // Unreachable by construction; the pipeline stays retryable for the next wake rather than being reported as an outcome it never reached.
    this.recordError(new Error(`pipeline ${claim.id} did not settle within ${steps} steps`));
    return null;
  }

  private queueClaimWindow(pipeline: Known): boolean {
    const key = `${pipeline.claim.generation_id}:${pipeline.claim.source_id}`;
    if (pipeline.claim_attempt?.reason === "input_too_large") this.singletons.add(key);
    if (pipeline.claim.state !== "queued") return false;
    this.rewindow.add(key);
    return true;
  }

  private deferWake(at: number): void {
    if (this.closed || !Number.isFinite(at) || (this.deferred && this.deferred.at <= at)) return;
    if (this.deferred) clearTimeout(this.deferred.timer);
    const timer = setTimeout(() => { this.deferred = undefined; if (!this.closed) this.wake(); }, Math.max(0, at - this.clock()) + 1);
    timer.unref?.();
    this.deferred = { at, timer };
  }

  /** A failed attempt retries only once its backoff has elapsed; until then the lane defers its wake to that moment. */
  private retryAfterBackoff(task: ModelTask): Action {
    const at = task.updated_at + modelTaskRetryDelayMs(task.attempts);
    if (at <= this.clock()) return { kind: "retry", task };
    this.deferWake(at);
    return "unresolved";
  }

  private classify(pipeline: Known): Action {
    const stage = (task: ModelTask): "ok" | Action => {
      switch (task.state) {
        case "succeeded": return "ok";
        case "queued": return "run";
        case "leased": return task.lease && task.lease.expires_at <= this.clock() ? { kind: "settle", task } : "unresolved";
        case "cancelled": case "failed": return task.attempts < this.maxAttempts && task.state === "failed" ? this.retryAfterBackoff(task) : "failed";
        // expired / worker_lost: no provider outcome was received, so the attempt budget is untouched; retry while the
        // lost-lease budget lasts, else cancel into a durable omission.
        default: return (task.lost_leases ?? 0) < this.maxLostLeases ? { kind: "retry", task } : { kind: "cancel", task };
      }
    };
    const claim = stage(pipeline.claim);
    if (claim !== "ok") return claim;
    if (!pipeline.judge) return "run";
    const judge = stage(pipeline.judge);
    if (judge !== "ok") return judge;
    if (pipeline.relation_judge === "pending") return "pending";
    return pipeline.relation_judge === "omitted" ? "failed" : "completed";
  }

  /** A lease left by a lost writer is settled as worker_lost; our own expired lease as expired. */
  private async settle(task: ModelTask): Promise<void> {
    const request = { task_id: task.id, expected_version: task.version, lease_epoch: task.lease!.epoch };
    try { await this.engine.store.settleModelTask({ ...request, reason: "worker_lost" }, this.context); }
    catch { await this.engine.store.settleModelTask({ ...request, reason: "expired" }, this.context); }
  }
}
