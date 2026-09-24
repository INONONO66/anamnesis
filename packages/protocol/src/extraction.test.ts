import { describe, expect, test } from "bun:test";
import { ExtractionAttempt, Generation, LeaseModelTask, ModelTask, canonicalExtractionBody, extractionBodyDigest } from "./extraction.ts";

describe("G004 extraction contract", () => {
  test("canonicalizes JSON domain with UTF-16 ordering and rejects invalid values", () => {
    expect(canonicalExtractionBody({ "\uffff": 1, "𐀀": 2 })).toBe('{"𐀀":2,"￿":1}');
    expect(() => canonicalExtractionBody({ x: undefined })).toThrow();
    expect(() => canonicalExtractionBody(NaN)).toThrow();
    expect(() => canonicalExtractionBody([,])).toThrow();
    expect(() => canonicalExtractionBody("\ud800")).toThrow();
  });
  test("parses a content-free terminal attempt record", () => {
    const attempt = ExtractionAttempt.parse({
      id: "018f5b5e-7b1e-7abc-8def-123456789012", generation_id: "018f5b5e-7b1e-7abc-8def-123456789013",
      task_id: "018f5b5e-7b1e-7abc-8def-123456789015", source_ingest_seq: 1,
      source_id: "018f5b5e-7b1e-7abc-8def-123456789014", source_revision: "b".repeat(64), body_digest: "a".repeat(64),
      state: "cancelled", reason: "cancelled", disposition: null, created_at: 1, updated_at: 1, lease: null,
      output: null, policy_context: { revision: 1, authority: "installation" }, spans: [],
    });
    expect(attempt.id).toMatch(/^018/);
    expect(canonicalExtractionBody({ claim: "x" })).toBe('{"claim":"x"}');
    // A mismatch names the check it failed (#218); records written before the field exists still parse.
    const mismatch = { ...attempt, state: "failed", reason: "provider_mismatch" };
    expect(ExtractionAttempt.parse(mismatch).detail).toBeUndefined();
    expect(ExtractionAttempt.parse({ ...mismatch, detail: "incarnation" }).detail).toBe("incarnation");
    expect(() => ExtractionAttempt.parse({ ...mismatch, detail: "other" })).toThrow();
    // Only an accepted answer can carry the upstream-reported model; a failure has no answer to attribute.
    expect(() => ExtractionAttempt.parse({ ...mismatch, reported_model: "gpt-test-2026-01-01:fp" })).toThrow();
    expect(() => ExtractionAttempt.parse({ ...attempt, reported_model: "" })).toThrow();
  });

  test("a succeeded attempt records the upstream-reported model beside the configured task identity", () => {
    const body = canonicalExtractionBody({ task: "claim", claims: [], language: "en", modality: "text" });
    const success = {
      id: "018f5b5e-7b1e-7abc-8def-123456789012", generation_id: "018f5b5e-7b1e-7abc-8def-123456789013",
      task_id: "018f5b5e-7b1e-7abc-8def-123456789015", source_ingest_seq: 1,
      source_id: "018f5b5e-7b1e-7abc-8def-123456789014", source_revision: "b".repeat(64), body_digest: "a".repeat(64),
      state: "succeeded", reason: null, disposition: "unknown", created_at: 1, updated_at: 1,
      lease: { worker_id: "w", epoch: "018f5b5e-7b1e-7abc-8def-123456789016", writer_epoch: 1, expires_at: 2 },
      output: { canonical_body: body, body_digest: extractionBodyDigest(JSON.parse(body)), spans: [], language: "en", modality: "text" },
      policy_context: { revision: 1, authority: "installation" }, spans: [],
    };
    expect(ExtractionAttempt.parse(success).reported_model).toBeUndefined();
    expect(ExtractionAttempt.parse({ ...success, reported_model: "claude-haiku-4-5-20251001" }).reported_model).toBe("claude-haiku-4-5-20251001");
  });

  test("a task counts lost leases apart from its attempt budget; older records carry no counter", () => {
    const task = {
      id: "018f5b5e-7b1e-7abc-8def-123456789012", generation_id: "018f5b5e-7b1e-7abc-8def-123456789013", source_id: "018f5b5e-7b1e-7abc-8def-123456789014",
      source_revision: "b".repeat(64), body_digest: "a".repeat(64), source_ingest_seq: 1, attempt_id: "018f5b5e-7b1e-7abc-8def-123456789015",
      kind: "claim", model: "fixture", model_incarnation: "a".repeat(64), state: "worker_lost", lease: null, policy_context: null,
      version: 3, attempts: 0, created_at: 1, updated_at: 2,
    };
    expect(ModelTask.parse(task).lost_leases).toBeUndefined();
    expect(ModelTask.parse({ ...task, lost_leases: 1 }).lost_leases).toBe(1);
    expect(() => ModelTask.parse({ ...task, lost_leases: -1 })).toThrow();
  });

  test("rejects incomplete task and generation records", () => {
    expect(() => ModelTask.parse({ id: "bad" })).toThrow();
    expect(() => Generation.parse({ id: "bad" })).toThrow();
  });

  test("a task lease may outlast one 30 s provider call with margin, but stays bounded", () => {
    // The lease is taken before the provider HTTP call and checked when the attempt is recorded; a lease equal to the
    // provider timeout turns every slow-but-successful call into lease_expired and burns an attempt.
    const base = { task_id: "01a0d34d-e781-70be-bf5f-c82ca4b53849", expected_version: 0, worker_id: "scheduler" };
    expect(LeaseModelTask.parse({ ...base, lease_ms: 90000 }).lease_ms).toBe(90000);
    expect(LeaseModelTask.parse({ ...base, lease_ms: 120000 }).lease_ms).toBe(120000);
    expect(() => LeaseModelTask.parse({ ...base, lease_ms: 120001 })).toThrow();
    expect(() => LeaseModelTask.parse({ ...base, lease_ms: 0 })).toThrow();
  });
});
