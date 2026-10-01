import { expect, test } from "bun:test";
import { extractionBodyDigest, semanticReviewClaimBody, type SemanticClaim, type SemanticResolution, type SemanticReviewPremises } from "./index.ts";

const uuid = (n: number) => `018f5b5e-7b1e-7abc-8def-${String(n).padStart(12, "0")}`;
const time = { time_value: "2026-09-10T12:00:00Z", time_utc: Date.parse("2026-09-10T12:00:00Z"), time_precision: "instant" as const };
const lineage = { episode_id: uuid(1), lineage_mode: "direct" as const, parent_recall_ids: [], context_digests: [], root_episode_ids: [uuid(1)], echo_depth: 0, complete: true };
const source: SemanticReviewPremises["source"] = {
  id: uuid(1), schema: "anamnesis.original-message/1", revision_key: "a".repeat(64), content_digest: "b".repeat(64),
  content: "Alice moved to Busan.", ingest_seq: 7, time, speaker: { origin_source: "chat", origin_actor: "Alice" },
  provenance: { episode_digest_version: 2, origin_role: "user", lineage, lineage_digest: extractionBodyDigest(lineage) },
};
const resolution: SemanticResolution = { entity_resolutions: [{ status: "existing", mention: "Alice", entity_id: uuid(2) }],
  attribution_speakers: [], allow_no_single_locus: true, content_language: "en" };
const claim = (evidence: SemanticClaim["evidence"]): SemanticClaim => ({
  content: "Alice moved to Busan.", content_language: "en", sub_kind: "event", modality: "asserted", confidence: 0.8,
  time: { ...time, time_precision: "inherited", resolution: "inherited", anchor_time_utc: time.time_utc },
  entities: [{ mention: "Alice", entity_id: uuid(2) }], subject_keys: [uuid(2)], predicate_text: "moved to",
  scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
  scope_complete: true, evidence,
});

test("the review claim body carries full candidate custody, with evidence flattened per locus kind", () => {
  const quoted = semanticReviewClaimBody(claim({ kind: "source_locus", quote: "Alice moved to Busan.", span: { start: 0, end: 21 } }), source, resolution);
  expect(quoted).toMatchObject({ content: "Alice moved to Busan.", sub_kind: "event", modality: "asserted", confidence: 0.8,
    evidence_quote: "Alice moved to Busan.", evidence_kind: null, span: [0, 21], time_value: time.time_value, time_utc: time.time_utc,
    time_precision: "inherited", entities: resolution.entity_resolutions, speaker: source.speaker, subject_keys: [uuid(2)],
    predicate_text: "moved to", scope_complete: true, corrects_local_claim_index: null, correction_scope_text: null, mode: null });
  expect(Object.keys(quoted).sort()).toEqual(["confidence", "content", "content_language", "corrects_local_claim_index", "correction_scope_text",
    "entities", "evidence_kind", "evidence_quote", "modality", "mode", "predicate_text", "scope", "scope_complete", "span", "speaker",
    "sub_kind", "subject_keys", "time_precision", "time_utc", "time_value"].sort());
  const spanOnly = semanticReviewClaimBody(claim({ kind: "source_locus", span: { start: 6, end: 11 } }), source, resolution);
  expect([spanOnly.evidence_quote, spanOnly.evidence_kind, spanOnly.span]).toEqual([null, null, [6, 11]]);
  const noLocus = semanticReviewClaimBody(claim({ kind: "no_single_locus" }), source, resolution);
  expect([noLocus.evidence_quote, noLocus.evidence_kind, noLocus.span]).toEqual([null, "no_single_locus", null]);
  expect(extractionBodyDigest(quoted)).not.toBe(extractionBodyDigest(spanOnly));
});
