import { expect, test } from "bun:test";
import { Generation, ModelTask, canonicalExtractionBody } from "./extraction.ts";
import { protocolJsonSchemas } from "../scripts/export-schemas.ts";
const id = "018f5b5e-7b1e-7abc-8def-123456789012";
const generation = { id, stream: "extraction", incarnation: "a".repeat(64), state: "active", covered_ingest_seq: 0, created_at: 2, updated_at: 1 };
test("generation timestamps cannot run backwards", () => {
  expect(Generation.safeParse(generation).success).toBe(false);
});
test("a leased model task requires a lease", () => {
  const task = { id, generation_id: id, source_id: id, source_revision: "a".repeat(64), body_digest: "b".repeat(64), source_ingest_seq: 1,
    attempt_id: id, kind: "claim", model: "fixture", model_incarnation: "a".repeat(64), state: "leased", policy_context: { revision: 0, authority: "installation" }, version: 1, attempts: 1, created_at: 1, updated_at: 1 };
  const lease = { worker_id: "w", epoch: id, writer_epoch: 1, expires_at: 2 };
  expect(ModelTask.safeParse({ ...task, lease }).success).toBe(true);
  expect(ModelTask.safeParse({ ...task, lease: null }).success).toBe(false);
  expect(ModelTask.safeParse({ ...task, lease: { ...lease, expires_at: 1 } }).success).toBe(false);
  expect(ModelTask.safeParse({ ...task, state: "succeeded", lease }).success).toBe(false);
});
test("canonical keys use every UTF-16 code unit and validate key Unicode", () => {
  expect(canonicalExtractionBody({ "𐀁": 1, "𐀀": 2 })).toBe('{"𐀀":2,"𐀁":1}');
  expect(() => canonicalExtractionBody({ "\ud800": 1 })).toThrow();
  expect(() => canonicalExtractionBody(new Date())).toThrow();
  const array = [1]; Object.defineProperty(array, "0", { value: 1, enumerable: false }); array.length = 2; array[1] = 2;
  expect(() => canonicalExtractionBody(array)).toThrow();
});
test("exports structural extraction record and command schemas", () => {
  const schemas = protocolJsonSchemas();
  for (const name of ["extraction-generation", "extraction-attempt", "model-task", "extraction-coverage", "extraction-model-output", "extraction-complete", "extraction-coverage-advance"]) expect(Object.hasOwn(schemas, name)).toBe(true);
});
