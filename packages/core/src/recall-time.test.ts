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
import { renderContext } from "./recall.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned Neo4j runner required");
const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const now = Date.UTC(2030, 0, 1), query = "wallaby", K = now + 10;
const root = await mkdtemp(join(tmpdir(), "anamnesis-recall-time-"));
const provider: EmbeddingProvider = {
  profile: EmbeddingProfile.parse({ model: "recall-time-fixture", model_incarnation: "f".repeat(64), dimensions: 2,
    document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 }),
  async embed() { return [1, 0]; },
};
const extractionProvider = {
  model: "recall-time-claim-fixture", modelIncarnation: extractionBodyDigest("recall-time-claim-fixture"),
  async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
      claims: [{ text: input.text, confidence: 0.7, evidence: { start: 0, end: Buffer.byteLength(input.text), text: input.text } }] };
    if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
      decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};

const engine = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider, extractionProvider,
  clock: () => now });
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
  state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now };
const days = { a: "2026-09-01T00:00:00Z", b: "2026-09-02T00:00:00Z", c: "2026-09-03T00:00:00Z" } as const;
const sources: Record<"a" | "b" | "c", string> = { a: "", b: "", c: "" };
const facts: Record<"a" | "b" | "c", string> = { a: "", b: "", c: "" };

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
  for (const key of ["a", "b", "c"] as const) {
    const content = `The wallaby budget on day ${key} is set.`;
    const episode = await engine.remember({ content, time: { value: days[key], precision: "second" },
      origin: { source: root, session: "time", actor: "user", record: key } },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    sources[key] = episode.id;
    facts[key] = await admitFact(episode.id);
  }
  expect(await engine.drainEmbeddingOutbox()).toEqual({ drained: 3, quarantined: 0, deferred: 0, deferral_reason: null });
  // Serving selection is fixture state; this test does not bypass or exercise the activation coverage gate.
  await admin.executeQuery(`MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:'extraction_selector'})
    SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=1`,
  { id: generation.id, body: canonicalExtractionBody({ ...generation, state: "active" }) });
  // Materialization times are fixture state: Facts a and c are known at K, Fact b only from K+1.
  await admin.executeQuery("UNWIND $rows AS row MATCH (f:Fact {id:row.id}) SET f.ingested_at=row.at",
    { rows: [{ id: facts.a, at: K }, { id: facts.b, at: K + 1 }, { id: facts.c, at: K }] });
  await engine.link({ id: uuidv7(), from: facts.b, to: facts.a, role: "INVALIDATES", content: "The day b budget replaces day a" });
  const link = await admin.executeQuery(`MATCH (:Fact {id:$from})-[l:INVALIDATES]->(:Fact {id:$to}) RETURN l.ingested_at AS ingested`,
    { from: facts.b, to: facts.a });
  expect(link.records.map(row => row.toObject())).toEqual([{ ingested: K + 1 }]);
}, 120000);
afterAll(async () => { await engine.close(); await admin.close(); await rm(root, { recursive: true }); });

const primaryIds = (result: RpcRecallResult) => result.results.map(item => item.id);

test("a time window serves only items whose time_utc lies inside it", async () => {
  const from = Date.parse(days.a), to = Date.parse("2026-09-02T23:59:59Z");
  const result = await engine.recallHybrid({ query, window: 0, time: { from, to } }, context);

  for (const item of [...result.results, ...result.companions]) expect(Date.parse(item.time.value)).toBeWithin(from, to + 1);
  expect(primaryIds(result).sort()).toEqual([sources.a, sources.b, facts.b].sort());
  const receipt = await engine.getReceipt(result.recall_id);
  expect(receipt?.serving?.candidates.map(candidate => candidate.id).sort()).toEqual([sources.a, sources.b, facts.b].sort());
}, 120000);

test("a window ending after T or starting after its end is rejected as invalid params", async () => {
  const T = Date.parse(days.c);
  await expect(engine.recallHybrid({ query, T, time: { to: T + 1 } }, context)).rejects.toMatchObject({ code: "invalid_params" });
  await expect(engine.recallHybrid({ query, time: { from: Date.parse(days.c), to: Date.parse(days.b) } }, context))
    .rejects.toMatchObject({ code: "invalid_params" });
  await expect(engine.recallHybrid({ query, T, time: { from: Date.parse(days.a), to: T } }, context)).resolves.toBeDefined();
}, 120000);

test("chronological order presents the relevance selection by ascending time_utc with contiguous ranks", async () => {
  const relevance = await engine.recallHybrid({ query, window: 0 }, context);
  const result = await engine.recallHybrid({ query, window: 0, order: "chronological" }, context);

  expect(result.results.length).toBeGreaterThan(2);
  expect(primaryIds(result).sort()).toEqual(primaryIds(relevance).sort());
  const keys = result.results.map(item => `${new Date(item.time.value).toISOString()} ${item.id}`);
  expect(keys).toEqual([...keys].sort());
  expect(result.results.map(item => item.rank)).toEqual(result.results.map((_, index) => index));
  expect(result.context_text).toBe(renderContext(result.results, result.companions));
  expect(result.used_budget).toBe(Buffer.byteLength(result.context_text));
}, 120000);

test("known_by hides a Fact materialized after it", async () => {
  const known = await engine.recallHybrid({ query, window: 0, known_by: K }, context);
  const later = await engine.recallHybrid({ query, window: 0, known_by: K + 1 }, context);

  expect(primaryIds(known)).not.toContain(facts.b);
  expect(primaryIds(known)).toContain(facts.c);
  expect(primaryIds(later)).toContain(facts.b);
}, 120000);

test("known_by keeps an invalidated Fact valid until its INVALIDATES link is known", async () => {
  const known = await engine.recallHybrid({ query, window: 0, known_by: K }, context);
  const later = await engine.recallHybrid({ query, window: 0, known_by: K + 1 }, context);

  const earlier = known.results.find(item => item.id === facts.a);
  assert(earlier?.kind === "Fact", JSON.stringify(known.results));
  expect(primaryIds(later)).not.toContain(facts.a);
  const replacing = later.results.find(item => item.id === facts.b);
  assert(replacing?.kind === "Fact", JSON.stringify(later.results));
  expect(replacing.provenance.supersedes).toEqual([{ id: facts.a, content: "The wallaby budget on day a is set." }]);
}, 120000);
