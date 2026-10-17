import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "../../packages/core/node_modules/uuid/dist/esm/index.js";
import { Engine } from "../../packages/core/src/engine.ts";
import { EmbeddingError, EmbeddingProfile, type EmbeddingProvider } from "../../packages/core/src/embedding.ts";
import { ExtractionProviderError, type ExtractionProviderInput } from "../../packages/core/src/extraction.ts";
import { extractionBodyDigest, type FactRelationContext } from "@anamnesis/protocol";

const OUT = process.env["ANAMNESIS_G8_SURFACE_OUT"]
  ?? "/Users/ino/Develop/anamnesis/.omo/evidence/ulw/01a11b8c-9f0b-71f3-ac0c-cb1048614a4e/G003-g8-judge-vector/surface";
const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "g8-relation-vector", incarnation = extractionBodyDigest(model);
const profile = EmbeddingProfile.parse({ model: "g8-relation-vector-fixture", model_incarnation: "d".repeat(64), dimensions: 2,
  document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 });

/** One owned graph and Engine; "tabs"/"spaces" embed close together, everything else orthogonal; claims starting "LO " mention Entity LO. */
async function surface(embeddings: boolean) {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const root = await mkdtemp("/tmp/g8-judge-vector-");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const judged: FactRelationContext[] = [], faults: "throw"[] = [], embedFaults = { query: false };
  const embeddingProvider: EmbeddingProvider = { profile, async embed(text, purpose) {
    if (purpose === "query" && embedFaults.query) throw new EmbeddingError("provider_unavailable");
    return text.includes("tabs") ? [1, 0] : text.includes("spaces") ? [0.8, 0.6] : [0, 1];
  } };
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") {
      const text = input.text.replace(/\.$/, ""), evidence = { start: 0, end: Buffer.byteLength(input.text), text: input.text };
      return { task: "claim", language: "en", modality: "text", claims: [{ text, evidence, confidence: 0.9,
        entities: text.startsWith("LO ") ? [{ mention: "LO", normalized_name: "LO", entity_kind: "person" }] : [] }] };
    }
    if (input.task === "judge_claims") return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context!.body_digest,
      decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.85 })) };
    if (input.task !== "judge_relations") throw new Error(`unexpected task ${input.task}`);
    const relation = input.relation_context!;
    judged.push(relation);
    if (faults.shift() === "throw") throw new ExtractionProviderError("provider_unavailable");
    return { task: "judge_relations", language: "en", modality: "text", relation_context_digest: relation.body_digest,
      judgements: relation.candidates.map(candidate => ({ candidate_id: candidate.id, relation: "unrelated", confidence: 0.2, reason: "fixture" })) };
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider, ...(embeddings ? { embeddingProvider } : {}),
    extractionJournalPath: join(root, "extraction-state.json"), materializationStatePath: join(root, "materialization-state.json") });
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const generation = { id: uuidv7(), stream: "extraction", incarnation, state: "catching_up" as const, covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  let record = 0;
  const ingest = async (content: string) => {
    const source = await engine.remember({ content, time: { value: "2026-09-10T00:00:00Z", precision: "day" as const }, origin: { source: root, session: root, actor: "user", record: String(++record) }, source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: source.id }, context);
    const result = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "g8", lease_ms: 30000 }, context);
    return { result, verdicts: (await engine.store.extractionJournal.get(task.id))?.relations?.verdicts ?? [] };
  };
  const facts = async () => (await driver.executeQuery(`MATCH (f:Fact) OPTIONAL MATCH (f)-[:MENTIONS]->(e:Entity)
    RETURN f.id AS id, f.content AS text, collect(e.id) AS entities ORDER BY text`)).records.map(row => row.toObject());
  return { engine, ingest, facts, judged, faults, embedFaults,
    async close() { await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}

test("G8 surface: entity-miss vector candidate, provider-absent fallback, digest determinism", async () => {
  await mkdir(OUT, { recursive: true });
  const miss = await surface(true);
  try {
    await miss.ingest("LO prefers tabs.");
    await miss.ingest("The weather is sunny.");
    expect(await miss.engine.drainEmbeddingOutbox()).toMatchObject({ drained: 2, quarantined: 0 });
    const spaces = await miss.ingest("the user prefers spaces.");
    const facts = await miss.facts(), tabs = facts.find(fact => fact.text === "LO prefers tabs")!;
    const claim = facts.find(fact => fact.text === "the user prefers spaces")!;
    expect(tabs.entities).toHaveLength(1); expect(claim.entities).toEqual([]);
    expect(miss.judged).toHaveLength(1);
    expect(miss.judged[0]!.candidates.map(candidate => candidate.id)).toContain(tabs.id);
    await writeFile(join(OUT, "01-entity-miss.json"), `${JSON.stringify({
      claim: { text: claim.text, entities: claim.entities }, facts, judge_input: miss.judged[0],
      recorded_vector: spaces.verdicts[0]!.vector, result: spaces.result,
      tabs_candidate: { id: tabs.id, entities: tabs.entities, shared_entities_with_claim: tabs.entities.filter((name: string) => claim.entities.includes(name)) },
    }, null, 2)}\n`);
  } finally { await miss.close(); }
  const absent = await surface(false);
  try {
    await absent.ingest("LO prefers tabs.");
    await absent.ingest("The weather is sunny.");
    const spaces = await absent.ingest("LO prefers spaces.");
    expect(spaces.verdicts.map(verdict => verdict.vector)).toEqual([{ reason: "provider_absent" }]);
    await writeFile(join(OUT, "02-provider-absent.json"), `${JSON.stringify({
      recorded_vector: spaces.verdicts[0]!.vector, candidate_count: spaces.verdicts[0]!.context.candidates.length,
      candidates: spaces.verdicts[0]!.context.candidates, judge_inputs: absent.judged, result: spaces.result,
    }, null, 2)}\n`);
  } finally { await absent.close(); }
  const digest = await surface(true);
  try {
    await digest.ingest("LO prefers tabs.");
    expect(await digest.engine.drainEmbeddingOutbox()).toMatchObject({ drained: 1, quarantined: 0 });
    digest.faults.push("throw");
    const first = await digest.ingest("LO prefers spaces.");
    digest.embedFaults.query = true;
    const second = await digest.ingest("LO prefers spaces.");
    const [a, b] = [first.verdicts[0]!, second.verdicts[0]!];
    expect(b.context.body_digest).toBe(a.context.body_digest);
    const { body_digest, ...body } = a.context;
    await writeFile(join(OUT, "03-digest-determinism.json"), `${JSON.stringify({
      runs: [a, b].map(verdict => ({ body_digest: verdict.context.body_digest, vector: verdict.vector, candidate_ids: verdict.context.candidates.map(candidate => candidate.id) })),
      equal: a.context.body_digest === b.context.body_digest, recomputed: extractionBodyDigest(body), recomputed_equal: extractionBodyDigest(body) === body_digest,
      judge_input_digests: digest.judged.map(input => input.body_digest),
    }, null, 2)}\n`);
  } finally { await digest.close(); }
}, 300000);
