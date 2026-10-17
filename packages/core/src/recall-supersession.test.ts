import { afterAll, beforeAll, expect, test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { canonicalExtractionBody, extractionBodyDigest, type Generation, type RpcRecallResult,
  type SemanticClaim, type SemanticReviewProvider } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { EmbeddingProfile, type EmbeddingProvider } from "./embedding.ts";
import type { ExtractionProviderInput } from "./extraction.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned Neo4j runner required");
const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const now = Date.UTC(2030, 0, 1), query = "quokka";
const older = "The quokka budget is 32k.", newer = "The quokka budget is 40k.";
const root = await mkdtemp(join(tmpdir(), "anamnesis-recall-supersession-"));
const provider: EmbeddingProvider = {
  profile: EmbeddingProfile.parse({ model: "recall-supersession-fixture", model_incarnation: "e".repeat(64), dimensions: 2,
    document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 }),
  async embed() { return [1, 0]; },
};
const extractionProvider = {
  model: "recall-supersession-claim-fixture", modelIncarnation: extractionBodyDigest("recall-supersession-claim-fixture"),
  async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
      claims: [{ text: input.text, evidence: { start: 0, end: Buffer.byteLength(input.text), text: input.text } }] };
    if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
      decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};
const review: SemanticReviewProvider = {
  profileId: extractionBodyDigest("recall-supersession-review-fixture"),
  async resolve() { return { entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false, content_language: "en" }; },
  async review(input) { return { disposition: "retain", semantic_claim: input.premises.request.semantic_claim, reason: "fixture retained" }; },
};
const engine = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider, extractionProvider,
  semanticReviewProvider: review, clock: () => now });
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
  state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now };
const sources: Record<"a" | "b", string> = { a: "", b: "" };
const facts: Record<"a" | "b", string> = { a: "", b: "" };

async function admitFact(sourceId: string, content: string) {
  const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: sourceId }, context);
  const audit = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
  assert(audit.state === "known" && audit.judge_attempt?.state === "succeeded" && !audit.semantic_writes);
  const source = await engine.store.semanticEpisode(sourceId, context);
  const semantic: SemanticClaim = {
    content, content_language: "en", sub_kind: "fact", modality: "asserted", confidence: 0.7,
    time: { time_value: source.time.time_value, time_utc: source.time.time_utc, time_precision: "inherited",
      resolution: "inherited", anchor_time_utc: source.time.time_utc },
    entities: [], subject_keys: null, predicate_text: content, scope_complete: false,
    scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
    evidence: { kind: "source_locus", span: { start: 0, end: Buffer.byteLength(content) } },
  };
  const request = { proposal_id: uuidv7(), generation_id: generation.id, source_episode_id: sourceId,
    judge_attempt_id: audit.judge_attempt.id, claim_index: 0, semantic_claim: semantic };
  await engine.proposeRetainedClaim(request, context);
  await engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: request.proposal_id, action: "accept", reason: "fixture accepted" }, context);
  return (await engine.materializeRetainedClaim({ ...request, operation_id: uuidv7() }, context)).fact_id;
}

beforeAll(async () => {
  await engine.init(); await engine.claimWriterEpoch();
  await engine.store.createExtractionGeneration(generation, context);
  for (const [key, content, value] of [["a", older, "2026-09-01T00:00:00Z"], ["b", newer, "2026-09-02T00:00:00Z"]] as const) {
    const episode = await engine.remember({ content, time: { value, precision: "second" },
      origin: { source: root, session: "supersession", actor: "user", record: key } },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    sources[key] = episode.id;
    facts[key] = await admitFact(episode.id, content);
  }
  expect(await engine.drainEmbeddingOutbox()).toEqual({ drained: 2, quarantined: 0, deferred: 0, deferral_reason: null });
  // Serving selection is fixture state; this test does not bypass or exercise the activation coverage gate.
  await admin.executeQuery(`MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:'extraction_selector'})
    SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=1`,
  { id: generation.id, body: canonicalExtractionBody({ ...generation, state: "active" }) });
  await engine.link({ id: uuidv7(), from: facts.b, to: facts.a, role: "INVALIDATES", content: "The later budget replaces the earlier one" });
  const link = await admin.executeQuery(`MATCH (:Fact {id:$from})-[l:INVALIDATES]->(:Fact {id:$to}) RETURN l.target_id AS target,l.effective_time_utc AS effective`,
    { from: facts.b, to: facts.a });
  expect(link.records.map(row => row.toObject())).toEqual([{ target: facts.a, effective: "2026-09-02T00:00:00.000Z" }]);
}, 120000);
afterAll(async () => { await engine.close(); await admin.close(); await rm(root, { recursive: true }); });

const primaryIds = (result: RpcRecallResult) => result.results.map(item => item.id);

test("after the invalidation takes effect the replacing Fact carries the superseded one", async () => {
  const result = await engine.recallHybrid({ query, window: 0, T: Date.parse("2026-09-03T00:00:00Z") }, context);

  const replacing = result.results.find(item => item.id === facts.b);
  assert(replacing?.kind === "Fact", JSON.stringify(result.results));
  expect(replacing.provenance).toMatchObject({ supersedes: [{ id: facts.a, content: older }], supersedes_redacted: false, warnings: [] });
  expect(primaryIds(result)).not.toContain(facts.a);
  const receipt = await engine.getReceipt(result.recall_id);
  expect(receipt?.serving?.candidates.map(candidate => candidate.id)).not.toContain(facts.a);
}, 120000);

test("before the invalidation takes effect only the earlier Fact is served", async () => {
  const result = await engine.recallHybrid({ query, window: 0, T: Date.parse("2026-09-01T12:00:00Z") }, context);

  const earlier = result.results.find(item => item.id === facts.a);
  assert(earlier?.kind === "Fact", JSON.stringify(result.results));
  expect(earlier.provenance).toMatchObject({ supersedes: [], supersedes_redacted: false, warnings: [] });
  expect(primaryIds(result)).not.toContain(facts.b);
}, 120000);

test("superseded Fact text whose source is denied is withheld behind a redaction flag", async () => {
  const policy_id = uuidv7();
  await engine.setPolicy({ policy_id, selector: { episode_id: sources.a }, scope: "content" }, context);
  try {
    const result = await engine.recallHybrid({ query, window: 0, T: Date.parse("2026-09-03T00:00:00Z") }, context);

    const replacing = result.results.find(item => item.id === facts.b);
    assert(replacing?.kind === "Fact", JSON.stringify(result.results));
    expect(replacing.provenance.supersedes).toEqual([]);
    expect(replacing.provenance.supersedes_redacted).toBe(true);
    expect(replacing.provenance.warnings.map(warning => warning.code)).toEqual(["supersedes_withheld"]);
  } finally {
    await engine.revokePolicy({ policy_id }, context);
  }
}, 120000);
