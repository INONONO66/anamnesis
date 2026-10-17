import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { extractionBodyDigest, type FactRelationContext } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { EmbeddingError, EmbeddingProfile, embeddingProfileId, type EmbeddingProvider } from "./embedding.ts";
import { ExtractionProviderError, type ExtractionProviderInput } from "./extraction.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "qa-relation-vector", incarnation = extractionBodyDigest(model);
const lo = { mention: "LO", normalized_name: "LO", entity_kind: "person" };
const profile = EmbeddingProfile.parse({ model: "relation-vector-fixture", model_incarnation: "c".repeat(64), dimensions: 2,
  document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001 });

async function setup(embeddings: boolean) {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "fact-relation-vector-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI!, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD!;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}) =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const relationInputs: FactRelationContext[] = [], faults: "throw"[] = [], embedFaults = { query: false };
  // "tabs" and "spaces" embed close together; anything else is orthogonal to both.
  const embeddingProvider: EmbeddingProvider = { profile, async embed(text, purpose) {
    if (purpose === "query" && embedFaults.query) throw new EmbeddingError("provider_unavailable");
    return text.includes("tabs") ? [1, 0] : text.includes("spaces") ? [0.8, 0.6] : [0, 1];
  } };
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") {
      const text = input.text.replace(/\.$/, ""), evidence = { start: 0, end: Buffer.byteLength(input.text), text: input.text };
      return { task: "claim", language: "en", modality: "text", claims: [{ text, evidence, confidence: 0.9, entities: text.startsWith("LO ") ? [lo] : [] }] };
    }
    if (input.task === "judge_claims") return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context!.body_digest,
      decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.85 })) };
    if (input.task !== "judge_relations") throw new Error(`unexpected task ${input.task}`);
    const relation = input.relation_context!;
    relationInputs.push(relation);
    if (faults.shift() === "throw") throw new ExtractionProviderError("provider_unavailable");
    return { task: "judge_relations", language: "en", modality: "text", relation_context_digest: relation.body_digest,
      judgements: relation.candidates.map(candidate => ({ candidate_id: candidate.id, relation: "unrelated", confidence: 0.2, reason: `${relation.fact.text} vs ${candidate.text}` })) };
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider, ...(embeddings ? { embeddingProvider } : {}),
    extractionJournalPath: join(root, "extraction-state.json"), materializationStatePath: join(root, "materialization-state.json") });
  await query("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const generation = { id: uuidv7(), stream: "extraction", incarnation, state: "catching_up" as const, covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  let record = 0;
  const ingest = async (content: string) => {
    const source = await engine.remember({ content, time: { value: "2026-09-10T00:00:00Z", precision: "day" as const }, origin: { source: root, session: root, actor: "user", record: String(++record) }, source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: source.id }, context);
    const result = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
    return { result, verdicts: (await engine.store.extractionJournal.get(task.id))?.relations?.verdicts ?? [] };
  };
  const factId = async (content: string) => {
    const rows = await query("MATCH (f:Fact {content:$content}) RETURN f.id AS id", { content });
    expect(rows).toHaveLength(1);
    return rows[0]!.id as string;
  };
  return { engine, ingest, factId, relationInputs, faults, embedFaults,
    async close() { await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}

test("an entity-less claim reaches the relation judge through its vector neighbours, nearest first", async () => {
  const f = await setup(true);
  try {
    const profileId = embeddingProfileId(profile);
    const tabs = await f.ingest("LO prefers tabs.");
    expect(tabs.verdicts.map(verdict => [verdict.context.candidates.length, verdict.vector])).toEqual([[0, { profile_id: profileId, candidates: 0 }]]);
    await f.ingest("The weather is sunny.");
    expect(await f.engine.drainEmbeddingOutbox()).toEqual({ drained: 2, quarantined: 0, deferred: 0, deferral_reason: null });
    const spaces = await f.ingest("the user prefers spaces.");
    expect(spaces.result).toMatchObject({ state: "known", semantic_writes: true, relation_judge: "complete" });
    const [tabsId, weatherId] = [await f.factId("LO prefers tabs"), await f.factId("The weather is sunny")];
    expect(f.relationInputs).toHaveLength(1);
    expect(f.relationInputs[0]!.candidates.map(candidate => candidate.id)).toEqual([tabsId, weatherId]);
    expect(spaces.verdicts).toHaveLength(1);
    const [verdict] = spaces.verdicts;
    expect(verdict!.vector).toEqual({ profile_id: profileId, candidates: 2 });
    expect(verdict!.context).toEqual(f.relationInputs[0]!);
    const { body_digest, ...body } = verdict!.context;
    expect(body_digest).toBe(extractionBodyDigest(body));
  } finally { await f.close(); }
}, 180000);

test("without an embedding provider the candidates stay entity-only and the premise records provider_absent", async () => {
  const f = await setup(false);
  try {
    await f.ingest("LO prefers tabs.");
    await f.ingest("The weather is sunny.");
    const spaces = await f.ingest("LO prefers spaces.");
    expect(spaces.result).toMatchObject({ state: "known", semantic_writes: true, relation_judge: "complete" });
    expect(f.relationInputs.map(input => input.candidates.map(candidate => candidate.id))).toEqual([[await f.factId("LO prefers tabs")]]);
    expect(spaces.verdicts.map(verdict => verdict.vector)).toEqual([{ reason: "provider_absent" }]);
    expect(spaces.verdicts.every(verdict => verdict.context.candidates.length <= 16)).toBe(true);
  } finally { await f.close(); }
}, 180000);

test("the premise digest depends only on the Fact and its candidates, not on the vector channel outcome", async () => {
  const f = await setup(true);
  try {
    await f.ingest("LO prefers tabs.");
    expect(await f.engine.drainEmbeddingOutbox()).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    // The first judgement fails so its Fact stays unwritten and both runs see the same candidate set.
    f.faults.push("throw");
    const first = await f.ingest("LO prefers spaces.");
    expect(first.result).toMatchObject({ state: "known", semantic_writes: false, relation_judge: "pending" });
    f.embedFaults.query = true;
    const second = await f.ingest("LO prefers spaces.");
    const tabsId = await f.factId("LO prefers tabs");
    expect([...first.verdicts, ...second.verdicts].map(verdict => [verdict.context.candidates.map(candidate => candidate.id), verdict.vector]))
      .toEqual([[[tabsId], { profile_id: embeddingProfileId(profile), candidates: 0 }], [[tabsId], { reason: "embed_failed" }]]);
    expect(second.verdicts[0]!.context.body_digest).toBe(first.verdicts[0]!.context.body_digest);
    expect(f.relationInputs.map(input => input.body_digest)).toEqual([first.verdicts[0]!.context.body_digest, first.verdicts[0]!.context.body_digest]);
  } finally { await f.close(); }
}, 180000);
