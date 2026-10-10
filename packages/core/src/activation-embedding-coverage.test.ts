import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { extractionBodyDigest, type SemanticClaim } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { EmbeddingProfile, type EmbeddingProvider } from "./embedding.ts";
import type { ExtractionProviderInput } from "./extraction.ts";
import type { SemanticReviewProvider } from "../../protocol/src/materialization.ts";
import { GenerationReadinessError } from "./store/extraction-store.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const content = "Alice prefers dark mode";
const model = "activation-coverage-fixture";
const incarnation = extractionBodyDigest(model);
const profile = EmbeddingProfile.parse({
  model: "activation-embedding-fixture", model_incarnation: "a".repeat(64), dimensions: 2,
  document_prefix: "", query_prefix: "", max_input_bytes: 8192, norm: "unit_l2", norm_tolerance: 0.001,
});
const embeddingProvider: EmbeddingProvider = { profile, async embed() { return [1, 0]; } };
const extractionProvider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
  if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
    claims: [{ text: content, evidence: { start: 0, end: Buffer.byteLength(content), text: content } }] };
  if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
  return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
    decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
} };
const semanticReviewProvider: SemanticReviewProvider = {
  profileId: extractionBodyDigest("activation-review-fixture"),
  async resolve() { return { entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false, content_language: "en" }; },
  async review(input) {
    return { disposition: "retain", semantic_claim: input.premises.request.semantic_claim, reason: "fixture retained" };
  },
};

async function setup(withLineage: boolean) {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "activation-coverage-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const engine = new Engine({ uri, password, objectsRoot: root, embeddingProvider, extractionProvider, semanticReviewProvider,
    extractionJournalPath: join(root, "extraction-state.json"), materializationStatePath: join(root, "materialization-state.json"),
    embeddingLedgerPath: join(root, "embedding-state.json") });
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  await engine.init();
  await engine.claimWriterEpoch();
  const generation = { id: uuidv7(), stream: "extraction" as const, incarnation, state: "catching_up" as const,
    covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  const input = { content, time: { value: "2026-09-01T00:00:00Z", precision: "day" as const },
    origin: { source: root, session: root, actor: "user", record: "one" }, source_revision: "v1", expected_previous_revision_key: null };
  const source = withLineage
    ? await engine.remember(input, { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context })
    : await engine.remember(input);
  const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: source.id }, context);
  const result = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
  if (result.state !== "known" || result.judge_attempt?.state !== "succeeded") throw new Error("completed fixture pipeline required");
  if (withLineage) {
    const episode = await engine.store.semanticEpisode(source.id, context);
    const semantic: SemanticClaim = {
      content, content_language: "en", sub_kind: "fact", modality: "asserted", confidence: 0.7,
      time: { time_value: episode.time.time_value, time_utc: episode.time.time_utc, time_precision: "inherited",
        resolution: "inherited", anchor_time_utc: episode.time.time_utc },
      entities: [], subject_keys: null, predicate_text: content, scope_complete: false,
      scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
      evidence: { kind: "source_locus", span: { start: 0, end: Buffer.byteLength(content) } },
    };
    const request = { proposal_id: uuidv7(), generation_id: generation.id, source_episode_id: source.id,
      judge_attempt_id: result.judge_attempt.id, claim_index: 0, semantic_claim: semantic };
    await engine.proposeRetainedClaim(request, context);
    await engine.reviewRetainedClaim({ review_id: uuidv7(), proposal_id: request.proposal_id,
      action: "accept", reason: "fixture accepted" }, context);
    await engine.materializeRetainedClaim({ ...request, operation_id: uuidv7() }, context);
  }
  for (const partition of ["episodes", "active_extraction"] as const)
    await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition,
      expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
  const count = async () => (await driver.executeQuery(
    "MATCH (f:Fact {generation:$generation}) RETURN count(f) AS count", { generation: generation.id })).records[0]?.get("count");
  const cutover = async (): Promise<readonly string[]> => {
    try {
      await engine.store.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: null,
        expected_selector_version: 0 }, context);
      return [];
    } catch (error) {
      if (error instanceof GenerationReadinessError && error.code === "activation_prerequisite_unavailable") return error.prerequisites;
      throw error;
    }
  };
  return { engine, count, cutover, result, async close() {
    await engine.close();
    await driver.close();
    await rm(root, { recursive: true, force: true });
  } };
}

test("activation covers generation Facts, including an empty generation and a Fact embedded after a failed cutover", async () => {
  // Given a covered source without an admitted Fact, its Episode has no vector.
  const empty = await setup(false);
  let a: readonly string[];
  try {
    expect(await empty.count()).toBe(0);
    expect(empty.result.semantic_writes).toBe(false);
    // When cutover checks the generation, zero Facts need no embeddings.
    a = await empty.cutover();
    expect(a).toEqual([]);
  } finally { await empty.close(); }

  // Given a generation with one materialized Fact and no embedding vector.
  const filled = await setup(true);
  try {
    expect(await filled.count()).toBe(1);
    // When cutover checks the still-unembedded Fact, its missing vector is the only cause.
    const b = await filled.cutover();
    expect(b).toEqual(["selected_model_embedding_coverage"]);

    // When the real outbox embeds that Fact, the same cutover succeeds.
    expect(await filled.engine.drainEmbeddingOutbox(100)).toMatchObject({ drained: 1 });
    const c = await filled.cutover();
    expect(c).toEqual([]);
    const path = process.env.ANAMNESIS_G4_ACTIVATION_EVIDENCE;
    if (path) {
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, JSON.stringify({ a, b, c }, null, 2) + "\n");
    }
  } finally { await filled.close(); }
}, 180000);
