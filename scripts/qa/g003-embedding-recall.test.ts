import { expect, test } from "bun:test";
import { startProcess } from "./runtime-scenarios.ts";
import { Engine } from "../../packages/core/src/engine.ts";
import { EmbeddingError, EmbeddingProfile, embeddingProfileId, type EmbeddingProvider } from "../../packages/core/src/embedding.ts";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { pathToFileURL } from "node:url";
import neo4j from "neo4j-driver";

test("configured provider, recovery, hybrid recall and exact budgets on real Node/UDS/Neo4j", async () => {
  const root = await mkdtemp("/tmp/g003-receipt-contract-");
  try {
    const bundle = `${root}/receipt-digest.mjs`;
    const build = await startProcess(process.execPath, ["build", "packages/core/src/receipt-digest.ts", "--target=node", "--outfile", bundle], { deadlineMs: 30000 }).done;
    expect(build.timedOut).toBe(false); expect(build.code).toBe(0);
    const hash = async () => createHash("sha256").update(await readFile(bundle)).digest("hex");
    const before = await hash();
    const child = startProcess("node", ["app/anamnesis/embedding-recall.surface.mjs"], {
      deadlineMs: 120000, onOutput: text => process.stdout.write(text),
      env: { ...process.env, RECEIPT_DIGEST_BUNDLE: pathToFileURL(bundle).href },
    });
    const result = await child.done;
    expect(await hash()).toBe(before);
    expect(result.timedOut).toBe(false); expect(result.code).toBe(0);
  } finally { await rm(root, { recursive: true }); }
}, 130000);

test("model-scoped vectors and an event-gated policy race preserve server-derived authority", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"]!, password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]!;
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const root = await mkdtemp("/tmp/g003-models-");
  const profile = EmbeddingProfile.parse({ model: "unit-fixture", model_incarnation: "c".repeat(64), dimensions: 2,
    query_prefix: "", document_prefix: "", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 });
  let announce: (() => void) | undefined, release: (() => void) | undefined;
  let gate: Promise<void> | undefined;
  const provider: EmbeddingProvider = { profile, async embed() { announce?.(); await gate; return [1, 0]; } };
  const context = { principal: "installation", commit_mode: "receipt" } as const;
  const first = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider });
  const otherProfile = { ...profile, model_incarnation: "d".repeat(64) };
  const second = new Engine({ uri, password, objectsRoot: root, embeddingProvider: { profile: otherProfile, async embed() { return [0, 1]; } } });
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  try {
    await first.init();
    const id = (await first.remember({ content: "gated model fixture", mass: 0,
      time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: root, session: "s", actor: "a", record: "one" } })).id;
    const operation_id = Bun.randomUUIDv7();
    const attempt = await first.recoverEmbedding({ episode_id: id, operation_id }, context);
    expect(attempt.state).toBe("succeeded");
    await second.init();
    await expect(second.recoverEmbedding({ episode_id: id, operation_id }, context)).rejects.toThrow("idempotency_conflict");
    expect((await second.recallHybrid({ query: "" }, context)).results).toEqual([]);
    expect((await second.recoverEmbedding({ episode_id: id, operation_id: Bun.randomUUIDv7() }, context)).profile_id).toBe(embeddingProfileId(otherProfile));
    expect((await second.recallHybrid({ query: "" }, context)).results.map(item => item.id)).toEqual([id]);
    const vectors = await driver.executeQuery("MATCH (v:EmbeddingVector {episode_id:$id}) RETURN v.profile_id AS profile,v.input_revision AS revision,v.vector AS vector ORDER BY profile", { id });
    expect(vectors.records).toHaveLength(2);
    expect(vectors.records.map(row => row.get("revision"))).toEqual([attempt.input_revision, attempt.input_revision]);
    const started = new Promise<void>(resolve => { announce = resolve; });
    gate = new Promise<void>(resolve => { release = resolve; });
    const recalling = first.recallHybrid({ query: "", limit: 1 }, context);
    await Promise.race([started, new Promise<never>((_, reject) => { AbortSignal.timeout(10000).addEventListener("abort", () => reject(new Error("provider event deadline")), { once: true }); })]);
    const policy = { policy_id: Bun.randomUUIDv7(), selector: { episode_id: id }, scope: "content" as const };
    await first.setPolicy(policy, context);
    release!();
    const result = await recalling;
    expect(result.results).toEqual([]);
    expect((await first.getReceipt(result.recall_id))!.primaries).toEqual([]);
    await first.revokePolicy({ policy_id: policy.policy_id }, context);
    const allowed = await first.recallHybrid({ query: "", limit: 1 }, context);
    expect(allowed.results[0]!.sources).toEqual([id]);
    await first.setPolicy({ ...policy, policy_id: Bun.randomUUIDv7() }, context);
    const feedback = { operation_id: Bun.randomUUIDv7(), recall_id: allowed.recall_id, adopted: [id], reward: 1 };
    await expect(first.commitReceipt(feedback, context)).rejects.toThrow("policy_denied");
    const hits = await driver.executeQuery("MATCH (h:Hit {namespace:$id}) RETURN count(h) AS n", { id: allowed.recall_id });
    expect(hits.records[0]!.get("n")).toBe(0);
  } finally { release?.(); await first.close(); await second.close(); await driver.close(); await rm(root, { recursive: true }); }
}, 60000);

/** Shared fixture for the outbox deferral tests: an in-process provider whose failure mode the test flips, and a
 * store clock the test advances so backoff is asserted exactly instead of waited for. */
async function deferralFixture(name: string) {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"]!, password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]!;
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const root = await mkdtemp(`/tmp/g003-${name}-`);
  const profile = EmbeddingProfile.parse({ model: `${name}-fixture`, model_incarnation: "e".repeat(64), dimensions: 2,
    query_prefix: "", document_prefix: "", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 });
  const state = { now: Date.parse("2026-09-24T00:00:00Z"), mode: "unavailable" as "unavailable" | "rejected" | "ok", calls: 0 };
  const provider: EmbeddingProvider = { profile, async embed() {
    state.calls++;
    if (state.mode === "unavailable") throw new EmbeddingError("provider_unavailable", "http 503");
    if (state.mode === "rejected") throw new EmbeddingError("provider_rejected", "http 400");
    return [1, 0];
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider, clock: () => state.now });
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  await engine.init();
  // Earlier tests in this file leave undrained outbox entries behind (they recover by hand); retire them so drain totals are exact.
  await driver.executeQuery("MATCH (o:Outbox) WHERE o.processed_at IS NULL SET o.processed_at = $now", { now: new Date().toISOString() });
  const context = { principal: "installation", commit_mode: "receipt" } as const;
  const remember = async (record: string) => (await engine.remember({ content: `${name} ${record}`, mass: 0,
    time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: root, session: "s", actor: "a", record } })).id;
  const attempts = async (id: string) => (await driver.executeQuery(
    "MATCH (a:EmbeddingAttempt {episode_id:$id}) RETURN a.body AS body, a.state AS state, a.reason AS reason ORDER BY a.operation_id", { id }))
    .records.map(row => ({ ...JSON.parse(row.get("body")), node_state: row.get("state"), node_reason: row.get("reason") }));
  const outbox = async (id: string) => (await driver.executeQuery(
    "MATCH (o:Outbox {element_id:$id}) RETURN o.processed_at AS processed_at, o.deferrals AS deferrals, o.retry_after AS retry_after ORDER BY o.enqueued_at", { id }))
    .records.map(row => row.toObject());
  const vectors = async (id: string) => (await driver.executeQuery("MATCH (v:EmbeddingVector {episode_id:$id}) RETURN count(v) AS n", { id })).records[0]!.get("n");
  const close = async () => { await engine.close(); await driver.close(); await rm(root, { recursive: true }); };
  return { engine, driver, state, context, remember, attempts, outbox, vectors, close };
}

test("a transient provider failure defers the outbox entry with backoff; fresh entries go first; deterministic failures quarantine at once", async () => {
  const f = await deferralFixture("deferral");
  try {
    const flaky = await f.remember("flaky");
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 1, deferral_reason: "provider_unavailable" });
    expect(await f.attempts(flaky)).toMatchObject([{ state: "deferred", reason: "provider_unavailable", detail: "http 503", node_state: "deferred", node_reason: "provider_unavailable" }]);
    expect(await f.outbox(flaky)).toEqual([{ processed_at: null, deferrals: 1, retry_after: f.state.now + 30_000 }]);
    expect(await f.vectors(flaky)).toBe(0);
    // Not due yet: the recovered provider is not even asked.
    f.state.mode = "ok";
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(f.state.calls).toBe(1);
    // Due, but a never-deferred entry is served before the retry.
    f.state.now += 30_000;
    const fresh = await f.remember("fresh");
    expect(await f.engine.drainEmbeddingOutbox(1)).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    expect([await f.vectors(fresh), await f.vectors(flaky)]).toEqual([1, 0]);
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(await f.vectors(flaky)).toBe(1);
    expect((await f.outbox(flaky))[0]!["processed_at"]).toEqual(expect.any(String));
    expect(await f.attempts(flaky)).toMatchObject([{ state: "deferred" }, { state: "succeeded", reason: null, detail: null }]);
    // Deterministic rejection: quarantined and retired in one pass, with the status recorded.
    f.state.mode = "rejected";
    const bad = await f.remember("bad");
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 1, deferred: 0, deferral_reason: null });
    expect(await f.attempts(bad)).toMatchObject([{ state: "quarantined", reason: "provider_rejected", detail: "http 400", node_reason: "provider_rejected" }]);
    expect((await f.outbox(bad))[0]!["processed_at"]).toEqual(expect.any(String));
    // An operator's explicit recover defers a transient failure too, never exhausts, and its operation stays a durable no-op.
    f.state.mode = "unavailable";
    const manual = await f.engine.recoverEmbedding({ episode_id: bad, operation_id: Bun.randomUUIDv7() }, f.context);
    expect(manual).toMatchObject({ state: "deferred", reason: "provider_unavailable", detail: "http 503" });
    expect(await f.engine.embeddingStatus(manual.operation_id, f.context)).toEqual(manual);
    const calls = f.state.calls;
    expect(await f.engine.recoverEmbedding({ episode_id: bad, operation_id: manual.operation_id }, f.context)).toEqual(manual);
    expect(f.state.calls).toBe(calls);
    expect(await f.vectors(bad)).toBe(0);
  } finally { await f.close(); }
}, 60000);

test("the transient budget is bounded: exhaustion quarantines as provider_unavailable_exhausted and requeue returns the Episode to the outbox", async () => {
  const f = await deferralFixture("requeue");
  try {
    const stuck = await f.remember("stuck");
    for (let deferrals = 1; deferrals <= 8; deferrals++) {
      expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 1, deferral_reason: "provider_unavailable" });
      expect(await f.outbox(stuck)).toEqual([{ processed_at: null, deferrals, retry_after: f.state.now + Math.min(30_000 * 2 ** (deferrals - 1), 3_600_000) }]);
      f.state.now += 3_600_000;
    }
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 1, deferred: 0, deferral_reason: null });
    const history = await f.attempts(stuck);
    expect(history.map(attempt => attempt.state)).toEqual([...Array<string>(8).fill("deferred"), "quarantined"]);
    expect(history[8]).toMatchObject({ reason: "provider_unavailable_exhausted", detail: "http 503", node_reason: "provider_unavailable_exhausted" });
    expect(await f.vectors(stuck)).toBe(0);
    expect((await f.outbox(stuck))[0]!["processed_at"]).toEqual(expect.any(String));
    // Requeue honors the reason filter, skips Episodes already queued, resets the budget and keeps the audit rows.
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10, reasons: ["input_too_large"] }, f.context)).toEqual({ requeued: 0 });
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10, reasons: ["provider_unavailable_exhausted"] }, f.context)).toEqual({ requeued: 1 });
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10 }, f.context)).toEqual({ requeued: 0 });
    expect(await f.outbox(stuck)).toMatchObject([{ processed_at: expect.any(String) }, { processed_at: null, deferrals: null, retry_after: null }]);
    f.state.mode = "ok";
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(await f.vectors(stuck)).toBe(1);
    expect((await f.attempts(stuck)).map(attempt => attempt.state)).toEqual([...Array<string>(8).fill("deferred"), "quarantined", "succeeded"]);
    // An Episode that holds a vector is never requeued; the limit bounds a pass.
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10 }, f.context)).toEqual({ requeued: 0 });
    f.state.mode = "rejected";
    const ids = await Promise.all(["r1", "r2", "r3"].map(record => f.remember(record)));
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 3, quarantined: 3, deferred: 0, deferral_reason: null });
    // Rows written before a.reason existed carry the reason only in the body; requeue lifts it before filtering.
    await f.driver.executeQuery("MATCH (a:EmbeddingAttempt {episode_id:$id}) REMOVE a.reason", { id: ids[0] });
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 2, reasons: ["provider_rejected"] }, f.context)).toEqual({ requeued: 2 });
    expect((await f.attempts(ids[0]!))[0]).toMatchObject({ node_reason: "provider_rejected" });
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 2, reasons: ["provider_rejected"] }, f.context)).toEqual({ requeued: 1 });
    f.state.mode = "ok";
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 3, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(await Promise.all(ids.map(id => f.vectors(id)))).toEqual([1, 1, 1]);
  } finally { await f.close(); }
}, 60000);
