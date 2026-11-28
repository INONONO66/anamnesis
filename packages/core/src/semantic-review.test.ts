import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import type { ExtractionProviderInput } from "./extraction.ts";
import { canonicalExtractionBody, extractionBodyDigest, ExtractionModelOutput, type Generation } from "../../protocol/src/extraction.ts";

const binding = uuidv7();
const context = { principal: "installation", client_binding: binding } as const;
const sentences = ["Alice prefers dark mode", "Alice prefers compact dark mode"];
const text = sentences.join(". ") + ".";
const spans = sentences.map(sentence => { const start = Buffer.byteLength(text.slice(0, text.indexOf(sentence))); return { start, end: start + Buffer.byteLength(sentence) }; });
const model = "qa-audit-judge";
const incarnation = extractionBodyDigest(model);
const EN = "en";

const extractionProvider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
  if (input.task === "claim") return { task: "claim", language: EN, modality: "text", claims: sentences.map((sentence, i) => ({ text: sentence, evidence: { ...spans[i]!, text: sentence } })) };
  if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
  return { task: "judge_claims", language: EN, modality: "text", claim_body_digest: input.claim_context.body_digest,
    decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
} };

async function setup() {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "semantic-review-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}): Promise<Record<string, unknown>[]> =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const materializationStatePath = join(root, "materialization-state.json");
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider, materializationStatePath });
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
  const generationIn = async (state: Generation["state"]) => {
    const created = await engine.store.createExtractionGeneration({ ...generation, id: uuidv7(), state: "catching_up" }, context);
    const moved: Generation = { ...created, state };
    await query("MATCH (g:ExtractionGeneration {id:$id}) SET g.state=$state, g.body=$body", { id: moved.id, state, body: canonicalExtractionBody(moved) });
    return moved;
  };
  return { engine, query, generation, episode, first, generationIn,
    async close() { await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
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

test("cutover exposes selected coverage and rollback reopens only a retired target", async () => {
  const f = await setup();
  try {
    const id = f.generation.id;
    for (const partition of ["episodes", "active_extraction"] as const) await f.engine.store.recordExtractionCoverage({ generation_id: id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
    const activated = await f.engine.cutoverExtractionGeneration({ generation_id: id, expected_generation_id: null, expected_selector_version: 0 }, context);
    expect(activated.state).toBe("active");
    expect(await f.engine.readExtractionSelection(context)).toEqual({ generation_id: id, selector_version: 1 });
    const coverage = await f.engine.readExtractionCoverage({ generation_id: id, expected_selector_version: 1 }, context);
    expect(coverage).toEqual({ generation_id: id, selector_version: 1, required_ingest_seq: 1, covered_ingest_seq: 1, omission_digest: expect.stringMatching(/^[0-9a-f]{64}$/), policy_revision: 0, read_at: expect.any(Number) });
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
