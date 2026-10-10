import { expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j, { type EagerResult } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { extractionBodyDigest, type ModelTask } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { ExtractionScheduler, type ExtractionSchedulerOptions } from "./extraction-scheduler.ts";
import { ExtractionProviderError, type ExtractionProviderInput } from "./extraction.ts";
import type { ExtractionJournalEntry } from "./store/extraction-journal.ts";

const context = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() } as const;
const model = "qa-window", incarnation = extractionBodyDigest(model);
const clock = () => 1800000000000;
const claims = (text: string) => [{ text, evidence: { start: 0, end: Buffer.byteLength(text), text },
  confidence: 0.9, entities: [{ mention: "Alice", normalized_name: "Alice", entity_kind: "person" }] }];

function answer(input: ExtractionProviderInput): unknown {
  switch (input.task) {
    case "claim_window": return { task: input.task, episodes: input.episodes.map(({ id, text }) => ({
      id, claims: claims(text), language: "en", modality: "text",
    })) };
    case "judge_claims": return { task: input.task, claim_body_digest: input.claim_context?.body_digest,
      decisions: input.claim_context?.claims.map((claim, claim_index) => ({
        claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.9,
      })), language: "en", modality: "text" };
    case "judge_relations": return { task: input.task, relation_context_digest: input.relation_context?.body_digest,
      judgements: input.relation_context?.candidates.map(candidate => ({
        candidate_id: candidate.id, relation: "unrelated", confidence: 0.9, reason: "distinct fact",
      })), language: "en", modality: "text" };
    default: throw new Error("scheduler called a single claim or legacy judge");
  }
}

async function fixture(reply: (input: ExtractionProviderInput) => unknown = answer) {
  const root = await mkdtemp(join(tmpdir(), "extraction-window-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const read = async <Row extends Record<string, unknown>>(query: string, params: Record<string, unknown>) =>
    (await driver.executeQuery<EagerResult<Row>>(query, params)).records.map(row => row.toObject());
  const calls: ExtractionProviderInput[] = [];
  let now = clock();
  const currentTime = () => now;
  const provider = { model, modelIncarnation: incarnation, async extract(input: ExtractionProviderInput) {
    calls.push(input);
    return reply(input);
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, extractionProvider: provider, clock: currentTime,
    extractionJournalPath: join(root, "extraction-state.json"), materializationStatePath: join(root, "materialization-state.json") });
  const snapshots = new Map<string, ExtractionJournalEntry>();
  const save = engine.store.extractionJournal.set.bind(engine.store.extractionJournal);
  engine.store.extractionJournal.set = async (id, entry) => { await save(id, entry); snapshots.set(id, structuredClone(entry)); };
  await read("MATCH (n) DETACH DELETE n", {});
  await engine.init();
  await engine.claimWriterEpoch();
  let seq = 0;
  const schedulers: ExtractionScheduler[] = [];
  return {
    engine, read, calls, snapshots, advance: (ms: number) => { now += ms; },
    remember: (content: string, session = root) => engine.remember({ content,
      time: { value: "2026-09-01T00:00:00Z", precision: "day" },
      origin: { source: root, session, actor: "user", record: String(++seq) }, source_revision: "v1", expected_previous_revision_key: null,
    }, { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context }),
    scheduler(options: Partial<ExtractionSchedulerOptions> = {}) {
      const scheduler = new ExtractionScheduler(engine, { provider, context, read, clock: currentTime, maxInFlight: 1, wake() {}, ...options });
      schedulers.push(scheduler);
      return scheduler;
    },
    async targets(ids: string[]) {
      const generation_id = uuidv7();
      await engine.store.createExtractionGeneration({ id: generation_id, stream: "extraction", incarnation,
        state: "catching_up", covered_ingest_seq: 0, created_at: clock(), updated_at: clock() }, context);
      const tasks: ModelTask[] = [];
      for (const source_id of ids) tasks.push(await engine.createExtractionPipeline({ id: uuidv7(), generation_id, source_id }, context));
      return tasks.map(task => ({ task_id: task.id, expected_version: task.version }));
    },
    async close() {
      for (const scheduler of schedulers) await scheduler.close();
      await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true });
    },
  };
}

async function drain(scheduler: ExtractionScheduler, target: number) {
  for (let turn = 0; turn < 16; turn++) {
    await scheduler.turn();
    await Promise.all([...scheduler["inFlight"].values()]);
    const status = scheduler.status();
    if (status.state !== "starting" && status.covered_ingest_seq === target && status.in_flight === 0) return status;
  }
  throw new Error("window scheduler did not cover the fixture");
}

test("ten Episodes use two balanced claim windows while keeping individual judge and DERIVED_FROM custody", async () => {
  const f = await fixture();
  try {
    const ids: string[] = [];
    for (let index = 1; index <= 10; index++) ids.push((await f.remember(`Alice owns numbered notebook ${index}.`)).id);
    const scheduler = f.scheduler();
    expect(await drain(scheduler, 10)).toMatchObject({ completed_total: 10, failed_total: 0, last_error: null });
    const windows = f.calls.filter(input => input.task === "claim_window");
    expect(windows.map(input => input.episodes.length)).toEqual([5, 5]);
    expect(windows.flatMap(input => input.episodes.map(episode => episode.id))).toEqual(ids);
    expect(windows.map(input => input.context.map(episode => episode.id))).toEqual([[], ids.slice(2, 5)]);
    const calls_by_task = Object.fromEntries([...new Set(f.calls.map(input => input.task))]
      .map(task => [task, f.calls.filter(input => input.task === task).length]));
    expect(calls_by_task.claim_window).toBeLessThanOrEqual(2);
    expect(calls_by_task.judge_claims).toBe(10);
    const derived_from = await f.read<{ fact: string; episodes: string[] }>(
      "MATCH (f:Fact)-[:DERIVED_FROM]->(e:Episode) RETURN f.id AS fact,collect(e.id) AS episodes ORDER BY fact", {});
    expect(derived_from).toHaveLength(10);
    expect(derived_from.every(row => row.episodes.length === 1)).toBe(true);
    expect(derived_from.flatMap(row => row.episodes).sort()).toEqual([...ids].sort());
    const journal_excerpt = [...f.snapshots.values()].map(entry => ({
      source_id: entry.source_id, claim_attempt: entry.claim_attempt, judge_attempt: entry.judge_attempt,
    }));
    expect(journal_excerpt.every(entry => entry.claim_attempt?.state === "succeeded" && entry.judge_attempt?.state === "succeeded")).toBe(true);
    const path = process.env.ANAMNESIS_G5_EVIDENCE;
    if (path) await writeFile(path, JSON.stringify({ calls_by_task, windows, derived_from, journal_excerpt }, null, 2) + "\n");
  } finally { await f.close(); }
}, 120000);

test("missing and duplicate window IDs fail affected targets while a valid target keeps its own spans", async () => {
  const f = await fixture(input => {
    if (input.task !== "claim_window") return answer(input);
    const [first, second] = input.episodes;
    if (!first || !second) throw new Error("missing test targets");
    const entry = (id: string, text: string) => ({ id, claims: claims(text), language: "en", modality: "text" });
    return { task: input.task, episodes: [entry(first.id, first.text), entry(second.id, second.text), entry(second.id, second.text)] };
  });
  try {
    const ids = [];
    for (let i = 0; i < 3; i++) ids.push((await f.remember(`Alice owns notebook ${i}.`)).id);
    const targets = await f.targets(ids);
    const attempts = await f.engine.runExtractionWindow({ targets, context_ids: [], worker_id: "qa", lease_ms: 30000 }, context);
    expect(attempts.map(attempt => [attempt.source_id, attempt.state, attempt.detail])).toEqual([
      [ids[0], "succeeded", undefined], [ids[1], "failed", "window_shape"], [ids[2], "failed", "window_shape"],
    ]);
    expect(attempts[0]?.spans[0]?.text).toBe("Alice owns notebook 0.");
    expect(f.calls).toHaveLength(1);
  } finally { await f.close(); }
}, 60000);

test("window byte admission fails every target before calling the provider", async () => {
  const f = await fixture();
  try {
    const ids = [(await f.remember("a".repeat(40000))).id, (await f.remember("b".repeat(40000))).id];
    const targets = await f.targets(ids);
    const attempts = await f.engine.runExtractionWindow({ targets, context_ids: [], worker_id: "qa", lease_ms: 30000 }, context);
    expect(attempts.map(attempt => [attempt.state, attempt.reason, attempt.output])).toEqual([
      ["failed", "input_too_large", null], ["failed", "input_too_large", null],
    ]);
    expect(f.calls).toEqual([]);
  } finally { await f.close(); }
}, 60000);

test("window provider failures retain the same content-free reason for every target", async () => {
  const f = await fixture(() => { throw new ExtractionProviderError("provider_mismatch", "model"); });
  try {
    const targets = await f.targets([(await f.remember("Alice owns a notebook.")).id, (await f.remember("Alice owns a calendar.")).id]);
    const attempts = await f.engine.runExtractionWindow({ targets, context_ids: [], worker_id: "qa", lease_ms: 30000 }, context);
    expect(attempts.map(attempt => [attempt.reason, attempt.detail, attempt.output])).toEqual([
      ["provider_mismatch", "model", null], ["provider_mismatch", "model", null],
    ]);
    expect(f.calls).toHaveLength(1);
  } finally { await f.close(); }
}, 60000);

test("context input silently drops denied and missing Episodes before provider egress", async () => {
  const f = await fixture();
  try {
    const allowed = await f.remember("Alice owns a notebook.");
    const denied = await f.remember("Alice owns a private calendar.");
    const target = await f.remember("Alice owns a pencil.");
    await f.engine.setPolicy({ policy_id: uuidv7(), selector: { episode_id: denied.id }, scope: "content" }, context);
    const targets = await f.targets([target.id]);
    const attempts = await f.engine.runExtractionWindow({ targets, context_ids: [allowed.id, denied.id, uuidv7()], worker_id: "qa", lease_ms: 30000 }, context);
    expect(attempts.map(attempt => attempt.state)).toEqual(["succeeded"]);
    expect(f.calls[0]).toMatchObject({ context: [{ id: allowed.id, text: "Alice owns a notebook." }] });
  } finally { await f.close(); }
}, 60000);

test("window evidence cannot borrow another target's text", async () => {
  const f = await fixture(input => {
    if (input.task !== "claim_window") return answer(input);
    const first = input.episodes[0];
    if (!first) throw new Error("missing test target");
    return { task: input.task, episodes: input.episodes.map(({ id }) => ({
      id, claims: claims(first.text), language: "en", modality: "text",
    })) };
  });
  try {
    const targets = await f.targets([(await f.remember("Alice owns a notebook.")).id, (await f.remember("Alice owns a calendar.")).id]);
    const attempts = await f.engine.runExtractionWindow({ targets, context_ids: [], worker_id: "qa", lease_ms: 30000 }, context);
    expect(attempts.map(attempt => [attempt.state, attempt.detail])).toEqual([["succeeded", undefined], ["failed", "span"]]);
  } finally { await f.close(); }
}, 60000);

test("an oversized window retries as single targets without oversized preceding context", async () => {
  const f = await fixture(input => input.task === "claim_window"
    ? { task: input.task, episodes: input.episodes.map(({ id }) => ({ id, claims: [], language: "en", modality: "text" })) }
    : answer(input));
  try {
    await f.remember("a".repeat(40000));
    await f.remember("b".repeat(40000));
    const scheduler = f.scheduler();
    await scheduler.turn();
    await Promise.all([...scheduler["inFlight"].values()]);
    expect([...f.snapshots.values()].map(entry => entry.claim_attempt?.reason)).toEqual(["input_too_large", "input_too_large"]);
    expect(f.calls).toEqual([]);
    f.advance(100000);
    expect(await drain(scheduler, 2)).toMatchObject({ completed_total: 2, failed_total: 0, last_error: null });
    const windows = f.calls.filter(input => input.task === "claim_window");
    expect(windows.map(input => [input.episodes.length, input.context.length])).toEqual([[1, 0], [1, 0]]);
  } finally { await f.close(); }
}, 60000);

test("a skipped gate window journals each cancellation atomically and seals coverage without Facts", async () => {
  const f = await fixture();
  try {
    await f.remember("Alice says hello.");
    await f.remember("Alice says goodbye.");
    const scheduler = f.scheduler({ gate: { model: "qa-gate", async score() { return [0.05, 0.1]; } } });
    expect(await drain(scheduler, 2)).toMatchObject({ gated_total: 2, failed_total: 0, completed_total: 0, last_error: null });
    const entries = [...f.snapshots.values()];
    expect(entries.map(entry => entry.claim_attempt?.reason)).toEqual(["gate_skipped", "gate_skipped"]);
    expect(entries.map(entry => entry.gate)).toEqual([0.05, 0.1].map(score => ({
      model: "qa-gate", score, window_score: 0.1, threshold: 0.2, decided_at: clock(),
    })));
    expect(f.calls).toEqual([]);
    expect(await f.read("MATCH (f:Fact) RETURN count(f) AS count", {})).toEqual([{ count: 0 }]);
    expect(await f.engine.store.extractionJournal.list()).toEqual([]);
    const path = process.env.ANAMNESIS_G5_EVIDENCE;
    if (path) await writeFile(path.replace(/\.json$/, "-gate-skip.json"), JSON.stringify({ status: scheduler.status(), state_file_entries: entries.map(entry => ({
      source_id: entry.source_id, claim: entry.claim, claim_attempt: entry.claim_attempt, gate: entry.gate,
    })) }, null, 2) + "\n");
  } finally { await f.close(); }
}, 60000);
