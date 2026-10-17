import { afterAll, beforeAll, expect, test } from "bun:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { canonicalExtractionBody, extractionBodyDigest, type Generation, type RpcRecallResult,
  type SemanticClaim, type SemanticReviewProvider } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { EmbeddingProfile, type EmbeddingProvider } from "./embedding.ts";
import type { ExtractionProviderInput } from "./extraction.ts";
import { renderContext } from "./recall.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned Neo4j runner required");
const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const now = Date.UTC(2030, 0, 1), query = "windowvectorprobe";
const content = "The operator prefers violet themes.";
const root = await mkdtemp(join(tmpdir(), "anamnesis-recall-window-"));
const provider: EmbeddingProvider = {
  profile: EmbeddingProfile.parse({ model: "recall-window-fixture", model_incarnation: "d".repeat(64), dimensions: 2,
    document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 }),
  async embed() { return [1, 0]; },
};
const extractionProvider = {
  model: "recall-window-claim-fixture", modelIncarnation: extractionBodyDigest("recall-window-claim-fixture"),
  async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
      claims: [{ text: content, evidence: { start: 0, end: Buffer.byteLength(content), text: content } }] };
    if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
      decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};
const review: SemanticReviewProvider = {
  profileId: extractionBodyDigest("recall-window-review-fixture"),
  async resolve() { return { entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false, content_language: "en" }; },
  async review(input) { return { disposition: "retain", semantic_claim: input.premises.request.semantic_claim, reason: "fixture retained" }; },
};
const engine = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider, extractionProvider,
  semanticReviewProvider: review, clock: () => now });
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
  state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now };
const episodes: string[] = [];
let factId: string;

function episodeId(position: number): string {
  const id = episodes[position - 1]; assert(id);
  return id;
}

async function admitFact(sourceId: string) {
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
  for (let i = 1; i <= 6; i++) {
    const episode = await engine.remember({ content: i === 3 ? content : `Context message ${i}.`,
      time: { value: "2026-09-01T00:00:00Z", precision: "second" },
      origin: { source: root, session: "window", actor: "user", record: String(i) } },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    episodes.push(episode.id);
  }
  const sourceId = episodes[2]; assert(sourceId);
  factId = await admitFact(sourceId);
  expect(await engine.drainEmbeddingOutbox()).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
  // Serving selection is fixture state; this test does not bypass or exercise the activation coverage gate.
  await admin.executeQuery(`MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:'extraction_selector'})
    SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=1`,
  { id: generation.id, body: canonicalExtractionBody({ ...generation, state: "active" }) });
}, 120000);
afterAll(async () => { await engine.close(); await admin.close(); await rm(root, { recursive: true }); });

function recalledFact(result: RpcRecallResult) {
  const fact = result.results.find(item => item.id === factId);
  assert(fact?.kind === "Fact");
  return fact;
}
function expectWindow(result: RpcRecallResult, before: string[], after: string[]) {
  const fact = recalledFact(result);
  expect(fact.sources).toEqual([episodeId(3)]);
  expect(fact.channels).toContain("vector");
  expect(fact.window).toEqual({ source: episodeId(3), before, after });
  expect(result.companions.map(item => item.id).sort()).toEqual([...before, ...after].sort());
  for (const item of result.companions) {
    expect(item).toMatchObject({ kind: "Episode", relevance: 0, score: 0, channels: [], sources: [item.id],
      provenance: { derived_from: [{ id: item.id, kind: "Episode", visible_at_T: true }] } });
  }
  expect(result.context_text).toBe(renderContext(result.results, result.companions));
  expect(result.used_budget).toBe(Buffer.byteLength(result.context_text));
}

test("default Fact recall delivers two same-session Episodes before and after its source", async () => {
  const result = await engine.recallHybrid({ query }, context);

  expect(result.results).toHaveLength(1);
  expect(result.diagnostics).toMatchObject({ pipeline: "derived-hybrid-v1", channels_used: ["vector"] });
  expectWindow(result, [episodeId(2), episodeId(1)], [episodeId(4), episodeId(5)]);
  const receipt = await engine.getReceipt(result.recall_id);
  expect(receipt?.serving?.response).toEqual(result);
  expect(receipt?.serving?.context_digest).toBe(createHash("sha256").update(result.context_text).digest("hex"));
  // D56: the served Fact owns the vector; no Episode is embedded and no vector names an Episode.
  const count = async (cypher: string, params: Record<string, string> = {}) =>
    (await admin.executeQuery(cypher, params)).records[0]?.get("n") as number;
  const cypher = {
    fact_vectors: await count("MATCH (v:EmbeddingVector {fact_id:$id}) RETURN count(v) AS n", { id: result.results[0]!.id }),
    episode_vectors: await count("MATCH (:Episode)--(v:EmbeddingVector) RETURN count(v) AS n"),
    episode_keyed_vectors: await count("MATCH (v:EmbeddingVector) WHERE v.episode_id IS NOT NULL RETURN count(v) AS n"),
  };
  expect(cypher).toEqual({ fact_vectors: 1, episode_vectors: 0, episode_keyed_vectors: 0 });
  if (process.env.ANAMNESIS_G4_EVIDENCE) await writeFile(process.env.ANAMNESIS_G4_EVIDENCE, JSON.stringify({ cypher, result }, null, 2) + "\n");
}, 120000);

test("window one delivers only the closest Episode on each side", async () => {
  const result = await engine.recallHybrid({ query, window: 1 }, context);

  expectWindow(result, [episodeId(2)], [episodeId(4)]);
}, 120000);

test("the byte budget trims a Fact window to the nearest two companions", async () => {
  const full = await engine.recallHybrid({ query }, context), fact = recalledFact(full);
  const nearest = { ...fact, window: { source: fact.window.source, before: fact.window.before.slice(0, 1), after: fact.window.after.slice(0, 1) } };
  const ids = [...nearest.window.before, ...nearest.window.after];
  const limit = Buffer.byteLength(renderContext([nearest], full.companions.filter(item => ids.includes(item.id))));

  const result = await engine.recallHybrid({ query, budget: { unit: "utf8_bytes", limit } }, context);

  expectWindow(result, [episodeId(2)], [episodeId(4)]);
  expect(result.used_budget).toBe(limit);
  expect(result.diagnostics.skipped_bundles).toBe(0);
  const receipt = await engine.getReceipt(result.recall_id);
  expect(receipt?.serving?.response).toEqual(result);
  expect(receipt?.serving?.result_digest).toBe(extractionBodyDigest({ results: result.results, companions: result.companions }));
}, 120000);

test("window zero keeps the Fact source attribution without window companions", async () => {
  const result = await engine.recallHybrid({ query, window: 0 }, context);

  expectWindow(result, [], []);
}, 120000);

test("a Fact found by BM25 and vector reports both lists", async () => {
  const result = await engine.recallHybrid({ query: "violet", window: 0 }, context);

  expect(recalledFact(result).channels).toEqual(["bm25", "vector", "ppr"]);
  const source = result.results.find(item => item.id === episodes[2]);
  expect(source?.kind).toBe("Episode");
  expect(source).not.toHaveProperty("window");
}, 120000);

test("Fact vectors are not searched without a selected extraction generation", async () => {
  await admin.executeQuery("MATCH (s:Meta {key:'extraction_selector'}) SET s.generation_id=null");
  try {
    const result = await engine.recallHybrid({ query }, context);

    expect(result.results).toEqual([]);
    expect(result.companions).toEqual([]);
    expect(result.diagnostics.channels_used).not.toContain("vector");
  } finally {
    await admin.executeQuery("MATCH (s:Meta {key:'extraction_selector'}) SET s.generation_id=$id", { id: generation.id });
  }
}, 120000);

test("window Episodes obey session boundaries, snapshot time, policy and invalidation", async () => {
  for (const [record, session, value] of [
    ["future", "window", "2031-01-01T00:00:00Z"],
    ["foreign", "other-session", "2026-09-01T00:00:00Z"],
  ] as const) {
    await engine.remember({ content: `${record} context`, time: { value, precision: "second" },
      origin: { source: root, session, actor: "user", record } },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
  }
  const policy_id = uuidv7(), invalidation = uuidv7();
  await engine.setPolicy({ policy_id, selector: { episode_id: episodeId(2) }, scope: "content" }, context);
  await admin.executeQuery(`MATCH (s:Episode {id:$source}),(e:Episode {id:$target})
    CREATE (s)-[:INVALIDATES {id:$id,target_id:e.id,effective_time_utc:$T}]->(e)`,
  { source: episodeId(6), target: episodeId(4), id: invalidation, T: "2026-09-01T00:00:00Z" });
  try {
    const result = await engine.recallHybrid({ query, window: 8 }, context);

    expectWindow(result, [episodeId(1)], [episodeId(5), episodeId(6)]);
  } finally {
    await engine.revokePolicy({ policy_id }, context);
    await admin.executeQuery("MATCH ()-[r:INVALIDATES {id:$id}]->() DELETE r", { id: invalidation });
  }
}, 120000);
