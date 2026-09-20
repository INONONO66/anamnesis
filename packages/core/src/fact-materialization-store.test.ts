import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import type { ExtractionProviderInput } from "./extraction.ts";
import { extractionBodyDigest } from "../../protocol/src/extraction.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const text = "Alice prefers dark mode. Alice prefers compact dark mode.";
const evidence = { start: 0, end: Buffer.byteLength(text), text };
const model = "qa-fidelity-judge";
const incarnation = extractionBodyDigest(model);

async function setup(metadata: boolean, lineage = true) {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "fact-unify-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI!, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD!;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}) =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text", claims: [
      { text: "Alice prefers dark mode", evidence, ...(metadata ? { confidence: 0.72, entities: [{ mention: "Alice", normalized_name: "Alice", entity_kind: "person" }], time: { value: "2025-01-01T00:00:00Z", precision: "year" } } : {}) },
      { text: "Alice prefers compact dark mode", evidence, ...(metadata ? { confidence: 0.63, entities: [{ mention: "Alice", normalized_name: "Alice", entity_kind: "person" }] } : {}) },
    ] };
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context!.body_digest,
      decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence,
        ...(metadata && claim_index === 0 ? { confidence: 0.81 } : {}) })) };
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider });
  await query("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const generation = { id: uuidv7(), stream: "extraction", incarnation, state: "catching_up" as const, covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  const episode = { content: text, time: { value: "2026-09-01T00:00:00Z", precision: "day" as const }, origin: { source: root, session: root, actor: "user", record: "one" }, source_revision: "v1", expected_previous_revision_key: null };
  // Metadata-free remember is the pre-lineage import contract; the explicit path needs all three admission fields.
  const source = lineage ? await engine.remember(episode, { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context }) : await engine.remember(episode);
  const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: source.id }, context);
  const run = () => engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
  return { engine, query, source, generation, run, async close() { await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}

test("legacy audit claims do not fabricate confidence, entities or semantic permission", async () => {
  const f = await setup(false);
  try {
    const result = await f.run();
    expect(result.state === "known" && result.judge?.state).toBe("succeeded");
    expect(result.state === "known" && result.semantic_writes).toBe(false);
    expect(await f.query("MATCH (f:Fact) RETURN count(f) AS count")).toEqual([{ count: 0 }]);
    expect(await f.query("MATCH (e:Entity) RETURN count(e) AS count")).toEqual([{ count: 0 }]);
    // A source whose judge admitted nothing still needs materialization custody, or the generation can never activate.
    expect(await f.query("MATCH (o:MaterializationOperation) RETURN o.fact_id AS fact, o.result AS result")).toEqual([{ fact: `suppressed:${f.source.id}`, result: JSON.stringify({ created: false, facts: 0, refused: [] }) }]);
    for (const partition of ["episodes", "active_extraction"] as const) await f.engine.store.recordExtractionCoverage({ generation_id: f.generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
    await f.engine.store.cutoverExtractionGeneration({ generation_id: f.generation.id, expected_generation_id: null, expected_selector_version: 0 }, context);
    expect(await f.query("MATCH (g:ExtractionGeneration) RETURN g.state AS state")).toEqual([{ state: "active" }]);
  } finally { await f.close(); }
}, 120000);

test("pre-lineage (metadata-free) Episodes refuse semantic Facts but keep custody", async () => {
  const f = await setup(true, false);
  try {
    const result = await f.run();
    expect(result.state === "known" && result.judge?.state).toBe("succeeded");
    expect(await f.query("MATCH (f:Fact) RETURN count(f) AS count")).toEqual([{ count: 0 }]);
    const operations = await f.query("MATCH (o:MaterializationOperation) RETURN o.fact_id AS fact, o.result AS result ORDER BY o.fact_id");
    expect(operations.filter(op => String(op.fact).startsWith("refused:")).map(op => JSON.parse(String(op.result)))).toEqual([
      { created: false, refused: "echo_lineage_unavailable" }, { created: false, refused: "echo_lineage_unavailable" }]);
    expect(operations.filter(op => op.fact === `suppressed:${f.source.id}`).map(op => JSON.parse(String(op.result)))).toEqual([{ created: false, facts: 0, refused: ["echo_lineage_unavailable", "echo_lineage_unavailable"] }]);
    for (const partition of ["episodes", "active_extraction"] as const) await f.engine.store.recordExtractionCoverage({ generation_id: f.generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
    await f.engine.store.cutoverExtractionGeneration({ generation_id: f.generation.id, expected_generation_id: null, expected_selector_version: 0 }, context);
    expect(await f.query("MATCH (g:ExtractionGeneration) RETURN g.state AS state")).toEqual([{ state: "active" }]);
  } finally { await f.close(); }
}, 120000);

test("judge-approved claims share validated Fact writes, real entities, time and conducting custody", async () => {
  const f = await setup(true);
  try {
    const result = await f.run();
    expect(result.state === "known" && result.judge?.state).toBe("succeeded");
    expect(result.state === "known" && result.semantic_writes).toBe(true);
    const facts = await f.query("MATCH (f:Fact) RETURN properties(f) AS p ORDER BY f.content");
    expect(facts).toHaveLength(2);
    const compact = facts[0]!.p, dark = facts[1]!.p;
    expect([compact.mass, dark.mass]).toEqual([0.63, 0.81]);
    for (const { p } of facts) {
      expect(p.mass).toBe(p.confidence);
      expect(p.origin_actor).toBe(model);
      expect(p.origin_actor).not.toBe("fixture");
      expect(p.meaning_digest).toMatch(/^[a-f0-9]{64}$/);
    }
    expect(dark.time_utc).toBe("2025-01-01T00:00:00.000Z"); expect(dark.time_precision).toBe("year");
    expect(compact.time_precision).toBe("day");
    expect(JSON.parse(compact.properties).time_basis).toBe("episode_fallback");
    expect(JSON.parse(dark.properties).time_basis).toBe("claim");
    const entities = await f.query("MATCH (e:Entity) RETURN properties(e) AS p");
    expect(entities).toHaveLength(1);
    expect(entities[0]!.p.content).toBe("Alice");
    expect(JSON.parse(entities[0]!.p.properties).entity_kind).toBe("person");
    expect(facts.every(({ p }) => p.content !== entities[0]!.p.content)).toBe(true);
    expect(await f.query("MATCH (:Fact)-[l:MENTIONS]->(:Entity) RETURN count(l) AS count")).toEqual([{ count: 2 }]);
    expect(await f.query("MATCH (:Fact)-[l:CONTRASTS|INVALIDATES]->(:Fact) RETURN count(l) AS count")).toEqual([{ count: 0 }]);
    expect(await f.query("MATCH (:Fact)-[l:DERIVED_FROM]->(:Episode) RETURN l.span AS span,l.evidence_text AS text")).toEqual([{ span: [0, evidence.end], text }, { span: [0, evidence.end], text }]);
    expect((await f.engine.checkConductingArcs()).issues).toEqual([]);
    expect(await f.query("MATCH (a:ConductingArc) RETURN count(a) AS count")).toEqual([{ count: 8 }]);
    expect(await f.run()).toEqual(result);
    expect(await f.query("MATCH (f:Fact) RETURN count(f) AS count")).toEqual([{ count: 2 }]);
    expect(await f.query("MATCH (o:MaterializationOperation) RETURN count(o) AS count")).toEqual([{ count: 3 }]);
    expect(await f.query("MATCH (o:MaterializationOperation) WHERE o.fact_id STARTS WITH 'custody:' RETURN o.result AS result")).toEqual([{ result: JSON.stringify({ created: true, facts: 2, refused: [] }) }]);
    for (const partition of ["episodes", "active_extraction"] as const) await f.engine.store.recordExtractionCoverage({ generation_id: f.generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
    await f.engine.store.cutoverExtractionGeneration({ generation_id: f.generation.id, expected_generation_id: null, expected_selector_version: 0 }, context);
    const recalled = await f.engine.recallHybrid({ query: "Alice dark mode", T: Date.parse("2027-01-01T00:00:00Z"), limit: 8 }, context);
    const derived = recalled.results.filter(item => item.kind === "Fact");
    expect(derived).toHaveLength(2);
    expect(derived.every(item => item.provenance.derived_from.some(source => source.id === f.source.id))).toBe(true);
    expect(derived.every(item => item.channels.includes("bm25") && item.relevance > 0)).toBe(true);
    expect(recalled.diagnostics.ppr_used).toBe(true);
  } finally { await f.close(); }
}, 120000);
