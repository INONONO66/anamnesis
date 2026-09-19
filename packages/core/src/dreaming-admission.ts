import { createHash } from "node:crypto";
import { readFileSync, renameSync, writeFileSync, mkdirSync } from "node:fs";
import { canonicalReceiptJson } from "./receipt-digest.ts";

export type DreamPhase = "community" | "synthesis" | "profile";
export type DreamFence = {
  extraction_generation: number;
  covered_ingest_seq: number;
  structure_revision: number;
  policy_revision: number;
};
export type DreamSourceReceipt = {
  id: string;
  revision: string;
  body_digest: string;
  ingest_seq: number;
  allowed: boolean;
};
export type DreamAdmissionInput = DreamFence & { phase: DreamPhase; source_ids: string[] };
export type DreamLease = { worker_id: string; epoch: string; expires_at: number };
export type DreamJob = DreamAdmissionInput & {
  job_id: string;
  source_receipts: DreamSourceReceipt[];
  state: "queued" | "leased";
  version: number;
  lease: DreamLease | null;
  semantic_writes: false;
  authority: "none";
};

export class DreamAdmissionError extends Error {
  constructor(readonly code: string) { super(code); this.name = "DreamAdmissionError"; }
}

export type DreamJobStore = { get(id: string): DreamJob | null; put(job: DreamJob): void };
export type DreamExecutionReview = { operator: string; independent_review: true; policy_checked: true; generation_checked: true; coverage_checked: true };
export type DreamExport = { operation_id: string; digest: string; bytes: string; source_receipts: DreamSourceReceipt[] };
export type LeidenSpec = { image_digest: string; gds_version: "2.13.12"; algorithm: "leiden"; network: "none"; max_iterations: number; tolerance: number };
export type LeidenResult = { export_digest: string; artifact_digest: string; assignments: Array<{ element_id: string; community_assignment: number }>; spec: LeidenSpec };
export type DreamPublisher = (job: DreamJob, result: LeidenResult) => void;
export type DreamExecutor = { export(input: DreamExport): Promise<LeidenResult> };

type Options = {
  now: () => number;
  readFence: () => DreamFence;
  readSource: (id: string) => DreamSourceReceipt | null;
  store?: DreamJobStore;
};

class MemoryDreamJobStore implements DreamJobStore {
  private jobs = new Map<string, DreamJob>();
  get(id: string) { return this.jobs.get(id) ?? null; }
  put(job: DreamJob) { this.jobs.set(job.job_id, structuredClone(job)); }
}

/** Atomic JSON store used by the daemon; rename makes restart recovery bounded and durable. */
export class FileDreamJobStore implements DreamJobStore {
  constructor(private readonly path: string) { mkdirSync(path.replace(/\\/g, "/").split("/").slice(0, -1).join("/") || ".", { recursive: true }); }
  get(id: string) { try { const all = JSON.parse(readFileSync(this.path, "utf8")) as DreamJob[]; return structuredClone(all.find(j => j.job_id === id) ?? null); } catch { return null; } }
  put(job: DreamJob) { let all: DreamJob[] = []; try { all = JSON.parse(readFileSync(this.path, "utf8")); } catch {} const i = all.findIndex(j => j.job_id === job.job_id); if (i < 0) all.push(job); else all[i] = job; const tmp = `${this.path}.tmp`; writeFileSync(tmp, JSON.stringify(all)); renameSync(tmp, this.path); }
}
type LeaseInput = { job_id: string; expected_version: number; worker_id: string; lease_ms: number };
type ExpireInput = { job_id: string; expected_version: number; lease_epoch: string };

const MAX_SOURCES = 256;
const MAX_LEASE_MS = 30_000;
const hex = /^[0-9a-f]{64}$/;
const stable = (value: unknown): string => canonicalReceiptJson(value);

/**
 * The deferred boundary only records an auditable, fenced work item. It has no
 * provider, model, graph, Fact, or commit API by construction.
 */
export class DreamAdmission {
  private readonly store: DreamJobStore;
  constructor(private readonly options: Options) { this.store = options.store ?? new MemoryDreamJobStore(); }

  admit(input: DreamAdmissionInput): DreamJob {
    this.validateInput(input);
    this.assertFence(input);
    const receipts = input.source_ids.map(id => {
      const receipt = this.options.readSource(id);
      if (!receipt) throw new DreamAdmissionError("dream_source_missing");
      if (!receipt.allowed) throw new DreamAdmissionError("dream_source_denied");
      if (!hex.test(receipt.revision) || !hex.test(receipt.body_digest) || receipt.ingest_seq > input.covered_ingest_seq)
        throw new DreamAdmissionError("dream_source_stale");
      return { ...receipt };
    });
    const identity = { ...input, phase: input.phase, source_ids: [...input.source_ids].sort(), source_receipts: receipts };
    const job_id = `dream-${createHash("sha256").update(stable(identity)).digest("hex")}`;
    const existing = this.store.get(job_id);
    if (existing) return this.copy(existing);
    const job: DreamJob = { ...input, source_ids: [...input.source_ids], source_receipts: receipts, job_id, state: "queued", version: 0, lease: null, semantic_writes: false, authority: "none" };
    this.store.put(job);
    return this.copy(job);
  }

  lease(input: LeaseInput): DreamJob {
    if (!Number.isSafeInteger(input.expected_version) || input.lease_ms < 1 || input.lease_ms > MAX_LEASE_MS) throw new DreamAdmissionError("dream_lease_invalid");
    const job = this.require(input.job_id);
    this.cas(job, input.expected_version);
    if (job.state === "leased" && job.lease && this.options.now() >= job.lease.expires_at) throw new DreamAdmissionError("dream_lease_expired");
    if (job.state !== "queued") throw new DreamAdmissionError("dream_not_queued");
    job.state = "leased";
    job.version++;
    job.lease = { worker_id: input.worker_id, epoch: `${job.job_id}:${job.version}`, expires_at: this.options.now() + input.lease_ms };
    this.store.put(job);
    return this.copy(job);
  }

  expire(input: ExpireInput): DreamJob {
    const job = this.require(input.job_id);
    this.cas(job, input.expected_version);
    if (job.state !== "leased" || !job.lease || job.lease.epoch !== input.lease_epoch) throw new DreamAdmissionError("dream_lease_fenced");
    if (this.options.now() < job.lease.expires_at) throw new DreamAdmissionError("dream_lease_live");
    job.state = "queued"; job.lease = null; job.version++;
    this.store.put(job);
    return this.copy(job);
  }

  async execute(jobId: string, review: DreamExecutionReview, executor: DreamExecutor, publish: DreamPublisher): Promise<DreamExport> {
    const job = this.require(jobId);
    if (review.operator !== "trusted-runtime" || review.independent_review !== true || review.policy_checked !== true || review.generation_checked !== true || review.coverage_checked !== true) throw new DreamAdmissionError("dream_review_required");
    if (job.state !== "leased" || !job.lease || this.options.now() >= job.lease.expires_at) throw new DreamAdmissionError("dream_lease_fenced");
    this.assertFence(job);
    const current = job.source_ids.map(id => this.options.readSource(id));
    if (current.some((r, i) => !r || canonicalReceiptJson(r) !== canonicalReceiptJson(job.source_receipts[i]))) throw new DreamAdmissionError("dream_source_changed");
    const bytes = canonicalReceiptJson({ phase: job.phase, fence: { extraction_generation: job.extraction_generation, covered_ingest_seq: job.covered_ingest_seq, structure_revision: job.structure_revision, policy_revision: job.policy_revision }, source_receipts: job.source_receipts });
    const exported: DreamExport = { operation_id: job.job_id, digest: sha(bytes), bytes, source_receipts: structuredClone(job.source_receipts) };
    return executor.export(exported).then(result => {
      const expected = sha(canonicalReceiptJson({ export_digest: result.export_digest, assignments: result.assignments, spec: result.spec }));
      if (result.export_digest !== exported.digest || result.spec.network !== "none" || result.spec.gds_version !== "2.13.12" || result.spec.algorithm !== "leiden" || !result.spec.image_digest.startsWith("sha256:") || result.artifact_digest !== expected) throw new DreamAdmissionError("dream_result_unverified");
      publish(this.copy(job), structuredClone(result));
      return exported;
    });
  }

  complete(_jobId: string, result: { semantic_writes: boolean }): never {
    if (result.semantic_writes) throw new DreamAdmissionError("dream_semantic_write_forbidden");
    throw new DreamAdmissionError("dream_completion_unimplemented");
  }

  private assertFence(input: DreamAdmissionInput) {
    const current = this.options.readFence();
    if (JSON.stringify(current) !== JSON.stringify({ extraction_generation: input.extraction_generation, covered_ingest_seq: input.covered_ingest_seq, structure_revision: input.structure_revision, policy_revision: input.policy_revision })) throw new DreamAdmissionError("dream_fence_stale");
  }
  private validateInput(input: DreamAdmissionInput) {
    if (!Number.isSafeInteger(input.extraction_generation) || !Number.isSafeInteger(input.covered_ingest_seq) || input.source_ids.length > MAX_SOURCES || input.source_ids.length === 0 || new Set(input.source_ids).size !== input.source_ids.length) throw new DreamAdmissionError("dream_input_invalid");
  }
  private require(id: string) { const job = this.store.get(id); if (!job) throw new DreamAdmissionError("dream_job_missing"); return job; }
  private cas(job: DreamJob, expected: number) { if (job.version !== expected) throw new DreamAdmissionError("dream_version_conflict"); }
  private copy(job: DreamJob): DreamJob { return structuredClone(job); }
}

const sha = (value: string) => createHash("sha256").update(value).digest("hex");
