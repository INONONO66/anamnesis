import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import { modelTaskRetryDelayMs } from "./backoff.ts";
import { ExtractionScheduler } from "./extraction-scheduler.ts";
import { ExtractionProviderError, type ExtractionProviderInput } from "./extraction.ts";
import { extractionBodyDigest } from "../../protocol/src/extraction.ts";
import type { ExtractionJournalEntry } from "./store/extraction-journal.ts";
import type { MaterializationStateEntry } from "./store/materialization-state.ts";
import type { ModelTask } from "@anamnesis/protocol";

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
  const audit: { event: string; fields: Record<string, unknown> }[] = [];
  const snapshots = new Map<string, ExtractionJournalEntry>();
  const operationSnapshots = new Map<string, MaterializationStateEntry>();
  const clock = () => Date.now() + offset;
  const advance = (ms: number) => { offset += ms; };
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    return behaviour(input, ++calls, { advance });
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider, clock,
    audit: (event, fields) => { audit.push({ event, fields }); } });
  const saveOperation = engine.store.materializationState.set.bind(engine.store.materializationState);
  engine.store.materializationState.set = async (id, entry) => { await saveOperation(id, entry); operationSnapshots.set(id, structuredClone(entry)); };
  const saveEntry = engine.store.extractionJournal.set.bind(engine.store.extractionJournal);
  engine.store.extractionJournal.set = async (id, entry) => { await saveEntry(id, entry); snapshots.set(id, structuredClone(entry)); };
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
  /** Moves the shared clock past every failed task's retry backoff (the persisted schedule, not the lane's timer). */
  const elapseBackoff = async () => {
    const failed = (await engine.store.extractionJournal.list()).flatMap(([, entry]) => [entry.claim, entry.judge])
      .filter((task): task is ModelTask => task?.state === "failed");
    if (failed.length === 0) return;
    const due = Math.max(...failed.map(task => task.updated_at + modelTaskRetryDelayMs(task.attempts)));
    offset = Math.max(offset, due - Date.now() + 1);
  };
  /** Turns until the lane is idle with the cursor at `target`, awaiting the real in-flight promises between turns; a failed
   * attempt's retry backoff elapses between turns. */
  const seen = new Set<string>();
  const capture = async (onTerminal?: (id: string, entry: ExtractionJournalEntry) => Promise<void>) => {
    for (const [id, entry] of await engine.store.extractionJournal.list()) {
      snapshots.set(id, entry);
      if (!onTerminal || seen.has(id)) continue;
      const pipeline = await engine.store.readExtractionPipeline(id, context);
      if (pipeline.state !== "known" || pipeline.relation_judge === "pending" ||
        (pipeline.claim.state !== "cancelled" && pipeline.claim.state !== "failed" &&
         pipeline.judge?.state !== "succeeded" && pipeline.judge?.state !== "cancelled" && pipeline.judge?.state !== "failed")) continue;
      seen.add(id);
      await onTerminal(id, entry);
    }
  };
  const settle = async (target: number, budget = 12, onTerminal?: (id: string, entry: ExtractionJournalEntry) => Promise<void>) => {
    let status = scheduler.status();
    for (let turns = 0; turns < budget; turns++) {
      await Promise.allSettled([...(scheduler as unknown as { inFlight: Map<string, Promise<void>> }).inFlight.values()]);
      await capture(onTerminal);
      await scheduler.turn();
      await Promise.allSettled([...(scheduler as unknown as { inFlight: Map<string, Promise<void>> }).inFlight.values()]);
      await capture(onTerminal);
      await elapseBackoff();
      status = scheduler.status();
      if (status.state !== "starting" && status.covered_ingest_seq === target && status.in_flight === 0) break;
    }
    return status;
  };
  return { engine, scheduler, query, wakes, audit, snapshots, operationSnapshots, calls: () => calls, root, remember, settle,
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
    const entries = [...f.snapshots.values()];
    expect(entries).toHaveLength(1);
    const task = entries[0]!.claim;
    expect(task.state).toBe("cancelled");
    expect([task.attempts, task.lost_leases]).toEqual([0, 3]); // no provider outcome was ever recorded against the budget
    expect(entries[0]!.attempts.sort((a, b) => a.id.localeCompare(b.id)).map(attempt => attempt.state)).toEqual(["expired", "expired", "expired", "cancelled"]);
    const coverage = await f.query("MATCH (c:ExtractionCoverage) RETURN c.covered_ingest_seq AS covered ORDER BY c.key");
    expect(coverage).toEqual([{ covered: 1 }, { covered: 1 }]);
    expect(await f.query("MATCH (f:Fact) RETURN count(f) AS n")).toEqual([{ n: 0 }]);
    expect(await f.engine.store.extractionJournal.list()).toEqual([]);
    expect(f.audit.filter(({ event }) => event === "extraction.pipeline.pruned")).toHaveLength(1);
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
    const sealed: (boolean | string)[] = [];
    const status = await f.settle(2, 30, async (id) => {
      sealed.push(await f.engine.store.sealFactRelationOmission({ pipeline_id: id, min_failures: 4 }, context).then(r => r.sealed, error => String(error)));
    });
    expect(status).toMatchObject({ state: "active", covered_ingest_seq: 2, live_ingest_seq: 2, in_flight: 0, completed_total: 1, failed_total: 1, last_error: null });
    expect(await f.query("MATCH (f:Fact) RETURN f.content AS content")).toEqual([{ content: "Alice likes dark mode" }]);
    const custody = [...f.operationSnapshots.values()].filter(entry => "omitted" in entry.result && entry.result.omitted === "relation_judge_exhausted");
    expect(custody).toHaveLength(1);
    const result = custody[0]?.result;
    expect(result).toMatchObject({ created: false, facts: 0, omitted: "relation_judge_exhausted", failures: 4 });
    if (!result || !("occurrences" in result) || !result.occurrences) throw new Error("missing omission occurrences");
    expect(result.occurrences).toHaveLength(1);
    expect(custody[0]?.fact_ids).toEqual([]);
    // The first Episode's premise had no candidates (failures 0, judged trivially); only the second owed a verdict.
    const inputs = [...f.snapshots.values()].flatMap(entry => entry.relations?.verdicts ?? [])
      .map(verdict => ({ candidates: verdict.context.candidates.length, failures: verdict.failures })).sort((a, b) => a.candidates - b.candidates);
    expect(inputs).toEqual([{ candidates: 0, failures: 0 }, { candidates: 1, failures: 4 }]);
    // The seal is terminal: another turn neither calls the provider again nor changes the lane.
    const before = f.calls();
    await f.scheduler.turn();
    expect(f.calls()).toBe(before);
    expect((await f.engine.readExtractionSelection(context)).generation_id).not.toBeNull();
    // Idempotency and non-exhausted rejection were checked while each entry still existed, before coverage pruned it.
    expect(sealed.sort()).toEqual(["Error: invalid_transition", false]);
    expect(await f.engine.store.extractionJournal.list()).toEqual([]);
    expect(await f.engine.store.materializationState.list()).toEqual([]);
    expect(f.audit.filter(({ event }) => event === "extraction.pipeline.pruned")).toHaveLength(2);
    expect(f.audit).toContainEqual(expect.objectContaining({ event: "extraction.pipeline.materialized",
      fields: expect.objectContaining({ relation_judge: "omitted", facts: 0 }) }));
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
    const attempts = [...f.snapshots.values()].flatMap(entry => entry.attempts);
    expect(attempts.filter(attempt => attempt.state === "failed")).toHaveLength(6);
    expect(attempts.filter(attempt => attempt.state === "succeeded")).toHaveLength(4);
    expect(await f.engine.store.extractionJournal.list()).toEqual([]);
    expect(f.audit.filter(({ event }) => event === "extraction.pipeline.pruned")).toHaveLength(2);
  } finally { await f.close(); }
}, 60000);

test("a judge retried past the 32-outcome history still reads its claim attempt and the pipeline completes", async () => {
  // One succeeded claim plus 33 failed judge outcomes exceeds the bounded attempt history. Every judge lease re-reads
  // the claim attempt by id (claim context), so the pinned claim and judge attempts must outlive the evicted history.
  let judgeFailuresLeft = 33;
  const f = await setup((input, calls, clock) => {
    if (input.task === "judge_claims" && judgeFailuresLeft > 0) { judgeFailuresLeft--; throw new ExtractionProviderError("provider_unavailable"); }
    return answer(input, calls, clock);
  }, { maxAttempts: 35 });
  try {
    await f.remember("Alice likes dark mode");
    const status = await f.settle(1, 120);
    expect(status).toMatchObject({ state: "active", covered_ingest_seq: 1, live_ingest_seq: 1, in_flight: 0, completed_total: 1, failed_total: 0, last_error: null });
    expect(judgeFailuresLeft).toBe(0);
    expect((await f.query("MATCH (f:Fact) RETURN f.content AS content")).map(row => row.content)).toEqual(["Alice likes dark mode"]);
    const recorded = f.audit.filter(({ event }) => event === "extraction.attempt.recorded").map(({ fields }) => fields);
    expect(recorded.filter(fields => fields["state"] === "failed")).toHaveLength(33);
    expect(recorded.filter(fields => fields["state"] === "succeeded")).toHaveLength(2);
    // The last snapshot before pruning: history bounded at 32, the claim attempt pinned outside it with its replay digest.
    const [entry] = [...f.snapshots.values()];
    expect(entry!.attempts).toHaveLength(32);
    expect(entry!.attempts.some(attempt => attempt.id === entry!.claim_attempt!.id)).toBe(false);
    expect(entry!.claim_attempt!.state).toBe("succeeded");
    expect(Object.keys(entry!.request_digests ?? {})).toContain(entry!.claim_attempt!.id);
    expect(await f.engine.store.extractionJournal.list()).toEqual([]);
  } finally { await f.close(); }
}, 120000);
