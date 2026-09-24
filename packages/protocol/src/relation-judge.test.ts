import { expect, test } from "bun:test";
import { ExtractionModelOutput, ExtractionOutput, canonicalExtractionBody, extractionBodyDigest } from "./extraction.ts";
import { ExtractionPipeline, FactRelationContext } from "./extraction-audit.ts";
import { FactRelationDecision, MaterializationResult } from "./materialization.ts";

const id = "01900000-0000-7000-8000-000000000001", other = "01900000-0000-7000-8000-000000000002";
const digest = "a".repeat(64);
const judgement = { candidate_id: other, relation: "invalidates" as const, confidence: 0.9, reason: "later statement replaces the earlier preference" };
const output = { task: "judge_relations" as const, relation_context_digest: digest, judgements: [judgement], language: "en", modality: "text" as const };

test("judge_relations output is a bounded strict per-candidate verdict list", () => {
  expect(ExtractionModelOutput.parse(output)).toEqual(output);
  for (const body of [
    { ...output, judgements: [{ ...judgement, relation: "supersedes" }] },
    { ...output, judgements: [{ ...judgement, confidence: 1.5 }] },
    { ...output, judgements: [{ ...judgement, reason: "" }] },
    { ...output, judgements: [{ ...judgement, target_ids: [id] }] },
    { ...output, judgements: Array(17).fill(judgement) },
    { ...output, relation_context_digest: "short" },
    { ...output, spans: [] },
  ]) expect(ExtractionModelOutput.safeParse(body).success).toBe(false);
  expect(ExtractionModelOutput.parse({ ...output, judgements: [] }).task).toBe("judge_relations");
});

test("judge_relations attempts are span-free outputs", () => {
  const canonical_body = canonicalExtractionBody(output);
  expect(ExtractionOutput.safeParse({ canonical_body, body_digest: extractionBodyDigest(output), spans: [], language: "en", modality: "text" }).success).toBe(true);
  expect(ExtractionOutput.safeParse({ canonical_body, body_digest: extractionBodyDigest(output), spans: [{ start: 0, end: 1, text: "x" }], language: "en", modality: "text" }).success).toBe(false);
});

test("relation context carries the new fact, bounded candidates and its own digest", () => {
  const context = { body_digest: digest, fact: { text: "Alice prefers light mode", time: { value: "2026-09-02T00:00:00.000Z", precision: "day" as const } },
    candidates: [{ id: other, text: "Alice prefers dark mode", time: { value: "2026-09-01T00:00:00.000Z", precision: "day" as const } }] };
  expect(FactRelationContext.parse(context)).toEqual(context);
  expect(FactRelationContext.safeParse({ ...context, candidates: Array(17).fill(context.candidates[0]) }).success).toBe(false);
  expect(FactRelationContext.safeParse({ ...context, candidates: [{ ...context.candidates[0], source_episode_id: id }] }).success).toBe(false);
  expect(FactRelationContext.safeParse({ ...context, fact: { text: "" , time: context.fact.time } }).success).toBe(false);
});

test("materialization results record relation decisions and duplicates without widening the base shape", () => {
  const decision = { candidate_id: other, relation: "contrasts" as const, confidence: 0.7, reason: "conflicting preferences", outcome: "linked" as const, link_id: id };
  expect(FactRelationDecision.parse(decision)).toEqual(decision);
  expect(FactRelationDecision.safeParse({ ...decision, outcome: "merged" }).success).toBe(false);
  expect(MaterializationResult.parse({ created: true, fact_id: id, link_id: id })).toEqual({ created: true, fact_id: id, link_id: id });
  expect(MaterializationResult.parse({ created: true, fact_id: id, link_id: id, relations: [decision] }).relations).toHaveLength(1);
  expect(MaterializationResult.safeParse({ created: true, fact_id: id, link_id: id, relations: Array(17).fill(decision) }).success).toBe(false);
  expect(MaterializationResult.safeParse({ created: true, fact_id: id, link_id: id, duplicate_of: other }).success).toBe(true);
});

test("known pipelines expose the optional relation judge stage", () => {
  const stage = ExtractionPipeline.options[1].shape.relation_judge;
  for (const value of ["disabled", "pending", "complete", undefined]) expect(stage.safeParse(value).success).toBe(true);
  expect(stage.safeParse("done").success).toBe(false);
});
