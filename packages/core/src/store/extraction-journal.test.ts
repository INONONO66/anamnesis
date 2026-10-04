import { expect, test } from "bun:test";
import { mkdtemp, open, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ExtractionAttempt, ModelTask } from "@anamnesis/protocol";
import { ExtractionJournal, type ExtractionJournalEntry } from "./extraction-journal.ts";

const generation = "018f5b5e-7b1e-7abc-8def-123456789010";
const source = "018f5b5e-7b1e-7abc-8def-123456789011";
const pipeline = "018f5b5e-7b1e-7abc-8def-123456789012";
const claimId = "018f5b5e-7b1e-7abc-8def-123456789013";
const judgeId = "018f5b5e-7b1e-7abc-8def-123456789014";
const attemptId = "018f5b5e-7b1e-7abc-8def-123456789015";
const hash = "a".repeat(64);

function task(id: string, kind: "claim" | "judge" = "claim") {
  return ModelTask.parse({
    id, generation_id: generation, source_id: source, source_revision: hash, body_digest: hash,
    source_ingest_seq: 1, attempt_id: null, kind, model: "fixture", model_incarnation: hash,
    pipeline: "claim-judge-audit-v1", state: "queued", lease: null, policy_context: null,
    version: 0, attempts: 0, created_at: 1, updated_at: 1,
  });
}

function entry(): ExtractionJournalEntry {
  return {
    work_key: `${generation}:${source}`, generation_id: generation, source_id: source,
    claim: task(claimId), claim_attempt: null, judge: null, judge_input: null,
    judge_attempt: null, decisions: [], attempts: [], sealed_ingest_seq: null,
  };
}

function failedAttempt() {
  return ExtractionAttempt.parse({
    id: attemptId, task_id: claimId, generation_id: generation, source_id: source,
    source_revision: hash, body_digest: hash, source_ingest_seq: 1,
    state: "failed", reason: "provider_unavailable", disposition: null,
    created_at: 1, updated_at: 1, lease: null, output: null,
    policy_context: { revision: 1, authority: "installation" }, spans: [],
  });
}

async function withFile(run: (path: string) => Promise<void>): Promise<void> {
  const directory = await mkdtemp(join(tmpdir(), "extraction-journal-"));
  try { await run(join(directory, "extraction-state.json")); }
  finally { await rm(directory, { recursive: true }); }
}

test("loads an existing journal and resolves work, task, and attempt identifiers", async () => {
  await withFile(async path => {
    // Given one persisted entry containing both tasks and a terminal attempt.
    const record = { ...entry(), judge: task(judgeId, "judge"), attempts: [failedAttempt()] };
    await writeFile(path, JSON.stringify({ version: 1, pipelines: { [pipeline]: record } }));
    const journal = new ExtractionJournal(path);

    // When the journal is loaded through its lookup APIs.
    const found = await Promise.all([
      journal.get(pipeline), journal.byWorkKey(record.work_key),
      journal.byAttempt(attemptId), journal.byTask(claimId), journal.byTask(judgeId),
    ]);

    // Then each lookup returns the same validated entry and unknown keys remain absent.
    for (const result of found) expect(result).toEqual(record);
    expect(await journal.byAttempt(pipeline)).toBeUndefined();
    expect(await journal.byTask(pipeline)).toBeUndefined();
    expect(await journal.get(source)).toBeUndefined();
  });
});

test("set and delete persist queued mutations atomically and preserve prior entries", async () => {
  await withFile(async path => {
    // Given an empty journal and two pipeline IDs.
    const journal = new ExtractionJournal(path);
    const second = "018f5b5e-7b1e-7abc-8def-123456789016";
    const firstEntry = entry();
    const nextEntry = { ...entry(), sealed_ingest_seq: 4 };

    // When mutations are queued without awaiting each intermediate write.
    await Promise.all([journal.set(pipeline, firstEntry), journal.set(second, nextEntry)]);

    // Then the renamed file contains both complete entries, not a leftover tmp file.
    expect(JSON.parse(await readFile(path, "utf8"))).toEqual({
      version: 1, pipelines: { [pipeline]: firstEntry, [second]: nextEntry },
    });
    expect(await readdir(join(path, ".."))).toEqual(["extraction-state.json"]);
    expect(await journal.list()).toEqual([[pipeline, firstEntry], [second, nextEntry]]);

    // When a stored entry is removed, only the other entry survives a reload.
    await journal.delete(pipeline);
    expect(await new ExtractionJournal(path).list()).toEqual([[second, nextEntry]]);
  });
});

test("pending selects queued or leased claim and judge tasks within one generation", async () => {
  // Given an in-memory journal with an active claim, an active judge, and a settled pipeline.
  const journal = new ExtractionJournal();
  const queued = entry();
  const judging = { ...entry(), claim: ModelTask.parse({
    ...task(claimId), state: "failed", attempt_id: attemptId,
  }), judge: task(judgeId, "judge") };
  const settled = { ...judging, judge: ModelTask.parse({
    ...task(judgeId, "judge"), state: "cancelled", attempt_id: attemptId,
  }) };
  const ids = [pipeline, "018f5b5e-7b1e-7abc-8def-123456789016", "018f5b5e-7b1e-7abc-8def-123456789017"];
  await Promise.all(ids.map((id, index) => journal.set(id, [queued, judging, settled][index] ?? settled)));

  // When pending work is selected by generation.
  const pending = await journal.pending(generation);

  // Then terminal pipelines and unrelated generations are excluded.
  expect(pending.map(([id]) => id)).toEqual(ids.slice(0, 2));
  expect(await journal.pending(source)).toEqual([]);
});

test("atomic rewrite leaves an already-open snapshot intact", async () => {
  await withFile(async path => {
    const journal = new ExtractionJournal(path);
    const original = entry();
    await journal.set(pipeline, original);
    const snapshot = await open(path, "r");
    try {
      const updated = { ...original, sealed_ingest_seq: 4 };
      await journal.set(pipeline, updated);
      expect(JSON.parse(await snapshot.readFile("utf8")).pipelines[pipeline]).toEqual(original);
      expect(await new ExtractionJournal(path).get(pipeline)).toEqual(updated);
    } finally { await snapshot.close(); }
  });
});

test("byAttempt resolves leased claim and judge IDs before attempts are recorded", async () => {
  // Given two leased tasks whose current attempt IDs have not entered attempts[] yet.
  const journal = new ExtractionJournal();
  const judgeAttemptId = "018f5b5e-7b1e-7abc-8def-123456789018";
  const lease = { worker_id: "worker", epoch: pipeline, writer_epoch: 1, expires_at: 10 };
  const policy_context = { revision: 1, authority: "installation" } as const;
  const claim = ModelTask.parse({
    ...task(claimId), state: "leased", attempt_id: attemptId,
    attempts: 1, lease, policy_context,
  });
  const judge = ModelTask.parse({
    ...task(judgeId, "judge"), state: "leased", attempt_id: judgeAttemptId,
    attempts: 1, lease, policy_context,
  });
  const record = { ...entry(), claim, judge };
  await journal.set(pipeline, record);

  // When each leased attempt ID is looked up.
  const claimEntry = await journal.byAttempt(attemptId);
  const judgeEntry = await journal.byAttempt(judgeAttemptId);

  // Then both identify their pipeline, but an unrelated ID does not.
  expect(claimEntry).toEqual(record);
  expect(judgeEntry).toEqual(record);
  expect(await journal.byAttempt(source)).toBeUndefined();
});

test("set isolates the journal from caller and reader mutations", async () => {
  // Given a valid in-memory entry.
  const journal = new ExtractionJournal();
  const input = entry();

  // When the entry is stored and both caller and returned copies are changed.
  await journal.set(pipeline, input);
  input.claim.model = "changed";
  const returned = await journal.get(pipeline);
  if (!returned) throw new Error("missing journal entry");
  returned.claim.model = "also changed";

  // Then the validated internal record is untouched.
  expect((await journal.get(pipeline))?.claim.model).toBe("fixture");
});

test("rejects malformed file envelopes, nested records, and overlong arrays", async () => {
  await withFile(async path => {
    // Given distinct invalid persisted representations.
    const good = entry();
    const invalid = [
      { version: 2, pipelines: {} },
      { version: 1, pipelines: {}, extra: true },
      { version: 1, pipelines: { bad: good } },
      { version: 1, pipelines: { [pipeline]: { ...good, unexpected: true } } },
      { version: 1, pipelines: { [pipeline]: { ...good, claim: { ...good.claim, unexpected: true } } } },
      { version: 1, pipelines: { [pipeline]: { ...good, decisions: Array(65).fill({}) } } },
      { version: 1, pipelines: { [pipeline]: { ...good, attempts: Array(33).fill(failedAttempt()) } } },
    ];

    // When each file is opened by a fresh journal.
    for (const file of invalid) {
      await writeFile(path, JSON.stringify(file));
      // Then Zod rejects it instead of silently accepting or truncating it.
      await expect(new ExtractionJournal(path).list()).rejects.toThrow();
    }
  });
});

test("rejects invalid set entries without rewriting the previous file", async () => {
  await withFile(async path => {
    // Given a persisted valid entry.
    const journal = new ExtractionJournal(path);
    const good = entry();
    await journal.set(pipeline, good);

    // When a caller supplies an invalid work key or more than 32 attempts.
    await expect(journal.set(pipeline, { ...good, work_key: source })).rejects.toThrow();
    await expect(journal.set(pipeline, { ...good, attempts: Array(33).fill(failedAttempt()) })).rejects.toThrow();

    // Then the existing persisted entry remains readable after both failed writes.
    expect(JSON.parse(await readFile(path, "utf8")).pipelines[pipeline]).toEqual(good);
    expect(await journal.get(pipeline)).toEqual(good);
  });
});
