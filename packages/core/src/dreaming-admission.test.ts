import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { DreamAdmission, DreamAdmissionError, FileDreamJobStore } from "./dreaming-admission.ts";
import { canonicalReceiptJson } from "./receipt-digest.ts";

const hash = (c: string) => c.repeat(64);
const source = { id: "source-1", revision: hash("a"), body_digest: hash("b"), ingest_seq: 4 };
const pin = { extraction_generation: 7, covered_ingest_seq: 4, structure_revision: 11, policy_revision: 3 };

function boundary() {
  return new DreamAdmission({
    now: () => 100,
    readFence: () => pin,
    readSource: id => id === source.id ? { ...source, allowed: true } : null,
  });
}

test("dream admission creates an idempotent deferred job with source custody and no semantic authority", () => {
  const d = boundary();
  const input = { phase: "community" as const, source_ids: [source.id], ...pin };
  const first = d.admit(input);
  const second = d.admit(input);
  expect(second).toEqual(first);
  expect(first.job_id).toMatch(/^dream-[0-9a-f]{64}$/);
  expect(first.state).toBe("queued");
  expect(first.source_receipts).toEqual([{ ...source, allowed: true }]);
  expect(first.semantic_writes).toBe(false);
  expect(first.authority).toBe("none");
});

test("nested custody revision and digest changes produce distinct jobs and receipts", () => {
  let current = { ...source, allowed: true };
  const d = new DreamAdmission({ now: () => 100, readFence: () => pin, readSource: id => id === source.id ? { ...current } : null });
  const input = { phase: "community" as const, source_ids: [source.id], ...pin };
  const first = d.admit(input);
  current = { ...current, revision: hash("c"), body_digest: hash("d") };
  const changed = d.admit(input);
  expect(changed.job_id).not.toBe(first.job_id);
  expect(changed.source_receipts).toEqual([{ ...current }]);
  expect(changed.source_receipts[0]).not.toEqual(first.source_receipts[0]);
});

test("equivalent nested receipt insertion ordering and identical retry remain stable", () => {
  let reordered = false;
  const d = new DreamAdmission({
    now: () => 100,
    readFence: () => pin,
    readSource: id => id === source.id
      ? reordered
        ? { allowed: true, ingest_seq: source.ingest_seq, body_digest: source.body_digest, revision: source.revision, id: source.id }
        : { id: source.id, revision: source.revision, body_digest: source.body_digest, ingest_seq: source.ingest_seq, allowed: true }
      : null,
  });
  const input = { phase: "community" as const, source_ids: [source.id], ...pin };
  const first = d.admit(input);
  reordered = true;
  const same = d.admit(input);
  expect(same).toEqual(first);
  expect(d.admit(input)).toEqual(first);
});

test("stale, missing, and denied custody are refused before queue mutation", () => {
  expect(() => boundary().admit({ phase: "community", source_ids: [source.id], ...pin, policy_revision: 4 })).toThrow("dream_fence_stale");
  expect(() => boundary().admit({ phase: "community", source_ids: ["missing"], ...pin })).toThrow("dream_source_missing");
  const denied = new DreamAdmission({ now: () => 100, readFence: () => pin, readSource: () => ({ ...source, allowed: false }) });
  expect(() => denied.admit({ phase: "community", source_ids: [source.id], ...pin })).toThrow("dream_source_denied");
});

test("lease expiry is explicit and retry is CAS fenced without sleeping", () => {
  let now = 100;
  const d = new DreamAdmission({ now: () => now, readFence: () => pin, readSource: id => id === source.id ? { ...source, allowed: true } : null });
  const job = d.admit({ phase: "community", source_ids: [source.id], ...pin });
  const leased = d.lease({ job_id: job.job_id, expected_version: 0, worker_id: "worker", lease_ms: 10 });
  now = 111;
  expect(() => d.lease({ job_id: job.job_id, expected_version: leased.version, worker_id: "worker-2", lease_ms: 10 })).toThrow("dream_lease_expired");
  const retried = d.expire({ job_id: job.job_id, expected_version: leased.version, lease_epoch: leased.lease!.epoch });
  expect(retried.state).toBe("queued");
  expect(d.lease({ job_id: job.job_id, expected_version: retried.version, worker_id: "worker-2", lease_ms: 10 }).state).toBe("leased");
});

test("persistent leased job executes only after trusted review and verifies pinned artifact", async () => {
  const dir = mkdtempSync(`${tmpdir()}/dream-`), path = `${dir}/jobs.json`;
  try {
    let now = 100, current = { ...source, allowed: true };
    const store = new FileDreamJobStore(path);
    const d = new DreamAdmission({ now: () => now, store, readFence: () => pin, readSource: id => id === source.id ? current : null });
    const job = d.admit({ phase: "community", source_ids: [source.id], ...pin });
    const leased = d.lease({ job_id: job.job_id, expected_version: 0, worker_id: "trusted-runtime", lease_ms: 100 });
    const restarted = new DreamAdmission({ now: () => now, store, readFence: () => pin, readSource: id => id === source.id ? current : null });
    const assignments = [{ element_id: source.id, community_assignment: 1 }];
    const spec = { image_digest: "sha256:" + "a".repeat(64), gds_version: "2.13.12" as const, algorithm: "leiden" as const, network: "none" as const, max_iterations: 1000, tolerance: 1e-9 };
    let published = 0;
    await expect(restarted.execute(leased.job_id, { operator: "caller", independent_review: true, policy_checked: true, generation_checked: true, coverage_checked: true }, { export: async () => { throw new Error("must not run"); } }, () => { published++; })).rejects.toThrow("dream_review_required");
    await restarted.execute(leased.job_id, { operator: "trusted-runtime", independent_review: true, policy_checked: true, generation_checked: true, coverage_checked: true }, { export: async e => { const exportDigest = e.digest; const artifactDigest = createHash("sha256").update(canonicalReceiptJson({ export_digest: exportDigest, assignments, spec })).digest("hex"); return { export_digest: exportDigest, artifact_digest: artifactDigest, assignments, spec }; } }, () => { published++; });
    expect(published).toBe(1);
    current = { ...current, body_digest: hash("c") };
    await expect(restarted.execute(leased.job_id, { operator: "trusted-runtime", independent_review: true, policy_checked: true, generation_checked: true, coverage_checked: true }, { export: async () => { throw new Error("must not run"); } }, () => { published++; })).rejects.toThrow("dream_source_changed");
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("completion is not an admission capability", () => {
  const d = boundary();
  const job = d.admit({ phase: "community", source_ids: [source.id], ...pin });
  expect(() => d.complete(job.job_id, { semantic_writes: true })).toThrow(DreamAdmissionError);
});
