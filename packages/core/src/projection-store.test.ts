import { afterAll, beforeAll, expect, test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { canonicalExtractionBody, extractionBodyDigest, type Generation } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import type { ExtractionProviderInput } from "./extraction.ts";

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
if (!uri || !password) throw new Error("owned Neo4j runner required");
const context = { principal: "installation", client_binding: uuidv7() } as const;
const now = Date.UTC(2030, 0, 1);
const root = await mkdtemp(join(tmpdir(), "anamnesis-projection-store-"));
const extractionProvider = {
  model: "projection-claim-fixture", modelIncarnation: extractionBodyDigest("projection-claim-fixture"),
  async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") return { task: "claim", language: "en", modality: "text",
      claims: [{ text: input.text, confidence: 0.7, evidence: { start: 0, end: Buffer.byteLength(input.text), text: input.text } }] };
    if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
      decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};

const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider, clock: () => now });
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const generation: Generation = { id: uuidv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
  state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now };
const fixtures = [
  ["Alice", "Alice moved to Lisbon.", "2026-09-01T00:00:00Z"], ["Alice", "Alice adopted a cat.", "2026-09-02T00:00:00Z"],
  ["Alice", "Alice moved to Porto.", "2026-09-03T00:00:00Z"], ["Bob", "Bob plays the cello.", "2026-09-04T00:00:00Z"],
  ["Bob", "Bob runs marathons.", "2026-09-05T00:00:00Z"], ["Carol", "Carol writes Rust.", "2026-09-06T00:00:00Z"],
  ["Carol", "Carol keeps bees.", "2026-09-07T00:00:00Z"],
] as const;
const entities: Record<string, string> = { Alice: uuidv7(), Bob: uuidv7(), Carol: uuidv7() };
const sources: string[] = [], facts: string[] = [];

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
  for (const [index, [, content, value]] of fixtures.entries()) {
    const episode = await engine.remember({ content, time: { value, precision: "second" },
      origin: { source: root, session: "projection", actor: "user", record: String(index) } },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    sources.push(episode.id);
    facts.push(await admitFact(episode.id));
  }
  // Entity resolution is fixture state; the projection only reads Entities and their MENTIONS arcs.
  await admin.executeQuery(`UNWIND $rows AS row MERGE (e:Element:Entity {id:row.entity})
    ON CREATE SET e.content=row.name,e.generation=$generation,e.schema='anamnesis.entity/1',e.properties='{"entity_kind":"person"}'
    WITH e,row MATCH (f:Element:Fact {id:row.fact}) CREATE (f)-[:MENTIONS]->(e)`,
  { generation: generation.id, rows: fixtures.map(([name], index) => ({ entity: entities[name], name, fact: facts[index] })) });
  await admin.executeQuery(`MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:'extraction_selector'})
    SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=1`,
  { id: generation.id, body: canonicalExtractionBody({ ...generation, state: "active" }) });
  await engine.link({ id: uuidv7(), from: facts[2]!, to: facts[0]!, role: "INVALIDATES", content: "Alice moved again" });
}, 120000);
afterAll(async () => { await engine.close(); await admin.close(); await rm(root, { recursive: true }); });

const shape = async () => (await engine.projection({}, context)).entities.map(entity => [entity.name, entity.facts.map(fact => fact.id)]);

test("the projection carries every Entity with its valid Facts and leaves the invalidated Fact out", async () => {
  const snapshot = await engine.projection({}, context);
  expect(snapshot.T).toBeNull();
  expect(snapshot.entities.map(entity => [entity.name, entity.kind])).toEqual([["Alice", "person"], ["Bob", "person"], ["Carol", "person"]]);
  expect(snapshot.entities.flatMap(entity => entity.facts)).toHaveLength(6);
  expect(await shape()).toEqual([["Alice", [facts[2]!, facts[1]!]], ["Bob", [facts[4]!, facts[3]!]], ["Carol", [facts[6]!, facts[5]!]]]);
  expect(snapshot.entities[0]!.facts[0]).toMatchObject({ content: fixtures[2][1], sources: [sources[2]] });
  expect(snapshot.entities[0]!.facts[0]!.time?.value).toBe("2026-09-03T00:00:00.000Z");
}, 120000);

test("a snapshot of an unchanged graph is deep-equal across calls", async () => {
  const T = "2029-01-01T00:00:00Z";
  const first = await engine.projection({ T }, context), second = await engine.projection({ T }, context);
  expect(second).toEqual(first);
  expect(first.T).toBe("2029-01-01T00:00:00.000Z");
}, 120000);

test("Facts whose source Episode an active deny selects drop out until it is revoked", async () => {
  const policy_id = uuidv7();
  await engine.setPolicy({ policy_id, selector: { episode_id: sources[4]! }, scope: "content" }, context);
  try {
    expect(await shape()).toEqual([["Alice", [facts[2]!, facts[1]!]], ["Bob", [facts[3]!]], ["Carol", [facts[6]!, facts[5]!]]]);
  } finally {
    await engine.revokePolicy({ policy_id }, context);
  }
  expect(await shape()).toEqual([["Alice", [facts[2]!, facts[1]!]], ["Bob", [facts[4]!, facts[3]!]], ["Carol", [facts[6]!, facts[5]!]]]);
}, 120000);
