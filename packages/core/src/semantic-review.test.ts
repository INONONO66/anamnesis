import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import type { ExtractionProviderInput } from "./extraction.ts";
import { canonicalExtractionBody, extractionBodyDigest, ExtractionModelOutput, type Generation } from "../../protocol/src/extraction.ts";
import type { SemanticClaim, SemanticSourceContext } from "../../protocol/src/semantic-claim.ts";
import type { SemanticResolution, SemanticReviewOutput, SemanticReviewPremises, SemanticReviewProvider } from "../../protocol/src/materialization.ts";

const binding = uuidv7();
const context = { principal: "installation", commit_mode: "receipt", client_binding: binding } as const;
const unbound = { principal: "installation", commit_mode: "receipt" } as const;
const sentences = ["Alice prefers dark mode", "Alice prefers compact dark mode"];
const text = sentences.join(". ") + ".";
const spans = sentences.map(sentence => { const start = Buffer.byteLength(text.slice(0, text.indexOf(sentence))); return { start, end: start + Buffer.byteLength(sentence) }; });
const model = "qa-legacy-judge";
const incarnation = extractionBodyDigest(model);
const PROFILE = extractionBodyDigest("semantic-review-fixture");
const EN = "en";
type Source = Omit<SemanticSourceContext["episode"], "content_language">;

const extractionProvider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
  if (input.task === "claim") return { task: "claim", language: EN, modality: "text", claims: sentences.map((sentence, i) => ({ text: sentence, evidence: { ...spans[i]!, text: sentence } })) };
  if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
  return { task: "judge_claims", language: EN, modality: "text", claim_body_digest: input.claim_context.body_digest,
    decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
} };

interface ReviewFixture { calls: string[]; resolution: SemanticResolution; output: ((premises: SemanticReviewPremises) => SemanticReviewOutput) | null; provider: SemanticReviewProvider }
function reviewFixture(): ReviewFixture {
  const fixture: ReviewFixture = {
    calls: [], output: null,
    resolution: { entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false, content_language: EN },
    provider: { profileId: PROFILE,
      async resolve(premises) { fixture.calls.push(`resolve:${premises.request.proposal_id}`); return fixture.resolution; },
      async review(input) {
        fixture.calls.push(`review:${input.premises.request.proposal_id}`);
        return fixture.output ? fixture.output(input.premises) : { disposition: "retain", semantic_claim: input.premises.request.semantic_claim, reason: "fixture retained" };
      } },
  };
  return fixture;
}

function entityKey(generation: string) { return extractionBodyDigest({ generation, normalized_name: "Alice", entity_kind: "person" }); }
function newResolution(generation: string, entity_id: string): SemanticResolution {
  return { entity_resolutions: [{ status: "new", mention: "Alice", entity_id, normalized_name: "Alice", entity_kind: "person", entity_key: entityKey(generation) }],
    attribution_speakers: [], allow_no_single_locus: false, content_language: EN };
}
function existingResolution(entity_id: string): SemanticResolution {
  return { entity_resolutions: [{ status: "existing", mention: "Alice", entity_id }], attribution_speakers: [], allow_no_single_locus: false, content_language: EN };
}
function claim(source: Source, index: number, entity: string, overrides: Partial<SemanticClaim> = {}): SemanticClaim {
  return { content: sentences[index]!, content_language: EN, sub_kind: "fact", modality: "asserted", confidence: 0.7,
    time: { time_value: source.time.time_value, time_utc: source.time.time_utc, time_precision: "inherited", resolution: "inherited", anchor_time_utc: source.time.time_utc },
    entities: [{ mention: "Alice", entity_id: entity }], subject_keys: [entity], predicate_text: sentences[index]!,
    scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] }, scope_complete: true,
    evidence: { kind: "source_locus", span: spans[index]! }, ...overrides };
}

async function setup() {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "semantic-review-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}): Promise<Record<string, unknown>[]> =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const review = reviewFixture();
  const materializationStatePath = join(root, "materialization-state.json");
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider, semanticReviewProvider: review.provider, materializationStatePath });
  await query("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation, state: "catching_up", covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  const episode = (record: string) => ({ content: text, time: { value: "2026-09-01T00:00:00Z", precision: "day" as const }, origin: { source: root, session: root, actor: "user", record }, source_revision: `v-${record}`, expected_previous_revision_key: null });
  const audit = async (record: string) => {
    const source = await engine.remember(episode(record), { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: source.id }, context);
    const result = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
    if (result.state !== "known" || result.judge_attempt?.state !== "succeeded" || result.semantic_writes) throw new Error("legacy audit fixture must admit nothing");
    return { judge: result.judge_attempt, source: await engine.store.semanticEpisode(source.id, context) };
  };
  const first = await audit("one");
  const request = (proposal_id: string, index: number, semantic_claim: SemanticClaim, overrides: Partial<{ generation_id: string; source_episode_id: string; judge_attempt_id: string }> = {}) =>
    ({ proposal_id, generation_id: generation.id, source_episode_id: first.source.id, judge_attempt_id: first.judge.id, claim_index: index, semantic_claim, ...overrides });
  const materialize = (operation_id: string, proposal_id: string | null, index: number, semantic_claim: SemanticClaim) =>
    engine.materializeRetainedClaim({ operation_id, ...(proposal_id === null ? {} : { proposal_id }), generation_id: generation.id, source_episode_id: first.source.id, judge_attempt_id: first.judge.id, claim_index: index, semantic_claim }, context);
  const generationIn = async (state: Generation["state"]) => {
    const created = await engine.store.createExtractionGeneration({ ...generation, id: uuidv7(), state: "catching_up" }, context);
    const moved: Generation = { ...created, state };
    await query("MATCH (g:ExtractionGeneration {id:$id}) SET g.state=$state, g.body=$body", { id: moved.id, state, body: canonicalExtractionBody(moved) });
    return moved;
  };
  return { engine, query, review, generation, episode, audit, first, request, materialize, generationIn,
    options: { uri, password, objectsRoot: root, materializationStatePath },
    async close() { await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}
type Fixture = Awaited<ReturnType<typeof setup>>;

async function admitFirstClaim(f: Fixture) {
  const entity = uuidv7(), proposal = uuidv7(), semantic = claim(f.first.source, 0, entity);
  f.review.resolution = newResolution(f.generation.id, entity);
  await f.engine.proposeRetainedClaim(f.request(proposal, 0, semantic), context);
  await f.engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: proposal, action: "accept", reason: "operator accepted" }, context);
  return { entity, fact: (await f.materialize(uuidv7(), proposal, 0, semantic)).fact_id };
}

test("extraction records read back: the judge attempt, both tasks, the recorded decisions and a fresh claim task", async () => {
  const f = await setup();
  try {
    const judge = await f.engine.store.getExtractionAttempt(f.first.judge.id, context);
    expect(judge).toEqual(f.first.judge);
    expect(await f.engine.store.getExtractionTask(judge.task_id, context))
      .toMatchObject({ id: judge.task_id, attempt_id: judge.id, kind: "judge_claims", state: "succeeded", source_id: f.first.source.id, generation_id: f.generation.id });
    if (!judge.output) throw new Error("judge output expected");
    const output = ExtractionModelOutput.parse(JSON.parse(judge.output.canonical_body));
    if (output.task !== "judge_claims") throw new Error(`judge output expected, saw ${output.task}`);
    const decisions = await f.engine.store.readExtractionDecisions(judge.id, context);
    const claimAttemptId = decisions[0]!.claim_attempt_id;
    expect(decisions).toEqual(output.decisions.map(decision => ({ ...decision, judge_attempt_id: judge.id, claim_attempt_id: claimAttemptId, claim_body_digest: output.claim_body_digest })));
    expect(decisions).toHaveLength(sentences.length);
    const claimAttempt = await f.engine.store.getExtractionAttempt(claimAttemptId, context);
    expect(claimAttempt).toMatchObject({ state: "succeeded", disposition: "retain", source_id: f.first.source.id, spans });
    expect((await f.engine.store.getExtractionTask(claimAttempt.task_id, context)).kind).toBe("claim");
    const source = await f.engine.remember(f.episode("three"), { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    const created = await f.engine.createModelTask({ id: uuidv7(), generation_id: f.generation.id, source_id: source.id, kind: "claim", model, model_incarnation: incarnation }, context);
    expect(created).toMatchObject({ kind: "claim", state: "queued", attempt_id: null, lease: null, source_id: source.id, generation_id: f.generation.id, model, model_incarnation: incarnation });
    expect(await f.engine.store.getExtractionTask(created.id, context)).toEqual(created);
    await expect(f.engine.store.getExtractionTask(uuidv7(), context)).rejects.toThrow("unknown_ModelTask");
    await expect(f.engine.store.readExtractionDecisions(created.id, context)).rejects.toThrow("unknown_ExtractionAttempt");
  } finally { await f.close(); }
}, 120000);

test("operator review admits a legacy audit claim: propose, retain, review, materialize once", async () => {
  const f = await setup();
  try {
    const entity = uuidv7(), proposal = uuidv7(), review = uuidv7(), operation = uuidv7();
    const semantic = claim(f.first.source, 0, entity);
    f.review.resolution = newResolution(f.generation.id, entity);
    const retained = await f.engine.proposeRetainedClaim(f.request(proposal, 0, semantic), context);
    expect(retained.output).toEqual({ disposition: "retain", semantic_claim: semantic, reason: "fixture retained" });
    expect(retained.premises.judge_profile_id).toBe(PROFILE);
    expect(retained.premises.audit_evidence).toEqual({ ...spans[0]!, text: sentences[0]! });
    expect(retained.premises.candidates).toEqual([]);
    expect(retained.premises.source.id).toBe(f.first.source.id);
    expect(f.review.calls).toEqual([`resolve:${proposal}`, `review:${proposal}`]);
    expect(await f.engine.proposeRetainedClaim(f.request(proposal, 0, semantic), context)).toEqual(retained);
    expect(f.review.calls).toHaveLength(2);
    await expect(f.engine.proposeRetainedClaim(f.request(proposal, 1, semantic), context)).rejects.toThrow("semantic_proposal_conflict");
    await expect(f.materialize(operation, proposal, 0, semantic)).rejects.toThrow("retained_review_unavailable");
    await expect(f.materialize(operation, null, 0, semantic)).rejects.toThrow("retained_review_unavailable");
    await expect(f.engine.reviewRetainedClaim({ review_id: review, proposal_id: proposal, action: "accept", reason: "no binding" }, unbound)).rejects.toThrow("semantic_operator_binding_required");
    const accepted = { review_id: review, proposal_id: proposal, action: "accept" as const, reason: "operator accepted" };
    expect(await f.engine.reviewRetainedClaim(accepted, context)).toEqual(accepted);
    expect(await f.engine.reviewRetainedClaim(accepted, context)).toEqual(accepted);
    await expect(f.engine.reviewRetainedClaim({ ...accepted, reason: "changed" }, context)).rejects.toThrow("semantic_review_conflict");
    const result = await f.materialize(operation, proposal, 0, semantic);
    expect(result.created).toBe(true);
    expect(await f.engine.store.materializationState.get(operation)).toMatchObject({
      generation_id: f.generation.id, source_episode_id: f.first.source.id, fact_ids: [result.fact_id],
      result: { created: true, fact_id: result.fact_id, link_id: result.link_id },
    });
    expect(await f.materialize(operation, proposal, 0, semantic)).toEqual({ ...result, created: false });
    await expect(f.materialize(operation, proposal, 0, { ...semantic, confidence: 0.8 })).rejects.toThrow("materialization_conflict");
    await expect(f.materialize(uuidv7(), proposal, 0, { ...semantic, confidence: 0.8 })).rejects.toThrow("semantic_candidate_mismatch");
    await expect(f.materialize(uuidv7(), proposal, 0, semantic)).rejects.toThrow("semantic_proposal_consumed");
    expect(await f.query("MATCH (x:Fact) RETURN x.id AS id, x.proposal_id AS proposal, x.confidence AS confidence, x.entity_ids AS entities")).toEqual([{ id: result.fact_id, proposal, confidence: 0.7, entities: [entity] }]);
    expect(await f.query("MATCH (e:Entity) RETURN e.id AS id, e.content AS name, e.entity_key AS key")).toEqual([{ id: entity, name: "Alice", key: entityKey(f.generation.id) }]);
    expect(await f.query("MATCH (:Fact)-[l:DERIVED_FROM]->(:Episode) RETURN l.id AS id, l.span AS span, l.evidence_text AS text, l.proposal_id AS proposal")).toEqual([{ id: result.link_id, span: [spans[0]!.start, spans[0]!.end], text: sentences[0], proposal }]);
    expect(await f.query("MATCH (:Fact)-[:MENTIONS]->(e:Entity) RETURN e.id AS id")).toEqual([{ id: entity }]);
    expect(await f.query("MATCH (c:AdjudicationConsumption) RETURN c.proposal_id AS proposal, c.operation_id AS operation, c.fact_id AS fact")).toEqual([{ proposal, operation, fact: result.fact_id }]);
    expect((await f.engine.checkConductingArcs()).issues).toEqual([]);
  } finally { await f.close(); }
}, 120000);

test("committed materialization replays across Engine restart and rejects changed request", async () => {
  const f = await setup();
  let replacement: Engine | undefined;
  try {
    const entity = uuidv7(), proposal = uuidv7(), operation = uuidv7();
    const semantic = claim(f.first.source, 0, entity);
    f.review.resolution = newResolution(f.generation.id, entity);
    await f.engine.proposeRetainedClaim(f.request(proposal, 0, semantic), context);
    await f.engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: proposal, action: "accept", reason: "restart fixture" }, context);
    const first = await f.materialize(operation, proposal, 0, semantic);
    const before = await f.query("MATCH (f:Fact)-[l:DERIVED_FROM]->(e:Episode {id:$source}) RETURN f.id AS fact,l.id AS link", { source: f.first.source.id });
    replacement = new Engine(f.options);
    await replacement.claimWriterEpoch();
    const request = { operation_id: operation, proposal_id: proposal, generation_id: f.generation.id,
      source_episode_id: f.first.source.id, judge_attempt_id: f.first.judge.id, claim_index: 0, semantic_claim: semantic };

    expect(await replacement.store.materializationState.get(operation)).toMatchObject({
      fact_ids: [first.fact_id], result: first,
    });
    expect(await replacement.materializeRetainedClaim(request, context)).toEqual({ ...first, created: false });
    await expect(replacement.materializeRetainedClaim({ ...request, semantic_claim: { ...semantic, confidence: 0.8 } }, context))
      .rejects.toThrow("materialization_conflict");
    expect(await f.query("MATCH (f:Fact)-[l:DERIVED_FROM]->(e:Episode {id:$source}) RETURN f.id AS fact,l.id AS link", { source: f.first.source.id })).toEqual(before);
  } finally { await replacement?.close(); await f.close(); }
}, 120000);

test("uncommitted materialization intent cannot replay a missing Fact", async () => {
  const f = await setup();
  try {
    const entity = uuidv7(), proposal = uuidv7(), operation = uuidv7();
    const semantic = claim(f.first.source, 0, entity);
    f.review.resolution = newResolution(f.generation.id, entity);
    await f.engine.proposeRetainedClaim(f.request(proposal, 0, semantic), context);
    await f.engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: proposal, action: "accept", reason: "rollback fixture" }, context);
    const request = { operation_id: operation, proposal_id: proposal, generation_id: f.generation.id,
      source_episode_id: f.first.source.id, judge_attempt_id: f.first.judge.id, claim_index: 0, semantic_claim: semantic };
    const nonexistent = uuidv7();
    await f.engine.store.materializationState.set(operation, {
      request_digest: extractionBodyDigest(request),
      occurrence_key: extractionBodyDigest([f.generation.id, f.first.source.id, f.first.judge.id, 0]),
      generation_id: f.generation.id, source_episode_id: f.first.source.id, source_ingest_seq: f.first.source.ingest_seq,
      fact_ids: [nonexistent], result: { created: true, fact_id: nonexistent, link_id: uuidv7() },
    });

    const result = await f.engine.materializeRetainedClaim(request, context);

    expect(result.created).toBe(true);
    expect(result.fact_id).not.toBe(nonexistent);
    expect(await f.engine.store.materializationState.get(operation)).toMatchObject({ fact_ids: [result.fact_id], result });
    expect(await f.query("MATCH (f:Fact)-[l:DERIVED_FROM]->(e:Episode {id:$source}) RETURN f.id AS fact,l.id AS link", { source: f.first.source.id }))
      .toEqual([{ fact: result.fact_id, link: result.link_id }]);
  } finally { await f.close(); }
}, 120000);

test("a second claim of the source reuses the Entity; stale resolutions, evidence and audit indexes are refused", async () => {
  const f = await setup();
  try {
    const { entity, fact } = await admitFirstClaim(f);
    const stale = uuidv7(), second = claim(f.first.source, 1, entity);
    f.review.resolution = newResolution(f.generation.id, entity);
    await expect(f.engine.proposeRetainedClaim(f.request(stale, 1, second), context)).rejects.toThrow("semantic_entity_stale");
    await expect(f.engine.proposeRetainedClaim(f.request(stale, 1, second), context)).rejects.toThrow("semantic_review_incomplete");
    await expect(f.engine.store.completeSemanticReview(stale, existingResolution(entity), { disposition: "retain", semantic_claim: second, reason: "late" }, context)).rejects.toThrow("semantic_review_terminal");
    expect(await f.query("MATCH (a:AdjudicationAttempt {id:$id}) RETURN a.outcome AS outcome", { id: stale })).toEqual([{ outcome: "validation_error" }]);
    await expect(f.engine.store.completeSemanticReview(uuidv7(), existingResolution(entity), { disposition: "retain", semantic_claim: second, reason: "orphan" }, context)).rejects.toThrow("semantic_review_unavailable");
    const ghost = uuidv7();
    f.review.resolution = existingResolution(ghost);
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 1, claim(f.first.source, 1, ghost)), context)).rejects.toThrow("semantic_entity_stale");
    f.review.resolution = existingResolution(uuidv7());
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 1, second), context)).rejects.toThrow("entity_resolution_mismatch");
    f.review.resolution = existingResolution(entity);
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 1, claim(f.first.source, 1, entity, { evidence: { kind: "source_locus", span: { start: 0, end: 5 } } })), context)).rejects.toThrow("semantic_evidence_mismatch");
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 7, second), context)).rejects.toThrow("audit_decision_refused");
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 1, { ...second, content_language: "fr" }), context)).rejects.toThrow("language_policy_mismatch");
    const proposal = uuidv7();
    const retained = await f.engine.proposeRetainedClaim(f.request(proposal, 1, second), context);
    expect(retained.premises.candidates.map(candidate => candidate.id)).toEqual([fact]);
    await f.engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: proposal, action: "accept", reason: "second accepted" }, context);
    const result = await f.materialize(uuidv7(), proposal, 1, second);
    expect(result.created).toBe(true);
    expect(await f.query("MATCH (e:Entity) RETURN count(e) AS count")).toEqual([{ count: 1 }]);
    expect(await f.query("MATCH (x:Fact)-[:MENTIONS]->(e:Entity {id:$id}) RETURN count(x) AS count", { id: entity })).toEqual([{ count: 2 }]);
  } finally { await f.close(); }
}, 120000);

test("corrections materialize the corrected claim; suppression, rejection and output drift never do", async () => {
  const f = await setup();
  try {
    const entity = uuidv7(), proposed = claim(f.first.source, 0, entity), corrected = { ...proposed, confidence: 0.4, modality: "hedged" as const };
    f.review.resolution = newResolution(f.generation.id, entity);
    f.review.output = () => ({ disposition: "retain", semantic_claim: corrected, reason: "drifted" });
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 0, proposed), context)).rejects.toThrow("semantic_output_mismatch");
    f.review.output = () => ({ disposition: "correct", semantic_claim: corrected, reason: "hedged in source" });
    const correction = uuidv7();
    const retained = await f.engine.proposeRetainedClaim(f.request(correction, 0, proposed), context);
    expect(retained.output_claim_digest).toBe(extractionBodyDigest(corrected));
    expect(retained.proposed_claim_digest).not.toBe(retained.output_claim_digest);
    await f.engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: correction, action: "accept", reason: "accept correction" }, context);
    await expect(f.materialize(uuidv7(), correction, 0, proposed)).rejects.toThrow("semantic_candidate_mismatch");
    const result = await f.materialize(uuidv7(), correction, 0, corrected);
    expect(result.created).toBe(true);
    expect(await f.query("MATCH (x:Fact) RETURN x.confidence AS confidence, x.modality AS modality")).toEqual([{ confidence: 0.4, modality: "hedged" }]);
    await expect(f.materialize(uuidv7(), uuidv7(), 0, corrected)).rejects.toThrow("retained_review_unavailable");
    f.review.resolution = existingResolution(entity);
    f.review.output = () => ({ disposition: "suppress", semantic_claim: null, reason: "not a durable preference" });
    const suppressed = uuidv7(), second = claim(f.first.source, 1, entity);
    expect((await f.engine.proposeRetainedClaim(f.request(suppressed, 1, second), context)).output.semantic_claim).toBeNull();
    await f.engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: suppressed, action: "accept", reason: "agree" }, context);
    await expect(f.materialize(uuidv7(), suppressed, 1, second)).rejects.toThrow("semantic_candidate_mismatch");
    f.review.output = null;
    const rejected = uuidv7();
    await f.engine.proposeRetainedClaim(f.request(rejected, 1, second), context);
    await f.engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: rejected, action: "reject", reason: "operator rejected" }, context);
    await expect(f.materialize(uuidv7(), rejected, 1, second)).rejects.toThrow("semantic_review_refused");
    expect(await f.query("MATCH (x:Fact) RETURN count(x) AS count")).toEqual([{ count: 1 }]);
    const unconfigured = new Engine(f.options);
    try { await expect(unconfigured.proposeRetainedClaim(f.request(uuidv7(), 1, second), context)).rejects.toThrow("semantic_review_not_configured"); }
    finally { await unconfigured.close(); }
  } finally { await f.close(); }
}, 120000);

test("premises bind the judge, the writable selected generation and a bounded candidate partition", async () => {
  const f = await setup();
  try {
    const entity = uuidv7(), semantic = claim(f.first.source, 0, entity);
    f.review.resolution = newResolution(f.generation.id, entity);
    const other = await f.audit("two");
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 0, semantic, { judge_attempt_id: other.judge.id }), context)).rejects.toThrow("stale_materialization");
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 0, semantic, { judge_attempt_id: uuidv7() }), context)).rejects.toThrow("unknown_ExtractionAttempt");
    const retired = await f.generationIn("retired");
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 0, semantic, { generation_id: retired.id }), context)).rejects.toThrow("generation_not_writable");
    const unselected = await f.generationIn("active");
    expect(await f.engine.readExtractionSelection(context)).toEqual({ generation_id: null, selector_version: 0 });
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 0, semantic, { generation_id: unselected.id }), context)).rejects.toThrow("semantic_generation_unselected");
    const partition = Array.from({ length: 129 }, () => uuidv7()).sort();
    const seed = (ids: string[]) => f.query("UNWIND $ids AS id CREATE (:Fact {id: id, generation: $generation, content: 'overflow', source_episode_ids: [$source]})", { ids, generation: f.generation.id, source: f.first.source.id });
    await seed(partition.slice(0, 128));
    const full = uuidv7();
    expect((await f.engine.proposeRetainedClaim(f.request(full, 0, semantic), context)).premises.candidates.map(candidate => candidate.id)).toEqual(partition.slice(0, 128));
    await seed(partition.slice(128));
    await expect(f.engine.proposeRetainedClaim(f.request(uuidv7(), 0, semantic), context)).rejects.toThrow("semantic_candidates_unavailable");
    expect(f.review.calls).toEqual([`resolve:${full}`, `review:${full}`]);
  } finally { await f.close(); }
}, 120000);

test("activation receipts bind the generation the store returns; rollback reopens only a retired target", async () => {
  const f = await setup();
  try {
    const id = f.generation.id;
    for (const partition of ["episodes", "active_extraction"] as const) await f.engine.store.recordExtractionCoverage({ generation_id: id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
    const profile = { generation_id: id, profile_version: "profile-1", coverage_generation_id: id };
    const activated = await f.engine.cutoverExtractionGenerationBound({ generation_id: id, expected_generation_id: null, expected_selector_version: 0 }, profile, context);
    expect(activated.generation.state).toBe("active");
    expect(activated.receipt).toMatchObject({ generation_id: id, generation_version: incarnation, profile_version: "profile-1", coverage_generation_id: id });
    expect(await f.engine.readExtractionSelection(context)).toEqual({ generation_id: id, selector_version: 1 });
    const coverage = await f.engine.readExtractionCoverage({ generation_id: id, expected_selector_version: 1 }, context);
    expect(coverage).toEqual({ generation_id: id, selector_version: 1, required_ingest_seq: 1, covered_ingest_seq: 1, omission_digest: expect.stringMatching(/^[0-9a-f]{64}$/), policy_revision: 0, read_at: expect.any(Number) });
    const bound = await f.engine.readExtractionCoverageBound({ generation_id: id }, profile, context);
    expect(bound.coverage).toEqual({ ...coverage, read_at: expect.any(Number) });
    expect(bound.receipt).toEqual(activated.receipt);
    await expect(f.engine.readExtractionCoverageBound({ generation_id: id }, { generation: 1, profile_version: "profile-1", coverage_generation_id: id }, context)).rejects.toThrow("generation_identity_refused");
    await expect(f.engine.readExtractionCoverageBound({ generation_id: id }, { ...profile, generation_id: uuidv7() }, context)).rejects.toThrow("generation_identity_mismatch");
    await expect(f.engine.readExtractionCoverage({ generation_id: id, expected_selector_version: 0 }, context)).rejects.toThrow("selector_version_conflict");
    await expect(f.engine.readExtractionCoverage({ generation_id: uuidv7() }, context)).rejects.toThrow("generation_not_selected");
    await expect(f.engine.rollbackExtractionGeneration({ generation_id: id, expected_generation_id: id, expected_selector_version: 1 }, context)).rejects.toThrow("rollback_requires_retired");
    const retired = await f.generationIn("retired");
    await expect(f.engine.rollbackExtractionGeneration({ generation_id: retired.id, expected_generation_id: id, expected_selector_version: 0 }, context)).rejects.toThrow("selector_version_conflict");
    await expect(f.engine.rollbackExtractionGeneration({ generation_id: retired.id, expected_generation_id: null, expected_selector_version: 1 }, context)).rejects.toThrow("selector_conflict");
    const reopened = await f.engine.rollbackExtractionGeneration({ generation_id: retired.id, expected_generation_id: id, expected_selector_version: 1 }, context);
    expect(reopened).toMatchObject({ id: retired.id, state: "catching_up" });
    expect(await f.engine.readExtractionSelection(context)).toEqual({ generation_id: id, selector_version: 2 });
    expect(await f.query("MATCH (g:ExtractionGeneration {id:$id}) RETURN g.state AS state, g.source_high_watermark AS watermark", { id: retired.id })).toEqual([{ state: "catching_up", watermark: 1 }]);
  } finally { await f.close(); }
}, 120000);
