import { createHash } from "node:crypto";
import { z } from "zod";
import { ClaimSubKind } from "./element.ts";
import { canonicalExtractionBody, extractionBodyDigest } from "./extraction.ts";

const id = z.uuidv7();
const hash = z.string().regex(/^[0-9a-f]{64}$/);
const count = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const epoch = z.number().int().min(-8640000000000000).max(8640000000000000);
const utf8 = (max: number) => z.string().min(1).max(max).refine(
  v => !/[\uD800-\uDFFF]/u.test(v) && Buffer.byteLength(v, "utf8") <= max, "invalid or oversized UTF-8",
);
const nfc = (max: number) => z.string().min(1).max(max * 2).refine(
  v => !/[\uD800-\uDFFF]/u.test(v) && [...v].length <= max && v.normalize("NFC") === v, "expected bounded NFC scalars",
);
const language = z.string().regex(/^[a-z]{2,8}(?:-[a-z0-9]{1,8})*$/).max(35);
const bytesCompare = (a: string, b: string) => Buffer.compare(Buffer.from(a), Buffer.from(b));
const ordered = <T extends z.ZodType<string>>(item: T, max: number) => z.array(item).max(max)
  .refine(v => v.every((s, i) => i === 0 || bytesCompare(v[i - 1]!, s) < 0), "expected distinct UTF-8 sorted keys");
const sha256 = (text: string) => createHash("sha256").update(text, "utf8").digest("hex");

/** Speech act, never the text/code modality of the separate audit ABI. */
export const SemanticClaimModality = z.enum(["asserted", "reported", "hedged", "intended", "hypothetical"]);
export const SemanticTimePrecision = z.enum(["instant", "day", "month", "year", "inherited"]);
/** Post-L2 event time. The expression is audit; UTC and precision carry meaning. */
export const SemanticResolvedTime = z.strictObject({
  time_value: utf8(1024), time_utc: epoch, time_precision: SemanticTimePrecision,
}).refine(v => {
  if (v.time_precision === "instant" || v.time_precision === "inherited") return true;
  const d = new Date(v.time_utc);
  return d.getUTCHours() === 0 && d.getUTCMinutes() === 0 && d.getUTCSeconds() === 0 && d.getUTCMilliseconds() === 0
    && (v.time_precision === "day" || d.getUTCDate() === 1)
    && (v.time_precision !== "year" || d.getUTCMonth() === 0);
}, "low precision must be the UTC interval start");
export type SemanticResolvedTime = z.infer<typeof SemanticResolvedTime>;
const resolvedTime = z.strictObject({
  ...SemanticResolvedTime.shape,
  resolution: z.enum(["explicit", "relative", "inherited"]),
  anchor_time_utc: epoch.nullable(),
}).superRefine((v, ctx) => {
  if (!SemanticResolvedTime.safeParse({ time_value: v.time_value, time_utc: v.time_utc, time_precision: v.time_precision }).success
    || (v.resolution === "inherited") !== (v.time_precision === "inherited")
    || (v.resolution === "explicit") !== (v.anchor_time_utc === null)
    || (v.resolution === "relative" && v.time_precision === "instant"))
    ctx.addIssue({ code: "custom", message: "invalid time resolution" });
});
const quantity = z.strictObject({
  value: z.string().max(64).regex(/^(?:0|-?(?:[1-9][0-9]*(?:\.[0-9]*[1-9])?|0\.[0-9]*[1-9]))$/),
  unit: nfc(64),
});
export const SemanticClaimScope = z.strictObject({
  object_keys: ordered(id, 16), location_keys: ordered(id, 8),
  quantities: z.array(quantity).max(8).refine(v => v.every((q, i) => {
    const previous = v[i - 1];
    return !previous || bytesCompare(previous.unit, q.unit) < 0
      || (previous.unit === q.unit && bytesCompare(previous.value, q.value) < 0);
  }), "expected distinct quantities sorted by unit then value"),
  condition: nfc(256).nullable(), attribution_speaker_keys: ordered(hash, 8),
});
/** Local mention text never stands in for a resolved Entity ID. */
export const SemanticEntityReference = z.strictObject({ mention: utf8(1024), entity_id: id.nullable() });
/** Supplied by the trusted, generation-pinned L3 resolver, not the extractor. */
export const SemanticEntityResolution = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("unresolved"), mention: utf8(1024) }),
  z.strictObject({ status: z.literal("existing"), mention: utf8(1024), entity_id: id }),
  z.strictObject({ status: z.literal("new"), mention: utf8(1024), entity_id: id,
    normalized_name: nfc(256), entity_kind: nfc(64), entity_key: hash }),
]);
const span = z.strictObject({ start: count, end: count })
  .refine(v => v.start < v.end && v.end - v.start <= 8192, "invalid source span");
const evidence = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("source_locus"), quote: utf8(8192).optional(), span: span.optional() })
    .refine(v => v.quote !== undefined || v.span !== undefined, "source locus requires a quote or span"),
  z.strictObject({ kind: z.literal("no_single_locus") }),
]);
/** Bounded post-L2/L3 extraction candidate, not a judge disposition or Fact.
 * Source-language fidelity, entailment and reference correctness need independent
 * semantic review. Mechanical validation neither judges world truth nor writes. */
export const SemanticClaim = z.strictObject({
  content: utf8(8192), content_language: language, sub_kind: ClaimSubKind,
  modality: SemanticClaimModality, confidence: z.number().min(0).max(1),
  time: resolvedTime, entities: z.array(SemanticEntityReference).max(16),
  subject_keys: ordered(id, 16).min(1).nullable(), predicate_text: nfc(256),
  scope: SemanticClaimScope, scope_complete: z.boolean(), evidence,
}).superRefine((v, ctx) => {
  if (new Set(v.entities.map(e => e.mention)).size !== v.entities.length)
    ctx.addIssue({ code: "custom", message: "duplicate entity mention" });
  if (v.scope_complete && (v.subject_keys === null || v.entities.some(e => e.entity_id === null)
    || (v.modality === "reported" && v.scope.attribution_speaker_keys.length === 0)))
    ctx.addIssue({ code: "custom", message: "unresolved scope cannot be complete" });
});
export type SemanticClaim = z.infer<typeof SemanticClaim>;
export const SemanticClaimBatch = z.strictObject({ claims: z.array(SemanticClaim).max(32) });

const role = z.enum(["user", "assistant", "tool", "document", "operator"]);
const speaker = z.strictObject({ origin_source: utf8(256), origin_actor: utf8(256) });
const lineage = z.strictObject({
  episode_id: id, lineage_mode: z.enum(["direct", "receipts"]),
  parent_recall_ids: ordered(id, 4), context_digests: z.array(hash).max(4),
  root_episode_ids: ordered(id, 16), echo_depth: count.max(8), complete: z.boolean(),
}).superRefine((v, ctx) => {
  if (v.parent_recall_ids.length !== v.context_digests.length
    || (v.lineage_mode === "direct" && (v.parent_recall_ids.length !== 0 || v.echo_depth !== 0 || !v.complete
      || v.root_episode_ids.length !== 1 || v.root_episode_ids[0] !== v.episode_id))
    || (v.lineage_mode === "receipts" && (v.parent_recall_ids.length === 0 || v.echo_depth === 0))
    || (v.complete && v.root_episode_ids.length === 0))
    ctx.addIssue({ code: "custom", message: "invalid retained lineage" });
});
const provenance = z.discriminatedUnion("episode_digest_version", [
  z.strictObject({ episode_digest_version: z.literal(1), origin_role: role.nullable(), lineage: z.null(), lineage_digest: z.null() }),
  z.strictObject({ episode_digest_version: z.literal(2), origin_role: role, lineage, lineage_digest: hash }),
]);
/** Application-only trust input: load from immutable retained Episode/lineage
 * and bounded L3 results. Never deserialize this from a model or public RPC.
 * Parsing checks integrity, not caller authentication or current graph state. */
export const SemanticSourceContext = z.strictObject({
  generation: id, fact_language_policy: z.literal("source"), allow_no_single_locus: z.boolean(),
  episode: z.strictObject({
    id, schema: z.enum(["anamnesis.original-message/1", "anamnesis.original-document/1"]),
    revision_key: hash, content_digest: hash, content: utf8(1048576), content_language: language,
    ingest_seq: count.positive(), time: SemanticResolvedTime,
    speaker: speaker.nullable(), provenance,
  }),
  entity_resolutions: z.array(SemanticEntityResolution).max(16),
  attribution_speakers: z.array(speaker).max(8),
});
export type SemanticSourceContext = z.infer<typeof SemanticSourceContext>;
export class SemanticClaimValidationError extends Error {
  constructor(readonly code: "invalid_contract" | "source_digest_mismatch" | "lineage_mismatch" | "echo_lineage_unavailable"
    | "language_policy_mismatch" | "time_resolution_mismatch" | "entity_resolution_mismatch"
    | "evidence_mismatch" | "no_single_locus_disallowed") {
    super(code); this.name = "SemanticClaimValidationError";
  }
}
function fail(code: SemanticClaimValidationError["code"]): never { throw new SemanticClaimValidationError(code); }

/** Pure W2 + static W1 prerequisite. This is intentionally not materialization
 * permission: review, policy/head/candidate revalidation and write fencing remain
 * separate. Known-echo classification is reserved for L4 exact-delivery review. */
export function validateSemanticClaim(input: unknown, retainedContext: unknown) {
  // Validate the JSON domain before Zod can strip undefined values or invoke accessors.
  try { canonicalExtractionBody(input); canonicalExtractionBody(retainedContext); }
  catch { fail("invalid_contract"); }
  const parsedClaim = SemanticClaim.safeParse(input);
  const parsedSource = SemanticSourceContext.safeParse(retainedContext);
  if (!parsedClaim.success || !parsedSource.success) fail("invalid_contract");
  const claim = parsedClaim.data;
  const context = parsedSource.data;
  const source = context.episode;
  if (sha256(source.content) !== source.content_digest) fail("source_digest_mismatch");
  const origin = source.provenance;
  if (origin.episode_digest_version === 1) fail("echo_lineage_unavailable");
  const ancestry = origin.lineage;
  if (ancestry.episode_id !== source.id || extractionBodyDigest(ancestry) !== origin.lineage_digest)
    fail("lineage_mismatch");
  if (!ancestry.complete) fail("echo_lineage_unavailable");
  if (ancestry.lineage_mode === "receipts" && ancestry.root_episode_ids.includes(source.id)) fail("lineage_mismatch");
  if (claim.content_language !== source.content_language) fail("language_policy_mismatch");
  if (claim.time.resolution !== "explicit" && claim.time.anchor_time_utc !== source.time.time_utc)
    fail("time_resolution_mismatch");
  if (claim.time.resolution === "inherited" && (claim.time.time_utc !== source.time.time_utc || claim.time.time_value !== source.time.time_value))
    fail("time_resolution_mismatch");

  const resolutions = new Map(context.entity_resolutions.map(e => [e.mention, e]));
  if (resolutions.size !== context.entity_resolutions.length) fail("entity_resolution_mismatch");
  for (const ref of claim.entities) {
    const resolved = resolutions.get(ref.mention);
    if (!resolved || !source.content.includes(ref.mention)
      || ref.entity_id !== (resolved.status === "unresolved" ? null : resolved.entity_id)) fail("entity_resolution_mismatch");
    if (resolved.status === "new" && (!source.content.includes(resolved.normalized_name)
      || resolved.entity_key !== extractionBodyDigest({ generation: context.generation, normalized_name: resolved.normalized_name, entity_kind: resolved.entity_kind })))
      fail("entity_resolution_mismatch");
  }
  const entityIds = [...new Set(claim.entities.flatMap(e => e.entity_id === null ? [] : [e.entity_id]))].sort(bytesCompare);
  for (const key of [...claim.subject_keys ?? [], ...claim.scope.object_keys, ...claim.scope.location_keys])
    if (!entityIds.includes(key)) fail("entity_resolution_mismatch");
  const attributionKeys = context.attribution_speakers.map(s => extractionBodyDigest(s));
  if (new Set(attributionKeys).size !== attributionKeys.length
    || claim.scope.attribution_speaker_keys.some(k => !attributionKeys.includes(k))) fail("entity_resolution_mismatch");

  let locus: { start: number; end: number; text: string } | null = null;
  if (claim.evidence.kind === "no_single_locus") {
    if (!context.allow_no_single_locus) fail("no_single_locus_disallowed");
  } else {
    const bytes = Buffer.from(source.content, "utf8");
    let selected = claim.evidence.span;
    const quote = claim.evidence.quote;
    if (!selected && quote !== undefined) {
      const needle = Buffer.from(quote, "utf8");
      const start = bytes.indexOf(needle);
      if (start < 0 || bytes.indexOf(needle, start + 1) !== -1) fail("evidence_mismatch");
      selected = { start, end: start + needle.length };
    }
    if (!selected || selected.end > bytes.length) fail("evidence_mismatch");
    const slice = bytes.subarray(selected.start, selected.end);
    // Fatal decoding rejects split UTF-8 code points; preserve a literal BOM too.
    let text: string;
    try { text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(slice); }
    catch { fail("evidence_mismatch"); }
    if (quote !== undefined && text !== quote) fail("evidence_mismatch");
    locus = { ...selected, text };
  }
  const speakerKey = source.speaker === null ? null : extractionBodyDigest(source.speaker);
  const time = { time_utc: claim.time.time_utc, time_precision: claim.time.time_precision };
  const properties = { speaker_key: speakerKey, subject_keys: claim.subject_keys,
    predicate_text: claim.predicate_text, scope: claim.scope, scope_complete: claim.scope_complete };
  // Exactly the extracted-Fact meaning fields of docs/01. No arbitrary properties,
  // confidence, raw output, expression, resolution hint or source byte span enters.
  const identity = {
    generation: context.generation, schema: "anamnesis.claim/1", content: claim.content,
    content_language: claim.content_language, properties, time, sub_kind: claim.sub_kind, modality: claim.modality,
    primary_episode_id: source.id, max_source_ingest_seq: source.ingest_seq,
    echo_state: ancestry.lineage_mode === "direct" ? "direct" : "context_derived",
    echo_of_element_id: null, echo_depth: ancestry.echo_depth, echo_lineage_truncated: false,
    parent_recall_ids: ancestry.parent_recall_ids, corroboration_root_episode_ids: ancestry.root_episode_ids,
    entity_ids: entityIds, source_episode_ids: [source.id], support_fact_ids: [],
  };
  const canonicalMeaning = canonicalExtractionBody(identity);
  return {
    claim, identity, canonical_meaning: canonicalMeaning, meaning_digest: sha256(canonicalMeaning),
    source_binding: { episode_id: source.id, revision_key: source.revision_key, content_digest: source.content_digest,
      origin_role: origin.origin_role, lineage_digest: origin.lineage_digest },
    authority: { primary_episode_id: source.id, source_episode_ids: [source.id], source_count_total: 1, sources_truncated: false },
    evidence: locus, mechanically_grounded: locus !== null,
    confidence: claim.confidence, confidence_basis: "source_fidelity" as const,
    semantic_writes: false as const, materialization_gate: "independent_shadow_review_required" as const,
  };
}
export type ValidatedSemanticClaim = ReturnType<typeof validateSemanticClaim>;
