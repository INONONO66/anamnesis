import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import { ExtractionScheduler } from "./extraction-scheduler.ts";
import { ExtractionProviderError, type ExtractionProviderInput } from "./extraction.ts";
import { extractionBodyDigest } from "../../protocol/src/extraction.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "qa-scheduler", incarnation = extractionBodyDigest(model);

const alice = { mention: "Alice", normalized_name: "Alice", entity_kind: "person" };
type Behaviour = (input: ExtractionProviderInput, calls: number, clock: { advance(ms: number): void }) => unknown;

/** A valid answer for any task: one claim per Episode mentioning Alice, every claim retained, relations "unrelated". */
const answer: Behaviour = (input) => {
  const evidence = { start: 0, end: Buffer.byteLength(input.text), text: input.text };
  if (input.task === "claim") return { task: "claim", language: "en", modality: "text", claims: [{ text: input.text, evidence, confidence: 0.9, entities: [alice] }] };
  if (input.task === "judge_claims") return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context!.body_digest,
    decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.85 })) };
  const relation = input.relation_context!;
  return { task: "judge_relations", language: "en", modality: "text", relation_context_digest: relation.body_digest,
    judgements: relation.candidates.map(candidate => ({ candidate_id: candidate.id, relation: "unrelated", confidence: 0.3, reason: "scripted" })) };
};

/** Owned harness Neo4j, an Engine and a scheduler that share one injected clock, and a scripted provider. The
 * scheduler creates its own catching_up generation, so every test exercises initial catch-up and cutover. No timers. */
async function setup(behaviour: Behaviour, options: { maxAttempts?: number; maxLostLeases?: number } = {}) {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "scheduler-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI!, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD!;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}) =>
    (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  let offset = 0, calls = 0;
  const clock = () => Date.now() + offset;
  const advance = (ms: number) => { offset += ms; };
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    return behaviour(input, ++calls, { advance });
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider, clock });
  await query("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const wakes: number[] = [];
  const scheduler = new ExtractionScheduler(engine, { provider, context, clock, maxAttempts: options.maxAttempts ?? 2, maxInFlight: 1,
    ...(options.maxLostLeases === undefined ? {} : { maxLostLeases: options.maxLostLeases }),
    read: async (cypher, params) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject()) as never,
    wake: () => { wakes.push(clock()); } });
  let record = 0;
  const remember = (content: string) => engine.remember({ content, time: { value: "2026-09-10T00:00:00Z", precision: "day" as const },
    origin: { source: root, session: root, actor: "user", record: String(++record) }, source_revision: "v1", expected_previous_revision_key: null },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
  /** Turns until the lane is idle with the cursor at `target`, awaiting the real in-flight promises between turns. */
  const settle = async (target: number, budget = 12) => {
    let status = scheduler.status();
    for (let turns = 0; turns < budget; turns++) {
      await Promise.allSettled([...(scheduler as unknown as { inFlight: Map<string, Promise<void>> }).inFlight.values()]);
      await scheduler.turn();
      await Promise.allSettled([...(scheduler as unknown as { inFlight: Map<string, Promise<void>> }).inFlight.values()]);
      status = scheduler.status();
      if (status.state !== "starting" && status.covered_ingest_seq === target && status.in_flight === 0) break;
    }
    return status;
  };
  return { engine, scheduler, query, wakes, calls: () => calls, root, remember, settle,
    async close() { await scheduler.close(); await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}

test("a claim whose lease keeps expiring is sealed as a durable omission after the lost-lease budget, and coverage advances past it", async () => {
  // Every provider round trip outlives the lease: the answer is valid, the lease is not. A lost lease is not a provider
  // outcome, so it is bounded by its own budget (3) rather than by the two provider attempts.
  const f = await setup((input, calls, clock) => { clock.advance(10 * 60 * 1000); return answer(input, calls, clock); }, { maxLostLeases: 3 });
  try {
    await f.remember("the lease of this claim always expires");
    const status = await f.settle(1);
    expect(status).toMatchObject({ state: "active", covered_ingest_seq: 1, live_ingest_seq: 1, in_flight: 0, completed_total: 0, failed_total: 1 });
    expect(f.calls()).toBe(3); // maxLostLeases leases, each answered after its lease ran out
    const tasks = await f.query("MATCH (t:ModelTask) RETURN t.state AS state, t.body AS body");
    expect(tasks).toHaveLength(1);
    const task = JSON.parse(String(tasks[0]!.body)) as { state: string; attempts: number; lost_leases: number; attempt_id: string };
    expect(task.state).toBe("cancelled");
    expect([task.attempts, task.lost_leases]).toEqual([0, 3]); // no provider outcome was ever recorded against the budget
    const attempts = await f.query("MATCH (a:ExtractionAttempt) RETURN a.state AS state ORDER BY a.id");
    expect(attempts.map(row => row.state)).toEqual(["expired", "expired", "expired", "cancelled"]);
    const coverage = await f.query("MATCH (c:ExtractionCoverage) RETURN c.covered_ingest_seq AS covered ORDER BY c.key");
    expect(coverage).toEqual([{ covered: 1 }, { covered: 1 }]);
    expect(await f.query("MATCH (f:Fact) RETURN count(f) AS n")).toEqual([{ n: 0 }]);
  } finally { await f.close(); }
}, 60000);

test("relation judge exhausted on initial catch-up seals the source as a durable omission and the generation still activates", async () => {
  // Two direct-lineage Episodes sharing Entity Alice: the second owes a relation verdict against the first's Fact, and the
  // provider refuses every relation call. Before the fix the lane reported "failed" without persisting anything, coverage
  // reached 2/2 and cutover failed with activation_prerequisite_unavailable: the generation never became active.
  const f = await setup((input, calls, clock) => { if (input.task === "judge_relations") throw new ExtractionProviderError("provider_unavailable"); return answer(input, calls, clock); }, { maxAttempts: 4 });
  try {
    await f.remember("Alice likes dark mode");
    await f.remember("Alice likes light mode");
    const status = await f.settle(2, 30);
    expect(status).toMatchObject({ state: "active", covered_ingest_seq: 2, live_ingest_seq: 2, in_flight: 0, completed_total: 1, failed_total: 1, last_error: null });
    expect(await f.query("MATCH (f:Fact) RETURN f.content AS content")).toEqual([{ content: "Alice likes dark mode" }]);
    const custody = await f.query("MATCH (o:MaterializationOperation) WHERE o.result CONTAINS 'relation_judge_exhausted' RETURN o.result AS result, o.fact_id AS fact");
    expect(custody).toHaveLength(1);
    const result = JSON.parse(String(custody[0]!.result)) as { created: boolean; facts: number; omitted: string; failures: number; occurrences: string[] };
    expect(result).toMatchObject({ created: false, facts: 0, omitted: "relation_judge_exhausted", failures: 4 });
    expect(result.occurrences).toHaveLength(1);
    expect(String(custody[0]!.fact)).toMatch(/^suppressed:/);
    // The first Episode's premise had no candidates (failures 0, judged trivially); only the second owed a verdict.
    const inputs = await f.query("MATCH (i:FactRelationInput) RETURN i.candidates AS candidates, i.failures AS failures ORDER BY candidates");
    expect(inputs).toEqual([{ candidates: 0, failures: 0 }, { candidates: 1, failures: 4 }]);
    // The seal is terminal: another turn neither calls the provider again nor changes the lane.
    const before = f.calls();
    await f.scheduler.turn();
    expect(f.calls()).toBe(before);
    expect((await f.engine.readExtractionSelection(context)).generation_id).not.toBeNull();
    // Idempotent and refuses to seal what is not exhausted.
    const pipelines = await f.query("MATCH (p:ExtractionPipeline) RETURN p.id AS id ORDER BY id");
    const sealed = await Promise.all(pipelines.map(row => f.engine.store.sealFactRelationOmission({ pipeline_id: String(row.id), min_failures: 4 }, context).then(r => r.sealed, error => String(error))));
    expect(sealed.sort()).toEqual(["Error: invalid_transition", false]);
  } finally { await f.close(); }
}, 60000);

test("a pipeline that succeeds on the last attempt of every stage completes instead of being reported as an exhausted failure", async () => {
  // Second Episode: claim, judge_claims and judge_relations each fail three times and succeed on the fourth attempt
  // (maxAttempts 4). The old fixed step budget ran out after the final success and returned "failed" without looking.
  const failuresLeft: Record<string, number> = { claim: 3, judge_claims: 3, judge_relations: 3 };
  const f = await setup((input, calls, clock) => {
    if (input.text === "Alice likes light mode" || input.task === "judge_relations") {
      if (failuresLeft[input.task]! > 0) { failuresLeft[input.task]!--; throw new ExtractionProviderError("provider_unavailable"); }
    }
    return answer(input, calls, clock);
  }, { maxAttempts: 4 });
  try {
    await f.remember("Alice likes dark mode");
    await f.remember("Alice likes light mode");
    const status = await f.settle(2, 40);
    expect(status).toMatchObject({ state: "active", covered_ingest_seq: 2, live_ingest_seq: 2, in_flight: 0, completed_total: 2, failed_total: 0, last_error: null });
    expect(failuresLeft).toEqual({ claim: 0, judge_claims: 0, judge_relations: 0 });
    expect((await f.query("MATCH (f:Fact) RETURN f.content AS content ORDER BY content")).map(row => row.content)).toEqual(["Alice likes dark mode", "Alice likes light mode"]);
    const attempts = await f.query("MATCH (a:ExtractionAttempt) RETURN a.state AS state, count(*) AS n ORDER BY state");
    expect(attempts).toEqual([{ state: "failed", n: 6 }, { state: "succeeded", n: 4 }]);
  } finally { await f.close(); }
}, 60000);
