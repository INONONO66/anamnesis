import { afterAll, beforeAll, expect, test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { canonicalExtractionBody, extractionBodyDigest, type Generation, type RpcRecallResult } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { EmbeddingProfile, type EmbeddingProvider } from "./embedding.ts";
import type { ExtractionProviderInput } from "./extraction.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned Neo4j runner required");
const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const DAY = 86400000, now = Date.UTC(2030, 0, 1);
let clock = now;
const root = await mkdtemp(join(tmpdir(), "anamnesis-recall-fact-score-"));
const provider: EmbeddingProvider = {
  profile: EmbeddingProfile.parse({ model: "recall-fact-score-fixture", model_incarnation: "f".repeat(64), dimensions: 2,
    document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 }),
  async embed(text) { return text.includes("kestrel") ? [0, 1] : [1, 0]; },
};
const extractionProvider = {
  model: "recall-fact-score-claim-fixture", modelIncarnation: extractionBodyDigest("recall-fact-score-claim-fixture"),
  async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
      claims: [{ text: input.text, confidence: 0.7, evidence: { start: 0, end: Buffer.byteLength(input.text), text: input.text } }] };
    if (input.task === "judge_relations" && input.relation_context) return { task: "judge_relations", language: "en", modality: "text",
      relation_context_digest: input.relation_context.body_digest,
      judgements: input.relation_context.candidates.map(({ id }) => ({
        candidate_id: id, relation: "unrelated", confidence: 1, reason: "distinct fixture occurrences",
      })) };
    if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
      decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};

const engine = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider, extractionProvider,
  clock: () => clock });
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
  state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now };
const contents = { vectorHit: "The zephyr note alpha.", unembedded: "The zephyr note beta.",
  idle: "The kestrel note gamma.", adopted: "The kestrel note delta." } as const;
type Key = keyof typeof contents;
const sources = {} as Record<Key, string>, facts = {} as Record<Key, string>;

async function admitFact(sourceId: string) {
  const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: sourceId }, context);
  const audit = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
  if (audit.state !== "known" || !audit.semantic_writes) throw new Error("Fact extraction failed");
  const custody = await engine.store.materializationState.byOccurrence(extractionBodyDigest([generation.id, sourceId]));
  const id = custody?.fact_ids[0];
  if (!id || custody?.fact_ids.length !== 1) throw new Error("expected one extracted Fact");
  return id;
}

beforeAll(async () => {
  await engine.init(); await engine.claimWriterEpoch();
  await engine.store.createExtractionGeneration(generation, context);
  for (const key of ["vectorHit", "idle", "adopted", "unembedded"] as const) {
    const episode = await engine.remember({ content: contents[key], time: { value: "2026-09-01T00:00:00Z", precision: "second" },
      origin: { source: root, session: "fact-score", actor: "user", record: key } },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    sources[key] = episode.id;
    facts[key] = await admitFact(episode.id);
    // The unembedded Fact is admitted after the only drain, so it never enters the vector channel.
    if (key === "adopted") expect(await engine.drainEmbeddingOutbox()).toEqual({ drained: 3, quarantined: 0, deferred: 0, deferral_reason: null });
  }
  // Serving selection is fixture state; this test does not bypass or exercise the activation coverage gate.
  await admin.executeQuery(`MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:'extraction_selector'})
    SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=1`,
  { id: generation.id, body: canonicalExtractionBody({ ...generation, state: "active" }) });
}, 120000);
afterAll(async () => { await engine.close(); await admin.close(); await rm(root, { recursive: true }); });

async function scored(result: RpcRecallResult, keys: Key[]) {
  const candidates = (await engine.getReceipt(result.recall_id))?.serving?.candidates ?? [];
  const dump = JSON.stringify(Object.fromEntries(keys.map(key => [key, candidates.find(candidate => candidate.id === facts[key]) ?? null])));
  const items = keys.map(key => {
    const item = result.results.find(candidate => candidate.id === facts[key]);
    assert(item?.kind === "Fact", `${key} missing from results: ${dump}`);
    const candidate = candidates.find(row => row.id === item.id);
    assert.deepEqual(candidate, { id: item.id, score: item.score, relevance: item.relevance, mass: item.mass, utility: item.utility }, dump);
    return item;
  });
  return { items, dump };
}

test("a vector rank-1 Fact outranks an equal-confidence Fact absent from that channel", async () => {
  const result = await engine.recallHybrid({ query: "zephyr", window: 0 }, context);

  const { items: [vectorHit, unembedded], dump } = await scored(result, ["vectorHit", "unembedded"]);
  assert(vectorHit && unembedded);
  expect(vectorHit.channels).toEqual(["bm25", "vector", "ppr"]);
  expect(unembedded.channels).toEqual(["bm25", "ppr"]);
  assert.equal(vectorHit.mass, unembedded.mass, dump);
  assert.ok(vectorHit.relevance > unembedded.relevance, dump);
  assert.ok(vectorHit.score > unembedded.score, dump);
  assert.ok(result.results.indexOf(vectorHit) < result.results.indexOf(unembedded), dump);
}, 120000);

test("a Fact whose source Episode was adopted has strictly higher mass and score", async () => {
  clock = now + 365 * DAY;
  const identity = await engine.recallHybrid({ query: sources.adopted, window: 0, limit: 64 }, context);
  expect(identity.results.map(item => item.id)).toContain(sources.adopted);
  const commit = await engine.commitReceipt({ operation_id: uuidv7(), recall_id: identity.recall_id, adopted: [sources.adopted] }, context);
  expect(commit.applied).toBe(true);
  clock = now + 730 * DAY;

  const result = await engine.recallHybrid({ query: "kestrel", window: 0 }, context);

  const { items: [idle, adopted], dump } = await scored(result, ["idle", "adopted"]);
  assert(idle && adopted);
  expect(idle.channels).toEqual(adopted.channels);
  assert.ok(adopted.mass > idle.mass, dump);
  assert.ok(adopted.score > idle.score, dump);
}, 120000);
