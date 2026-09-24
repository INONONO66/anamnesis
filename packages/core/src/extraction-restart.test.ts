// Issue #218: pipelines in flight across a daemon restart must be finished by the restarted daemon, never sealed as
// terminal omissions. Two Engines (two writer epochs) over one database model the restart; daemon A's in-flight
// provider call never returns before the stop, daemon B takes over with a provider whose identity may have drifted
// (a prompt file, base URL or model alias edited between boots changes `modelIncarnation`).
import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
import { ExtractionScheduler } from "./extraction-scheduler.ts";
import { ExtractionProviderError, type ExtractionProvider, type ExtractionProviderInput } from "./extraction.ts";
import { extractionBodyDigest } from "../../protocol/src/extraction.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "qa-restart", incarnationA = extractionBodyDigest("qa-restart:prompt-v1"), incarnationB = extractionBodyDigest("qa-restart:prompt-v2");
const alice = { mention: "Alice", normalized_name: "Alice", entity_kind: "person" };
const answer = (input: ExtractionProviderInput): unknown => {
  const evidence = { start: 0, end: Buffer.byteLength(input.text), text: input.text };
  if (input.task === "claim") return { task: "claim", language: "en", modality: "text", claims: [{ text: input.text, evidence, confidence: 0.9, entities: [alice] }] };
  if (input.task === "judge_claims") return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context!.body_digest,
    decisions: input.claim_context!.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.85 })) };
  const relation = input.relation_context!;
  return { task: "judge_relations", language: "en", modality: "text", relation_context_digest: relation.body_digest,
    judgements: relation.candidates.map(candidate => ({ candidate_id: candidate.id, relation: "unrelated", confidence: 0.3, reason: "scripted" })) };
};
type Row = Record<string, unknown>;
const inFlight = (scheduler: ExtractionScheduler) => [...(scheduler as unknown as { inFlight: Map<string, Promise<void>> }).inFlight.values()];

/** The identity daemon B's provider reports for its accepted answers, as a Chat upstream would (`model:fingerprint`). */
const reportedB = "qa-restart-20260901:fp-b";

/** Daemon A runs until the call selected by `hangs` is in flight; `restart` brings up daemon B over the same database.
 * `answerA` scripts daemon A's provider for the calls that do not hang (default: a valid answer). */
async function harness(hangs: (input: ExtractionProviderInput) => boolean, answerA: (input: ExtractionProviderInput) => unknown = answer) {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "restart-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI!, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD!;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}): Promise<Row[]> => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const read = async (cypher: string, params: Record<string, unknown>) => (await query(cypher, params)) as never;
  let offset = 0;
  const clock = () => Date.now() + offset;
  let hung!: () => void, release!: (error: Error) => void;
  const inFlightAtStop = new Promise<void>(resolve => { hung = resolve; });
  const stopped = new Promise<never>((_, reject) => { release = reject; });
  const providerA: ExtractionProvider = { model, modelIncarnation: incarnationA, async extract(input) {
    if (hangs(input)) { hung(); return stopped; }
    return answerA(input);
  } };
  const engineA = new Engine({ uri, password, objectsRoot: root, extractionProvider: providerA, clock });
  await query("MATCH (n) DETACH DELETE n");
  await engineA.init(); await engineA.claimWriterEpoch();
  const schedulerA = new ExtractionScheduler(engineA, { provider: providerA, context, clock, maxAttempts: 4, maxInFlight: 1, read, wake: () => {} });
  const daemons: { engine: Engine; scheduler: ExtractionScheduler }[] = [{ engine: engineA, scheduler: schedulerA }];
  let record = 0;
  const remember = (content: string) => engineA.remember({ content, time: { value: "2026-09-10T00:00:00Z", precision: "day" as const },
    origin: { source: root, session: root, actor: "user", record: String(++record) }, source_revision: "v1", expected_previous_revision_key: null },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
  const bodies = async (label: string) => (await query(`MATCH (n:${label}) RETURN n.body AS body ORDER BY n.id`)).map(row => JSON.parse(String(row.body)) as Record<string, unknown>);
  return {
    query, bodies, remember,
    /** Turns daemon A until the selected provider call is in flight (the state a SIGTERM/SIGKILL finds). */
    async runUntilHung() {
      for (let turns = 0; turns < 8; turns++) {
        await schedulerA.turn();
        const settled = await Promise.race([inFlightAtStop.then(() => "hung" as const), Promise.allSettled(inFlight(schedulerA)).then(() => "settled" as const)]);
        if (settled === "hung") return;
      }
      throw new Error("daemon A never reached the in-flight call");
    },
    /** A fresh Engine claims the next writer epoch over the same database; its provider identity is `incarnation`. */
    async restart(incarnation: string) {
      const provider: ExtractionProvider = { model, modelIncarnation: incarnation, reportedModelIncarnation: reportedB, async extract(input) { return answer(input); } };
      const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider, clock });
      await engine.init(); await engine.claimWriterEpoch();
      const scheduler = new ExtractionScheduler(engine, { provider, context, clock, maxAttempts: 4, maxInFlight: 1, read, wake: () => {} });
      daemons.push({ engine, scheduler });
      return scheduler;
    },
    /** Moves the shared clock past every live lease so the new daemon may settle what the old one lost. */
    async expireLeases() {
      const leased = (await bodies("ModelTask")).filter(task => task.state === "leased") as { lease: { expires_at: number } }[];
      const latest = Math.max(...leased.map(task => task.lease.expires_at));
      offset = Math.max(offset, latest - Date.now() + 1);
    },
    /** Turns until the lane is idle with the cursor at `target`, awaiting the real in-flight promises between turns. */
    async settle(scheduler: ExtractionScheduler, target: number, budget = 30) {
      let status = scheduler.status();
      for (let turns = 0; turns < budget; turns++) {
        await scheduler.turn();
        await Promise.allSettled(inFlight(scheduler));
        status = scheduler.status();
        if (status.state !== "starting" && status.covered_ingest_seq === target && status.in_flight === 0) break;
      }
      return status;
    },
    async close() {
      // The old daemon's call returns after the epoch moved on: its drive fails the writer fence and settles without effect.
      release(new Error("daemon stopped"));
      for (const daemon of daemons) await daemon.scheduler.close();
      for (const daemon of daemons) await daemon.engine.close();
      await driver.close(); await rm(root, { recursive: true, force: true });
    },
  };
}
const attemptOutcomes = (attempts: Record<string, unknown>[]) => attempts.map(attempt => [attempt.state, attempt.reason]);

test("a judge lease in flight across a restart is settled and judged by the new daemon with the same provider identity", async () => {
  const f = await harness(input => input.task === "judge_claims");
  try {
    await f.remember("Alice likes dark mode");
    await f.runUntilHung();
    const [claim, judge] = await f.bodies("ModelTask");
    expect([claim!.state, judge!.state, judge!.kind, judge!.attempts]).toEqual(["succeeded", "leased", "judge_claims", 1]);
    const schedulerB = await f.restart(incarnationA);
    // The lost lease is left alone until it expires; then the new daemon settles it as worker_lost and retries.
    expect(await schedulerB.turn()).toBe("waiting");
    await Promise.allSettled(inFlight(schedulerB));
    expect((await f.bodies("ModelTask"))[1]!.state).toBe("leased");
    await f.expireLeases();
    expect(await f.settle(schedulerB, 1)).toMatchObject({ state: "active", covered_ingest_seq: 1, live_ingest_seq: 1, in_flight: 0, completed_total: 1, failed_total: 0, last_error: null });
    expect(attemptOutcomes(await f.bodies("ExtractionAttempt"))).toEqual([["succeeded", null], ["worker_lost", "worker_lost"], ["succeeded", null]]);
    expect(await f.query("MATCH (f:Fact) RETURN f.content AS content")).toEqual([{ content: "Alice likes dark mode" }]);
  } finally { await f.close(); }
}, 60000);

test("a judge lease in flight across a restart is judged by the new daemon even when the provider identity drifted between boots", async () => {
  const f = await harness(input => input.task === "judge_claims");
  try {
    await f.remember("Alice likes dark mode");
    await f.runUntilHung();
    const schedulerB = await f.restart(incarnationB);
    await f.expireLeases();
    // Before the fix: every retry failed provider_mismatch before calling the provider (the task pinned incarnation A),
    // the budget was spent in milliseconds and the Episode was sealed as a terminal omission.
    expect(await f.settle(schedulerB, 1)).toMatchObject({ state: "active", covered_ingest_seq: 1, live_ingest_seq: 1, in_flight: 0, completed_total: 1, failed_total: 0, last_error: null });
    expect(attemptOutcomes(await f.bodies("ExtractionAttempt"))).toEqual([["succeeded", null], ["worker_lost", "worker_lost"], ["succeeded", null]]);
    const [claim, judge] = await f.bodies("ModelTask");
    // The task records the provider that actually produced its attempt; the immutable claim keeps its own identity.
    expect([claim!.model_incarnation, judge!.model_incarnation, judge!.state]).toEqual([incarnationA, incarnationB, "succeeded"]);
    // The accepted judge answer is attributed to the model daemon B's upstream reported, not only to the configured alias.
    expect((await f.bodies("ExtractionAttempt")).map(attempt => attempt.reported_model)).toEqual([undefined, undefined, reportedB]);
    expect(await f.query("MATCH (f:Fact) RETURN f.content AS content")).toEqual([{ content: "Alice likes dark mode" }]);
  } finally { await f.close(); }
}, 60000);

test("a restart during the last budgeted judge lease is retried, not sealed: a lost lease never spends the provider-failure budget", async () => {
  // Three provider failures spend attempts 1-3 of 4; the fourth lease (the last one the budget allows) is what the stop
  // interrupts. Before the fix the settlement found attempts = maxAttempts and cancelled the task into a durable omission
  // although no provider outcome had been received for that lease.
  let judgeCalls = 0;
  const f = await harness(input => input.task === "judge_claims" && judgeCalls === 3, input => {
    if (input.task === "judge_claims") { judgeCalls++; throw new ExtractionProviderError("provider_unavailable"); }
    return answer(input);
  });
  try {
    await f.remember("Alice likes dark mode");
    await f.runUntilHung();
    const [, judge] = await f.bodies("ModelTask");
    expect([judge!.state, judge!.attempts, judge!.lost_leases]).toEqual(["leased", 4, undefined]);
    const schedulerB = await f.restart(incarnationA);
    await f.expireLeases();
    expect(await f.settle(schedulerB, 1)).toMatchObject({ state: "active", covered_ingest_seq: 1, live_ingest_seq: 1, in_flight: 0, completed_total: 1, failed_total: 0, last_error: null });
    expect(attemptOutcomes(await f.bodies("ExtractionAttempt"))).toEqual([["succeeded", null], ["failed", "provider_unavailable"], ["failed", "provider_unavailable"], ["failed", "provider_unavailable"], ["worker_lost", "worker_lost"], ["succeeded", null]]);
    // The lost lease returned its attempt to the budget and was counted on its own; the retry was the fourth provider outcome.
    const [, settled] = await f.bodies("ModelTask");
    expect([settled!.state, settled!.attempts, settled!.lost_leases]).toEqual(["succeeded", 4, 1]);
    expect(await f.query("MATCH (f:Fact) RETURN f.content AS content")).toEqual([{ content: "Alice likes dark mode" }]);
  } finally { await f.close(); }
}, 60000);

test("a relation judge in flight across a restart is judged by the new daemon even when the provider identity drifted between boots", async () => {
  // The second Episode owes a verdict against the first's Fact; that call is what the stop interrupts.
  const f = await harness(input => input.task === "judge_relations");
  try {
    await f.remember("Alice likes dark mode");
    await f.remember("Alice likes light mode");
    await f.runUntilHung();
    const tasks = await f.bodies("ModelTask");
    expect(tasks.map(task => task.state)).toEqual(["succeeded", "succeeded", "succeeded", "succeeded"]);
    const schedulerB = await f.restart(incarnationB);
    expect(await f.settle(schedulerB, 2)).toMatchObject({ state: "active", covered_ingest_seq: 2, live_ingest_seq: 2, in_flight: 0, completed_total: 1, failed_total: 0, last_error: null });
    expect(await f.query("MATCH (i:FactRelationInput) RETURN i.candidates AS candidates, i.failures AS failures, i.last_failure AS failure ORDER BY candidates"))
      .toEqual([{ candidates: 0, failures: 0, failure: null }, { candidates: 1, failures: 0, failure: null }]);
    expect(await f.query("MATCH (v:FactRelationVerdict) RETURN v.model_incarnation AS incarnation, v.reported_model AS reported")).toEqual([{ incarnation: incarnationB, reported: reportedB }]);
    expect((await f.query("MATCH (f:Fact) RETURN f.content AS content ORDER BY content")).map(row => row.content)).toEqual(["Alice likes dark mode", "Alice likes light mode"]);
  } finally { await f.close(); }
}, 60000);
