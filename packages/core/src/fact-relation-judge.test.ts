import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import { ExtractionProviderError, type ExtractionProviderInput } from "./extraction.ts";
import { extractionBodyDigest, type FactRelationKind } from "../../protocol/src/extraction.ts";
import type { FactRelationContext } from "../../protocol/src/extraction-audit.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "qa-relation-judge", incarnation = extractionBodyDigest(model);
const alice = { mention: "Alice", normalized_name: "Alice", entity_kind: "person" };

/** Scripted verdicts keyed by (new fact text, candidate text). Anything else is unrelated. */
const script: Record<string, Record<string, { relation: FactRelationKind; confidence: number }>> = {
  "Alice prefers light mode": { "Alice prefers dark mode": { relation: "invalidates", confidence: 0.9 } },
  "Alice prefers dark mode again": { "Alice prefers light mode": { relation: "invalidates", confidence: 0.95 } },
  "Alice likes dark mode": { "Alice prefers dark mode again": { relation: "duplicate", confidence: 0.9 }, "Alice prefers light mode": { relation: "unrelated", confidence: 0.3 } },
  "Alice might prefer compact mode": { "Alice prefers dark mode again": { relation: "contrasts", confidence: 0.7 }, "Alice prefers light mode": { relation: "contrasts", confidence: 0.5 } },
};

async function setup() {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "fact-relations-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI!, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD!;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}) =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const relationInputs: FactRelationContext[] = [];
  const faults: ("throw" | "digest")[] = [];
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") {
      const text = input.text.replace(/\.$/, ""), evidence = { start: 0, end: Buffer.byteLength(input.text), text: input.text };
      const day = /\[(\d{4}-\d{2}-\d{2})\]/.exec(input.text)?.[1];
      return { task: "claim", language: "en", modality: "text", claims: [{ text: text.replace(/ \[.*$/, ""), evidence, confidence: 0.9, entities: [alice], ...(day ? { time: { value: `${day}T00:00:00Z`, precision: "day" } } : {}) }] };
    }
    if (input.task === "judge_claims") return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context!.body_digest,
      decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.85 })) };
    const relation = input.relation_context!;
    relationInputs.push(relation);
    const fault = faults.shift();
    if (fault === "throw") throw new ExtractionProviderError("provider_unavailable");
    return { task: "judge_relations", language: "en", modality: "text", relation_context_digest: fault === "digest" ? "0".repeat(64) : relation.body_digest,
      judgements: relation.candidates.map(candidate => {
        const verdict = script[relation.fact.text]?.[candidate.text] ?? { relation: "unrelated", confidence: 0.2 };
        return { candidate_id: candidate.id, ...verdict, reason: `${relation.fact.text} vs ${candidate.text}` };
      }) };
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider });
  await query("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const generation = { id: uuidv7(), stream: "extraction", incarnation, state: "catching_up" as const, covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  let record = 0;
  const ingest = async (content: string) => {
    const source = await engine.remember({ content, time: { value: "2026-09-10T00:00:00Z", precision: "day" as const }, origin: { source: root, session: root, actor: "user", record: String(++record) }, source_revision: "v1", expected_previous_revision_key: null },
      { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
    const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id: source.id }, context);
    const run = () => engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
    return { source, task, run };
  };
  const factByContent = async (content: string) => {
    const rows = await query("MATCH (f:Fact {content:$content}) RETURN properties(f) AS p", { content });
    expect(rows).toHaveLength(1);
    return rows[0]!.p as Record<string, unknown>;
  };
  const operations = async (sourceId: string) => (await query("MATCH (o:MaterializationOperation {source_episode_id:$source}) RETURN o.fact_id AS fact, o.result AS result ORDER BY o.fact_id", { source: sourceId }))
    .map(row => ({ fact: String(row.fact), result: JSON.parse(String(row.result)) as Record<string, unknown> }));
  return { engine, query, generation, ingest, factByContent, operations, relationInputs, faults, driver,
    async close() { await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}

test("relation judge gates validated Facts: invalidates, chain refusal, duplicates, contrasts and idempotent retries", async () => {
  const f = await setup();
  try {
    // E1: nothing to compare against, so no relation call and a plain Fact.
    const first = await f.ingest("Alice prefers dark mode [2026-09-01].");
    const firstResult = await first.run();
    expect(firstResult.state === "known" && firstResult.relation_judge).toBe("complete");
    expect(firstResult.state === "known" && firstResult.semantic_writes).toBe(true);
    expect(f.relationInputs).toHaveLength(0);
    const dark = await f.factByContent("Alice prefers dark mode");
    expect(await f.query("MATCH (p:FactRelationInput) RETURN p.candidates AS candidates")).toEqual([{ candidates: 0 }]);

    // E2: provider fails twice (transport, then a verdict bound to the wrong premise); the pipeline stays pending without writes.
    f.faults.push("throw", "digest");
    const second = await f.ingest("Alice prefers light mode [2026-09-02].");
    for (let attempt = 0; attempt < 2; attempt++) {
      const pending = await second.run();
      expect(pending.state === "known" && pending.judge?.state).toBe("succeeded");
      expect(pending.state === "known" && pending.relation_judge).toBe("pending");
      expect(pending.state === "known" && pending.semantic_writes).toBe(false);
      expect(await f.query("MATCH (f:Fact) RETURN count(f) AS count")).toEqual([{ count: 1 }]);
      expect(await f.query("MATCH (v:FactRelationVerdict) RETURN count(v) AS count")).toEqual([{ count: 0 }]);
    }
    expect(f.relationInputs).toHaveLength(2);
    expect(await f.query("MATCH (p:FactRelationInput {source_episode_id:$source}) RETURN p.candidates AS candidates, p.last_failure AS failure, p.last_failure_detail AS detail", { source: second.source.id }))
      .toEqual([{ candidates: 1, failure: "provider_mismatch", detail: "digest" }]);
    const completed = await second.run();
    expect(completed.state === "known" && completed.relation_judge).toBe("complete");
    expect(completed.state === "known" && completed.semantic_writes).toBe(true);
    expect(f.relationInputs).toHaveLength(3);
    expect(f.relationInputs[2]!.fact).toEqual({ text: "Alice prefers light mode", time: { value: "2026-09-02T00:00:00.000Z", precision: "day" as const } });
    expect(f.relationInputs[2]!.candidates).toEqual([{ id: dark.id as string, text: "Alice prefers dark mode", time: { value: "2026-09-01T00:00:00.000Z", precision: "day" } }]);
    const light = await f.factByContent("Alice prefers light mode");
    // Backup must accept the judge's committed authority, not demand fields no writer persists.
    const snapshot = await f.engine.store.authoritySnapshot({}, context);
    expect(snapshot.invalidation_evidence).toEqual([{ id: expect.any(String), source_hash: light.digest as string,
      outcome_hash: extractionBodyDigest({ id: snapshot.invalidation_evidence[0]!.id, from: light.id, to: dark.id,
        target_id: dark.id, effective_time_utc: "2026-09-02T00:00:00.000Z", generation: f.generation.id }) }]);
    expect(snapshot.source_hashes).toEqual((await f.query("MATCH (e:Episode) RETURN e.digest AS digest ORDER BY digest")).map(row => row.digest as string));
    expect(snapshot.source_hashes).toHaveLength(2);
    const invalidations = await f.query("MATCH (a:Fact)-[l:INVALIDATES]->(b:Fact) RETURN a.id AS from, b.id AS to, l.target_id AS target, l.effective_time_utc AS effective, l.generation AS generation, l.id AS id");
    expect(invalidations).toEqual([{ from: light.id, to: dark.id, target: dark.id, effective: "2026-09-02T00:00:00.000Z", generation: f.generation.id, id: expect.any(String) }]);
    const secondOps = await f.operations(second.source.id);
    // Operations sort by fact_id: the uuidv7 of the Fact precedes the `custody:` marker.
    expect(secondOps.map(op => op.fact)).toEqual([light.id as string, `custody:${second.source.id}`]);
    expect(secondOps[0]!.result).toEqual({ created: true, fact_id: light.id, link_id: expect.any(String),
      relations: [{ candidate_id: dark.id, relation: "invalidates", confidence: 0.9, reason: "Alice prefers light mode vs Alice prefers dark mode", outcome: "linked", link_id: invalidations[0]!.id }] });

    // Retrying a finished pipeline changes nothing: no new verdicts, calls, links or operations.
    expect(await second.run()).toEqual(completed);
    expect(await f.engine.store.authoritySnapshot({}, context)).toEqual(snapshot);
    expect(f.relationInputs).toHaveLength(3);
    expect(await f.query("MATCH (v:FactRelationVerdict) RETURN count(v) AS count")).toEqual([{ count: 1 }]);
    expect(await f.query("MATCH ()-[l:INVALIDATES]->() RETURN count(l) AS count")).toEqual([{ count: 1 }]);
    expect(await f.query("MATCH (o:MaterializationOperation) RETURN count(o) AS count")).toEqual([{ count: 4 }]);

    // E3: the invalidated Fact is no longer a candidate; invalidating an invalidator is refused, never chained.
    const third = await f.ingest("Alice prefers dark mode again [2026-09-03].");
    const thirdResult = await third.run();
    expect(thirdResult.state === "known" && thirdResult.semantic_writes).toBe(true);
    expect(f.relationInputs.at(-1)!.candidates.map(candidate => candidate.id)).toEqual([light.id as string]);
    const again = await f.factByContent("Alice prefers dark mode again");
    expect(await f.query("MATCH ()-[l:INVALIDATES]->() RETURN count(l) AS count")).toEqual([{ count: 1 }]);
    expect((await f.operations(third.source.id)).find(op => op.fact === again.id)!.result.relations).toEqual([
      { candidate_id: light.id, relation: "invalidates", confidence: 0.95, reason: "Alice prefers dark mode again vs Alice prefers light mode", outcome: "chain_refused" }]);

    // E4: a duplicate writes no Fact but keeps per-claim and per-source custody.
    const fourth = await f.ingest("Alice likes dark mode [2026-09-04].");
    const fourthResult = await fourth.run();
    expect(fourthResult.state === "known" && fourthResult.relation_judge).toBe("complete");
    expect(fourthResult.state === "known" && fourthResult.semantic_writes).toBe(false);
    expect(await f.query("MATCH (f:Fact) RETURN count(f) AS count")).toEqual([{ count: 3 }]);
    const fourthOps = await f.operations(fourth.source.id);
    expect(fourthOps.map(op => op.fact.split(":")[0])).toEqual(["duplicate", "suppressed"]);
    expect(fourthOps[0]!.result).toEqual({ created: false, duplicate_of: again.id, relations: expect.arrayContaining([
      expect.objectContaining({ candidate_id: again.id, relation: "duplicate", outcome: "duplicate" }),
      expect.objectContaining({ candidate_id: light.id, relation: "unrelated", outcome: "unrelated" })]) });
    expect(fourthOps[1]!.result).toEqual({ created: false, facts: 0, refused: [], duplicates: [again.id] });

    // E5: contrasts links both Facts symmetrically readable; low-confidence verdicts fall back to unrelated.
    const fifth = await f.ingest("Alice might prefer compact mode [2026-09-05].");
    await fifth.run();
    const compact = await f.factByContent("Alice might prefer compact mode");
    expect(await f.query("MATCH (a:Fact)-[l:CONTRASTS]->(b:Fact) RETURN a.id AS from, b.id AS to, l.generation AS generation")).toEqual([{ from: compact.id, to: again.id, generation: f.generation.id }]);
    expect((await f.operations(fifth.source.id)).find(op => op.fact === compact.id)!.result.relations).toEqual(expect.arrayContaining([
      expect.objectContaining({ candidate_id: again.id, relation: "contrasts", outcome: "linked", link_id: expect.any(String) }),
      expect.objectContaining({ candidate_id: light.id, relation: "contrasts", confidence: 0.5, outcome: "low_confidence" })]));
    expect(await f.query("MATCH (:Fact)-[l:CONTRASTS|INVALIDATES]->(:Fact) RETURN count(l) AS count")).toEqual([{ count: 2 }]);

    // Every covered source carries custody, so the generation still activates.
    for (const partition of ["episodes", "active_extraction"] as const) await f.engine.store.recordExtractionCoverage({ generation_id: f.generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 5 }, context);
    await f.engine.store.cutoverExtractionGeneration({ generation_id: f.generation.id, expected_generation_id: null, expected_selector_version: 0 }, context);
    expect(await f.query("MATCH (g:ExtractionGeneration) RETURN g.state AS state")).toEqual([{ state: "active" }]);
    expect((await f.engine.checkConductingArcs()).issues).toEqual([]);
  } finally { await f.close(); }
}, 180000);

test("backup evidence covers originals without mutating history and refuses incomplete authority", async () => {
  const f = await setup();
  try {
    const input = { content: "Original statement", time: { value: "2026-09-01T00:00:00Z", precision: "day" as const },
      origin: { source: "backup-test", session: "revision", actor: "user", record: "1" } };
    const first = await f.engine.remember({ ...input, source_revision: "v1" });
    const initial = await f.engine.store.authoritySnapshot({}, context);
    expect(initial.invalidation_evidence).toEqual([]);
    expect(initial.source_hashes).toHaveLength(1);
    const second = await f.engine.remember({ ...input, content: "Revised statement", source_revision: "v2" });
    const history = () => f.query("MATCH (e:Episode) RETURN properties(e) AS p ORDER BY e.id");
    const before = await history();
    const edge = (await f.query("MATCH ()-[l:INVALIDATES]->() RETURN properties(l) AS p"))[0]!.p as Record<string, unknown>;
    const snapshot = await f.engine.store.authoritySnapshot({}, context);
    const sourceDigest = (await f.query("MATCH (e:Episode {id:$id}) RETURN e.digest AS digest", { id: second.id }))[0]!.digest;
    expect(snapshot.source_hashes).toHaveLength(2);
    expect(snapshot.invalidation_evidence).toEqual([{ id: edge.id as string, source_hash: sourceDigest,
      outcome_hash: extractionBodyDigest({ id: edge.id, from: second.id, to: first.id, target_id: first.id,
        effective_time_utc: "2026-09-01T00:00:00.000Z", generation: null }) }]);
    expect(await history()).toEqual(before);
    expect(await f.query("MATCH ()-[l:INVALIDATES]->() RETURN properties(l) AS p")).toEqual([{ p: edge }]);
    await expect(f.engine.store.authoritySnapshot({ maxItems: 1 }, context)).rejects.toThrow("authority_snapshot_limit_exceeded");

    // Deliberately corrupt only the isolated test DB. Null digest rows must not disappear in collect().
    await f.query("MATCH (e:Episode {id:$id}) REMOVE e.digest", { id: second.id });
    await expect(f.engine.store.authoritySnapshot({}, context)).rejects.toThrow("source hash evidence is incomplete");
    await f.query("MATCH (e:Episode {id:$id}) SET e.digest=$digest", { id: second.id, digest: sourceDigest });
    for (const field of ["id", "target_id", "effective_time_utc"]) {
      await f.query(`MATCH ()-[l:INVALIDATES]->() REMOVE l.${field}`);
      await expect(f.engine.store.authoritySnapshot({}, context)).rejects.toThrow("invalidation hash evidence is incomplete");
      await f.query(`MATCH ()-[l:INVALIDATES]->() SET l.${field}=$value`, { value: edge[field] });
    }
    await f.query("MATCH ()-[l:INVALIDATES]->() SET l.target_id=$wrong", { wrong: second.id });
    await expect(f.engine.store.authoritySnapshot({}, context)).rejects.toThrow("invalidation hash evidence is incomplete");
    await f.query("MATCH ()-[l:INVALIDATES]->() SET l.target_id=$target", { target: first.id });
    expect(await f.engine.store.authoritySnapshot({}, context)).toEqual(snapshot);
  } finally { await f.close(); }
}, 180000);
