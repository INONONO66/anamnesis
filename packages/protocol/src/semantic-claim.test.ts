import { createHash } from "node:crypto";
import { describe, expect, test } from "bun:test";
import {
  ClaimSubKind, ExtractionModelOutput, SemanticClaim, SemanticClaimBatch, SemanticClaimModality,
  SemanticClaimValidationError, SemanticResolvedTime, SemanticSourceContext,
  canonicalExtractionBody, extractionBodyDigest, validateSemanticClaim,
} from "./index.ts";

const uuid = (n: number) => `018f5b5e-7b1e-7abc-8def-${String(n).padStart(12, "0")}`;
const episodeId = uuid(1), entityId = uuid(2), generation = uuid(3), rootId = uuid(4);
const sha = (s: string) => createHash("sha256").update(s).digest("hex");
const sourceTime = { time_value: "2026-09-10T12:00:00Z", time_utc: Date.parse("2026-09-10T12:00:00Z"), time_precision: "instant" as const };
const bob = { origin_source: "chat", origin_actor: "Bob" };
const bobKey = extractionBodyDigest(bob);
function context(content = "Alice moved to Busan."): SemanticSourceContext {
  const lineage = { episode_id: episodeId, lineage_mode: "direct" as const, parent_recall_ids: [], context_digests: [],
    root_episode_ids: [episodeId], echo_depth: 0, complete: true };
  return {
    generation, fact_language_policy: "source", allow_no_single_locus: false,
    episode: { id: episodeId, schema: "anamnesis.original-message/1", revision_key: "a".repeat(64),
      content_digest: sha(content), content, content_language: "en", ingest_seq: 7, time: { ...sourceTime },
      speaker: { origin_source: "chat", origin_actor: "Alice" },
      provenance: { episode_digest_version: 2, origin_role: "user", lineage, lineage_digest: extractionBodyDigest(lineage) } },
    entity_resolutions: [{ status: "existing", mention: "Alice", entity_id: entityId }], attribution_speakers: [bob],
  };
}
function claim(content = "Alice moved to Busan."): SemanticClaim {
  return {
    content, content_language: "en", sub_kind: "event", modality: "asserted", confidence: 0.8,
    time: { ...sourceTime, time_precision: "inherited", resolution: "inherited", anchor_time_utc: sourceTime.time_utc },
    entities: [{ mention: "Alice", entity_id: entityId }], subject_keys: [entityId], predicate_text: "moved to",
    scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
    scope_complete: true, evidence: { kind: "source_locus", quote: content },
  };
}
function reject(input: unknown, ctx: unknown, code: SemanticClaimValidationError["code"]) {
  try { validateSemanticClaim(input, ctx); throw new Error("unexpected admission"); }
  catch (e) { expect(e).toBeInstanceOf(SemanticClaimValidationError); expect((e as SemanticClaimValidationError).code).toBe(code); }
}
function receiptContext(complete = true): SemanticSourceContext {
  const ctx = context();
  const lineage = { episode_id: episodeId, lineage_mode: "receipts" as const, parent_recall_ids: [uuid(5)],
    context_digests: ["c".repeat(64)], root_episode_ids: [rootId], echo_depth: 2, complete };
  ctx.episode.provenance = { episode_digest_version: 2, origin_role: "assistant", lineage, lineage_digest: extractionBodyDigest(lineage) };
  return ctx;
}

describe("semantic extraction boundary, not audit disposition or Fact permission", () => {
  test("accepts source-faithful speech acts without promoting intent or possibility", () => {
    const fixtures = [
      ["asserted", "Alice moved to Busan."],
      ["reported", "Bob said Alice moved to Busan."],
      ["hedged", "Alice thinks she moved on Friday."],
      ["intended", "Alice plans to move to Busan."],
      ["hypothetical", "If Alice moves to Busan, she will rent."],
    ] as const;
    for (const [modality, content] of fixtures) {
      const c = claim(content); c.modality = modality;
      if (modality === "reported") c.scope.attribution_speaker_keys = [bobKey];
      if (modality === "hypothetical") c.scope.condition = "If Alice moves to Busan";
      const result = validateSemanticClaim(c, context(content));
      expect(result.identity.modality).toBe(modality);
      expect(result.evidence).toEqual({ start: 0, end: Buffer.byteLength(content), text: content });
      expect(result.confidence_basis).toBe("source_fidelity");
      expect(result.semantic_writes).toBe(false);
      expect(result.materialization_gate).toBe("independent_shadow_review_required");
    }
  });
  test("does not mistake a high confidence or exact source slice for truth/entailment", () => {
    const c = claim("Alice lives on Mars."); c.evidence = { kind: "source_locus", quote: "Alice moved to Busan." }; c.confidence = 1;
    const result = validateSemanticClaim(c, context());
    expect(result.mechanically_grounded).toBe(true);
    expect(result.semantic_writes).toBe(false);
    expect(result.identity.content).toBe(c.content);
  });
  test("rejects audit ABI, absent/unknown modalities, sub-kinds, confidence and arbitrary metadata", () => {
    expect(SemanticClaim).not.toBe(ExtractionModelOutput);
    for (const sub_kind of ClaimSubKind.options) expect(SemanticClaim.safeParse({ ...claim(), sub_kind }).success).toBe(true);
    expect(SemanticClaimModality.options).toEqual(["asserted", "reported", "hedged", "intended", "hypothetical"]);
    for (const patch of [
      { modality: "text" }, { modality: "unknown" }, { modality: undefined }, { sub_kind: "wish" },
      ...[NaN, Infinity, -Infinity, -0.01, 1.01].map(confidence => ({ confidence })),
      { origin_role: "user" }, { source_episode_ids: [uuid(9)] }, { echo_state: "direct" },
      { properties: { audit: { confidence: 1 } } }, { disposition: "retain" },
    ]) reject({ ...claim(), ...patch }, context(), "invalid_contract");
    reject({ task: "claim", claims: [], language: "en", modality: "text" }, context(), "invalid_contract");
  });
  test("bounds a batch to 32 claims and entity mentions to 16 without truncation", () => {
    expect(SemanticClaimBatch.safeParse({ claims: Array.from({ length: 32 }, () => claim()) }).success).toBe(true);
    expect(SemanticClaimBatch.safeParse({ claims: Array.from({ length: 33 }, () => claim()) }).success).toBe(false);
    reject({ ...claim(), entities: Array.from({ length: 17 }, (_, i) => ({ mention: `name${i}`, entity_id: null })) }, context(), "invalid_contract");
  });
  test("binds source digest, revision, authenticated speaker/role and single original authority", () => {
    const ctx = context(); const result = validateSemanticClaim(claim(), ctx);
    if (ctx.episode.provenance.episode_digest_version !== 2) throw new Error("expected version-2 fixture");
    expect(result.source_binding).toEqual({ episode_id: episodeId, revision_key: ctx.episode.revision_key,
      content_digest: ctx.episode.content_digest, origin_role: "user", lineage_digest: ctx.episode.provenance.lineage_digest });
    expect(result.identity.properties.speaker_key).toBe(extractionBodyDigest(ctx.episode.speaker));
    expect(result.authority).toEqual({ primary_episode_id: episodeId, source_episode_ids: [episodeId], source_count_total: 1, sources_truncated: false });
    expect(result.identity.max_source_ingest_seq).toBe(7);
    expect(result.identity.support_fact_ids).toEqual([]);
    ctx.episode.content += " changed";
    reject(claim(), ctx, "source_digest_mismatch");
    for (const ingest_seq of [NaN, Infinity, -1, 0, 0.5, Number.MAX_SAFE_INTEGER + 1])
      reject(claim(), { ...context(), episode: { ...context().episode, ingest_seq } }, "invalid_contract");
    reject(claim(), { ...context(), episode: { ...context().episode, schema: "anamnesis.claim/1" } }, "invalid_contract");
  });
});

describe("exact canonical meaning identity", () => {
  test("pins the complete canonical identity, excluding confidence and byte/audit time hints", () => {
    const expected: ReturnType<typeof validateSemanticClaim>["identity"] = {
      generation, schema: "anamnesis.claim/1", content: "Alice moved to Busan.", content_language: "en",
      properties: { speaker_key: extractionBodyDigest({ origin_source: "chat", origin_actor: "Alice" }), subject_keys: [entityId],
        predicate_text: "moved to", scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] }, scope_complete: true },
      time: { time_utc: sourceTime.time_utc, time_precision: "inherited" }, sub_kind: "event", modality: "asserted",
      primary_episode_id: episodeId, max_source_ingest_seq: 7, echo_state: "direct", echo_of_element_id: null,
      echo_depth: 0, echo_lineage_truncated: false, parent_recall_ids: [], corroboration_root_episode_ids: [episodeId],
      entity_ids: [entityId], source_episode_ids: [episodeId], support_fact_ids: [],
    };
    const result = validateSemanticClaim(claim(), context());
    expect(result.identity).toEqual(expected);
    expect(result.canonical_meaning).toBe(canonicalExtractionBody(expected));
    expect(result.meaning_digest).toBe(sha(result.canonical_meaning));
    for (const confidence of [0, 0.12, 1]) {
      const retry = validateSemanticClaim({ ...claim(), confidence }, context());
      expect(retry.canonical_meaning).toBe(result.canonical_meaning);
      expect(retry.meaning_digest).toBe(result.meaning_digest);
      expect(retry.confidence).toBe(confidence);
    }
    const spanOnly = { ...claim(), evidence: { kind: "source_locus", span: { start: 0, end: 5 } } };
    expect(validateSemanticClaim(spanOnly, context()).canonical_meaning).toBe(result.canonical_meaning);
    const c = claim(); c.time = { time_value: "September", time_utc: Date.parse("2026-09-01"), time_precision: "month", resolution: "explicit", anchor_time_utc: null };
    const first = validateSemanticClaim(c, context());
    c.time.time_value = "2026-09";
    expect(validateSemanticClaim(c, context()).canonical_meaning).toBe(first.canonical_meaning);
  });
  test("changes identity for modality, UTC, precision, scope, predicate, provenance and occurrence", () => {
    const original = validateSemanticClaim(claim(), context()).canonical_meaning;
    for (const c of [
      { ...claim(), modality: "intended" },
      { ...claim(), time: { time_value: "2019", time_utc: Date.parse("2019-01-01"), time_precision: "year", resolution: "explicit", anchor_time_utc: null } },
      { ...claim(), time: { ...claim().time, resolution: "explicit", anchor_time_utc: null, time_precision: "instant" } },
      { ...claim(), scope: { ...claim().scope, condition: "if employed" } },
      { ...claim(), scope_complete: false }, { ...claim(), predicate_text: "Moved to" },
    ]) expect(validateSemanticClaim(c, context()).canonical_meaning).not.toBe(original);
    expect(validateSemanticClaim(claim(), receiptContext()).canonical_meaning).not.toBe(original);
    const ctx = context(); ctx.episode.id = uuid(6);
    if (ctx.episode.provenance.episode_digest_version !== 2) throw new Error("fixture");
    ctx.episode.provenance.lineage.episode_id = uuid(6); ctx.episode.provenance.lineage.root_episode_ids = [uuid(6)];
    ctx.episode.provenance.lineage_digest = extractionBodyDigest(ctx.episode.provenance.lineage);
    expect(validateSemanticClaim(claim(), ctx).canonical_meaning).not.toBe(original);
  });
});

describe("source-language, Unicode and exact UTF-8 locus", () => {
  test("accepts only lowercase language tags within the 35-character storage bound", () => {
    const boundary = `abcdefgh-${"a".repeat(8)}-${"b".repeat(8)}-${"c".repeat(8)}`;
    expect(boundary.length).toBe(35);
    for (const tag of ["en", "zh-hant", "mul", "und", boundary]) {
      const c = claim(), ctx = context();
      c.content_language = tag; ctx.episode.content_language = tag;
      expect(validateSemanticClaim(c, ctx).identity.content_language).toBe(tag);
    }
    for (const tag of ["en-US", "zh-Hant", `${boundary}-d`]) {
      const c = claim(), ctx = context();
      c.content_language = tag; ctx.episode.content_language = tag;
      reject(c, ctx, "invalid_contract");
    }
  });
  test("preserves multilingual names, identifiers and predicates without translation or normalization", () => {
    const text = "민수🙂 moved to 부산 /tmp/Αlice.ts";
    const ctx = context(text); ctx.episode.content_language = "mul";
    ctx.entity_resolutions = [{ status: "existing", mention: "민수", entity_id: entityId }];
    const c = claim(text); c.content_language = "mul"; c.predicate_text = "이사했다"; c.entities = [{ mention: "민수", entity_id: entityId }];
    const result = validateSemanticClaim(c, ctx);
    expect(result.identity.content).toBe(text);
    expect(result.identity.properties.predicate_text).toBe("이사했다");
    expect(result.evidence?.end).toBe(Buffer.byteLength(text));
    reject({ ...c, content_language: "en" }, ctx, "language_policy_mismatch");
    reject(c, { ...ctx, fact_language_policy: "en" }, "invalid_contract");
  });
  test("derives a unique quote span and requires an exact disambiguating span for repeated quotes", () => {
    const ctx = context("🙂 Alice Alice"); const c = claim(); c.evidence = { kind: "source_locus", quote: "Alice" };
    reject(c, ctx, "evidence_mismatch");
    c.evidence = { kind: "source_locus", quote: "Alice", span: { start: 11, end: 16 } };
    expect(validateSemanticClaim(c, ctx).evidence).toEqual({ start: 11, end: 16, text: "Alice" });
    c.evidence = { kind: "source_locus", quote: "🙂" };
    expect(validateSemanticClaim(c, ctx).evidence).toEqual({ start: 0, end: 4, text: "🙂" });
    c.evidence = { kind: "source_locus", quote: "aa" };
    reject(c, context("Alice aaa"), "evidence_mismatch"); // overlapping occurrences also ambiguous
  });
  test("rejects nonliteral, normalized, translated and mismatched quotes", () => {
    for (const quote of ["ALICE", "Alice moved", "민수", "é"])
      reject({ ...claim(), evidence: { kind: "source_locus", quote } }, context("Alice e\u0301"), "evidence_mismatch");
    reject({ ...claim(), evidence: { kind: "source_locus", quote: "Alice", span: { start: 1, end: 6 } } }, context(), "evidence_mismatch");
  });
  test("rejects split code points, invalid offsets, oversized/empty/disjoint evidence", () => {
    for (const span of [{ start: 0, end: 1 }, { start: 1, end: 4 }, { start: 0, end: 99 }])
      reject({ ...claim(), evidence: { kind: "source_locus", span } }, context("🙂 Alice"), "evidence_mismatch");
    for (const span of [{ start: 1, end: 1 }, { start: -1, end: 2 }, { start: 0.5, end: 2 }, { start: NaN, end: 2 },
      { start: 0, end: Infinity }, { start: 0, end: 8193 }, { start: Number.MAX_SAFE_INTEGER + 1, end: Number.MAX_SAFE_INTEGER + 2 }])
      reject({ ...claim(), evidence: { kind: "source_locus", span } }, context(), "invalid_contract");
    for (const evidence of [{ kind: "source_locus" }, { kind: "source_locus", quote: "" }, { kind: "source_locus", quote: "🙂".repeat(2049) },
      { kind: "source_locus", spans: [{ start: 0, end: 5 }] }]) reject({ ...claim(), evidence }, context(), "invalid_contract");
  });
  test("rejects invalid Unicode anywhere, including trusted content before digest encoding", () => {
    for (const bad of ["\ud800", "\udc00"]) {
      reject({ ...claim(), content: bad }, context(), "invalid_contract");
      reject({ ...claim(), predicate_text: bad }, context(), "invalid_contract");
      reject({ ...claim(), evidence: { kind: "source_locus", quote: bad } }, context(), "invalid_contract");
      reject(claim(), context(bad), "invalid_contract");
    }
    reject({ ...claim(), predicate_text: "e\u0301" }, context(), "invalid_contract");
    expect(SemanticClaim.safeParse({ ...claim(), predicate_text: "🙂".repeat(256) }).success).toBe(true);
    expect(SemanticClaim.safeParse({ ...claim(), predicate_text: "🙂".repeat(257) }).success).toBe(false);
  });
  test("allows no single locus only with explicit permission, never labels it grounded", () => {
    const c = { ...claim(), evidence: { kind: "no_single_locus" } };
    reject(c, context(), "no_single_locus_disallowed");
    const result = validateSemanticClaim(c, { ...context(), allow_no_single_locus: true });
    expect(result.evidence).toBeNull(); expect(result.mechanically_grounded).toBe(false); expect(result.semantic_writes).toBe(false);
    reject({ ...claim(), evidence: undefined }, { ...context(), allow_no_single_locus: true }, "invalid_contract");
  });
  test("preserves BOM bytes in a literal source span", () => {
    const text = "\ufeffAlice";
    expect(validateSemanticClaim(claim(text), context(text)).evidence?.text).toBe(text);
  });
});

describe("resolved event time", () => {
  test("accepts explicit, source-anchored relative and exact inherited time", () => {
    const c = claim();
    c.time = { time_value: "2019-03", time_utc: Date.parse("2019-03-01"), time_precision: "month", resolution: "explicit", anchor_time_utc: null };
    expect(validateSemanticClaim(c, context()).identity.time.time_utc).toBe(Date.parse("2019-03-01"));
    c.time = { time_value: "last month", time_utc: Date.parse("2026-08-01"), time_precision: "month", resolution: "relative", anchor_time_utc: sourceTime.time_utc };
    expect(validateSemanticClaim(c, context()).identity.time.time_precision).toBe("month");
    c.time.anchor_time_utc! += 1;
    reject(c, context(), "time_resolution_mismatch");
    c.time = { ...claim().time, time_utc: sourceTime.time_utc - 1 };
    reject(c, context(), "time_resolution_mismatch");
  });
  test("rejects unresolved hints, precision defaults, invalid epochs and non-start intervals", () => {
    for (const patch of [{ time_precision: "second" }, { time_precision: "month" }, { time_utc: NaN }, { time_utc: Infinity },
      { time_utc: 8640000000000001 }, { time_utc: 0.5 }, { resolution: "wall_clock" }, { anchor_time_utc: null }, { time_hint: "yesterday" }])
      reject({ ...claim(), time: { ...claim().time, ...patch } }, context(), "invalid_contract");
    for (const [precision, value] of [["day", "2026-09-10T00:00:00.001Z"], ["month", "2026-09-02"], ["year", "2026-02-01"]])
      expect(SemanticResolvedTime.safeParse({ time_value: value, time_utc: Date.parse(value!), time_precision: precision }).success).toBe(false);
    expect(SemanticResolvedTime.safeParse({ time_value: "1960", time_utc: Date.parse("1960-01-01"), time_precision: "year" }).success).toBe(true);
  });
});

describe("entity resolution and closed grouping scope", () => {
  test("unresolved mentions create no Entity and null subjects disable grouping", () => {
    const c = claim("The client moved."); c.entities = [{ mention: "The client", entity_id: null }]; c.subject_keys = null; c.scope_complete = false;
    const ctx = context(c.content); ctx.entity_resolutions = [{ status: "unresolved", mention: "The client" }];
    const result = validateSemanticClaim(c, ctx);
    expect(result.identity.entity_ids).toEqual([]); expect(result.identity.properties.subject_keys).toBeNull();
    reject({ ...c, scope_complete: true }, ctx, "invalid_contract");
    reject({ ...c, subject_keys: ["the client"] }, ctx, "invalid_contract");
  });
  test("requires generation-bound trusted new/existing resolutions, not invented IDs or transliterations", () => {
    const ctx = context(); ctx.entity_resolutions = [{ status: "new", mention: "Alice", entity_id: entityId,
      normalized_name: "Alice", entity_kind: "person", entity_key: extractionBodyDigest({ generation, normalized_name: "Alice", entity_kind: "person" }) }];
    expect(validateSemanticClaim(claim(), ctx).identity.entity_ids).toEqual([entityId]);
    reject({ ...claim(), entities: [{ mention: "Alice", entity_id: uuid(99) }] }, ctx, "entity_resolution_mismatch");
    reject({ ...claim(), scope: { ...claim().scope, object_keys: [uuid(99)] } }, ctx, "entity_resolution_mismatch");
    reject(claim(), { ...ctx, generation: uuid(99) }, "entity_resolution_mismatch");
    reject(claim(), { ...ctx, entity_resolutions: [] }, "entity_resolution_mismatch");
    const resolved = ctx.entity_resolutions[0]!;
    reject(claim(), { ...ctx, entity_resolutions: [resolved, resolved] }, "entity_resolution_mismatch");
    reject(claim(), { ...ctx, entity_resolutions: [{ ...resolved, normalized_name: "Alisa" }] }, "entity_resolution_mismatch");
  });
  test("checks attribution speaker resolvability without literal-name fallback", () => {
    const c = claim(); c.modality = "reported";
    reject(c, context(), "invalid_contract");
    c.scope_complete = false;
    expect(validateSemanticClaim(c, context()).identity.modality).toBe("reported");
    c.scope.attribution_speaker_keys = ["b".repeat(64)];
    reject(c, context(), "entity_resolution_mismatch");
    c.scope.attribution_speaker_keys = [bobKey]; c.scope_complete = true;
    expect(validateSemanticClaim(c, context()).identity.properties.scope.attribution_speaker_keys).toEqual([bobKey]);
  });
  test("enforces canonical decimal, NFC, sorted distinct bounded scope and rejects unknown qualifiers", () => {
    for (const value of ["0", "-2", "0.01", "-0.01", "12.5"])
      expect(SemanticClaim.safeParse({ ...claim(), scope: { ...claim().scope, quantities: [{ value, unit: "kg" }] } }).success).toBe(true);
    for (const value of ["-0", "+1", "01", "1.0", "1e2", ".1", "0.0", "1." , "1".repeat(65)])
      reject({ ...claim(), scope: { ...claim().scope, quantities: [{ value, unit: "kg" }] } }, context(), "invalid_contract");
    for (const scope of [
      { ...claim().scope, object_keys: [entityId, entityId] }, { ...claim().scope, location_keys: [uuid(9), uuid(8)] },
      { ...claim().scope, quantities: [{ value: "2", unit: "kg" }, { value: "1", unit: "kg" }] },
      { ...claim().scope, quantities: [{ value: "1", unit: "kg" }, { value: "1", unit: "kg" }] },
      { ...claim().scope, quantities: Array.from({ length: 9 }, (_, i) => ({ value: String(i), unit: "kg" })) },
      { ...claim().scope, condition: "e\u0301" }, { ...claim().scope, qualifiers: "sometimes" },
    ]) reject({ ...claim(), scope }, context(), "invalid_contract");
  });
});

describe("retained lineage is not model-claimed independence", () => {
  test("direct authenticated roles retain own root and receipt lineage never adds assistant as a root", () => {
    for (const origin_role of ["user", "assistant", "tool", "document", "operator"] as const) {
      const ctx = context(); ctx.episode.provenance.origin_role = origin_role;
      const result = validateSemanticClaim(claim(), ctx);
      expect(result.identity.echo_state).toBe("direct"); expect(result.identity.corroboration_root_episode_ids).toEqual([episodeId]);
    }
    const ctx = receiptContext(); const result = validateSemanticClaim(claim(), ctx);
    expect(result.identity.echo_state).toBe("context_derived"); expect(result.identity.echo_depth).toBe(2);
    expect(result.identity.parent_recall_ids).toEqual([uuid(5)]); expect(result.identity.corroboration_root_episode_ids).toEqual([rootId]);
    expect(result.identity.echo_of_element_id).toBeNull(); expect(result.authority.source_episode_ids).toEqual([episodeId]);
    reject({ ...claim(), echo_of_element_id: uuid(10) }, ctx, "invalid_contract");
  });
  test("legacy/unknown or incomplete lineage fails closed, never repaired to direct", () => {
    for (const origin_role of [null, "user", "assistant"] as const) {
      const ctx = context(); ctx.episode.provenance = { episode_digest_version: 1, origin_role, lineage: null, lineage_digest: null };
      reject(claim(), ctx, "echo_lineage_unavailable");
    }
    reject(claim(), receiptContext(false), "echo_lineage_unavailable");
    const ctx = context();
    reject(claim(), { ...ctx, episode: { ...ctx.episode, provenance: { ...ctx.episode.provenance, episode_digest_version: 1 } } }, "invalid_contract");
  });
  test("rejects digest/episode mismatch, dishonest direct metadata and lineage bound overflow", () => {
    const ctx = receiptContext(); const provenance = ctx.episode.provenance;
    if (provenance.episode_digest_version !== 2) throw new Error("fixture");
    reject(claim(), { ...ctx, episode: { ...ctx.episode, provenance: { ...provenance, lineage_digest: "0".repeat(64) } } }, "lineage_mismatch");
    for (const patch of [
      { echo_depth: 9 }, { echo_depth: Infinity }, { parent_recall_ids: Array.from({ length: 5 }, (_, i) => uuid(i + 10)) },
      { context_digests: [] }, { root_episode_ids: Array.from({ length: 17 }, (_, i) => uuid(i + 10)) },
      { root_episode_ids: [rootId, rootId] }, { lineage_mode: "direct" }, { lineage_mode: "unknown" },
    ]) reject(claim(), { ...ctx, episode: { ...ctx.episode, provenance: { ...provenance, lineage: { ...provenance.lineage, ...patch } } } }, "invalid_contract");
    for (const patch of [{ episode_id: uuid(99) }, { root_episode_ids: [episodeId] }]) {
      const lineage = { ...provenance.lineage, ...patch };
      reject(claim(), { ...ctx, episode: { ...ctx.episode, provenance: { ...provenance, lineage, lineage_digest: extractionBodyDigest(lineage) } } }, "lineage_mismatch");
    }
  });
  test("is pure and cannot mutate retained input through its result", () => {
    const c = claim(), ctx = context(), before = canonicalExtractionBody({ c, ctx });
    const result = validateSemanticClaim(c, ctx);
    result.identity.source_episode_ids.push(uuid(99)); result.claim.content = "changed";
    expect(canonicalExtractionBody({ c, ctx })).toBe(before);
  });
});
