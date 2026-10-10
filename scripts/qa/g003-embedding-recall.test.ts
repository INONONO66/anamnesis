import { expect, test } from "bun:test";
import { startProcess } from "./runtime-scenarios.ts";
import { Engine } from "../../packages/core/src/engine.ts";
import { EmbeddingError, EmbeddingProfile, embeddingProfileId, type EmbeddingProvider } from "../../packages/core/src/embedding.ts";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { pathToFileURL } from "node:url";
import neo4j from "neo4j-driver";
import { extractionBodyDigest, type Generation, type SemanticClaim, type SemanticReviewProvider } from "../../packages/protocol/src/index.ts";
import type { ExtractionProviderInput } from "../../packages/core/src/extraction.ts";

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

const context = { principal: "installation", commit_mode: "receipt", client_binding: Bun.randomUUIDv7() } as const;
const extractionProvider = {
  model: "g003-fact-fixture", modelIncarnation: extractionBodyDigest("g003-fact-fixture"),
  async extract(input: ExtractionProviderInput) {
    if (input.task === "claim") {
      const text = input.text.split("\n")[0]!;
      return { task: "claim", language: "en", modality: "text",
        claims: [{ text, evidence: { start: 0, end: Buffer.byteLength(text), text } }] };
    }
    if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected task ${input.task}`);
    return { task: "judge_claims", language: "en", modality: "text", claim_body_digest: input.claim_context.body_digest,
      decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence })) };
  },
};
const semanticReviewProvider: SemanticReviewProvider = {
  profileId: extractionBodyDigest("g003-review-fixture"),
  async resolve() { return { entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false, content_language: "en" }; },
  async review(input) { return { disposition: "retain", semantic_claim: input.premises.request.semantic_claim, reason: "fixture retained" }; },
};
async function factGeneration(engine: Engine, now = Date.now()): Promise<Generation> {
  const generation: Generation = { id: Bun.randomUUIDv7(), stream: "extraction", incarnation: extractionProvider.modelIncarnation,
    state: "catching_up", covered_ingest_seq: 0, created_at: now, updated_at: now };
  await engine.store.createExtractionGeneration(generation, context);
  return generation;
}
async function materializeFact(engine: Engine, generation: Generation, content: string, record: string) {
  const sourceId = (await engine.remember({
    content: `${content}\nRaw context stays in the Episode.`,
    time: { value: "2026-09-01T00:00:00Z", precision: "second" },
    origin: { source: "g003-fact-fixture", session: generation.id, actor: "test", record },
  }, { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context })).id;
  const task = await engine.createExtractionPipeline({ id: Bun.randomUUIDv7(), generation_id: generation.id, source_id: sourceId }, context);
  const audit = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "qa", lease_ms: 30000 }, context);
  if (audit.state !== "known" || audit.judge_attempt?.state !== "succeeded" || audit.semantic_writes)
    throw new Error("audit fixture must retain the claim without writing a Fact");
  const source = await engine.store.semanticEpisode(sourceId, context);
  const semantic: SemanticClaim = {
    content, content_language: "en", sub_kind: "fact", modality: "asserted", confidence: 0.7,
    time: { time_value: source.time.time_value, time_utc: source.time.time_utc, time_precision: "inherited",
      resolution: "inherited", anchor_time_utc: source.time.time_utc },
    entities: [], subject_keys: null, predicate_text: content, scope_complete: false,
    scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
    evidence: { kind: "source_locus", span: { start: 0, end: Buffer.byteLength(content) } },
  };
  const request = { proposal_id: Bun.randomUUIDv7(), generation_id: generation.id, source_episode_id: sourceId,
    judge_attempt_id: audit.judge_attempt.id, claim_index: 0, semantic_claim: semantic };
  await engine.proposeRetainedClaim(request, context);
  await engine.reviewRetainedClaim({ review_id: Bun.randomUUIDv7(), proposal_id: request.proposal_id, action: "accept", reason: "fixture accepted" }, context);
  const id = (await engine.materializeRetainedClaim({ ...request, operation_id: Bun.randomUUIDv7() }, context)).fact_id;
  return { id, sourceId };
}

test("model-scoped vectors and an event-gated policy race preserve server-derived authority", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"]!, password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]!;
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const root = await mkdtemp("/tmp/g003-models-");
  const profile = EmbeddingProfile.parse({ model: "unit-fixture", model_incarnation: "c".repeat(64), dimensions: 2,
    query_prefix: "", document_prefix: "", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 });
  let announce: (() => void) | undefined, release: (() => void) | undefined;
  let gate: Promise<void> | undefined;
  let providerCalls = 0;
  const provider: EmbeddingProvider = { profile, async embed() { providerCalls++; announce?.(); await gate; return [1, 0]; } };
  const first = new Engine({ uri, password, objectsRoot: root, embeddingProvider: provider, extractionProvider, semanticReviewProvider });
  const otherProfile = { ...profile, model_incarnation: "d".repeat(64) };
  const second = new Engine({ uri, password, objectsRoot: root, embeddingProvider: { profile: otherProfile, async embed() { return [0, 1]; } } });
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  try {
    // The preceding Node surface shares this owned graph; isolate its derived generation and ingest sequence.
    await driver.executeQuery("MATCH (n) DETACH DELETE n");
    await first.init();
    await first.claimWriterEpoch();
    const generation = await factGeneration(first);
    const { id, sourceId } = await materializeFact(first, generation, "gated model fixture", "one");
    const operation_id = Bun.randomUUIDv7();
    const attempt = await first.recoverEmbedding({ fact_id: id, operation_id }, context);
    expect(attempt.state).toBe("succeeded");
    expect(await first.recoverEmbedding({ fact_id: id, operation_id }, context)).toEqual(attempt);
    const freshOperation = Bun.randomUUIDv7(), calls = providerCalls;
    expect(await first.recoverEmbedding({ fact_id: id, operation_id: freshOperation }, context)).toEqual(attempt);
    expect(providerCalls).toBe(calls);
    expect(await first.embeddingStatus(freshOperation, context)).toEqual({ state: "unknown", operation_id: freshOperation });
    for (const partition of ["episodes", "active_extraction"] as const)
      await first.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
    await first.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: null, expected_selector_version: 0 }, context);
    await second.init();
    await expect(second.recoverEmbedding({ fact_id: id, operation_id }, context)).rejects.toThrow("idempotency_conflict");
    expect((await second.recallHybrid({ query: "" }, context)).results).toEqual([]);
    expect((await second.recoverEmbedding({ fact_id: id, operation_id: Bun.randomUUIDv7() }, context)).profile_id).toBe(embeddingProfileId(otherProfile));
    expect((await second.recallHybrid({ query: "" }, context)).results.map(item => item.id)).toEqual([id]);
    const vectors = await driver.executeQuery("MATCH (v:EmbeddingVector {fact_id:$id}) RETURN v.profile_id AS profile,v.input_revision AS revision,v.vector AS vector ORDER BY profile", { id });
    expect(vectors.records).toHaveLength(2);
    expect(vectors.records.map(row => row.get("revision"))).toEqual([attempt.input_revision, attempt.input_revision]);
    const started = new Promise<void>(resolve => { announce = resolve; });
    gate = new Promise<void>(resolve => { release = resolve; });
    const recalling = first.recallHybrid({ query: "", limit: 1 }, context);
    await Promise.race([started, new Promise<never>((_, reject) => { AbortSignal.timeout(10000).addEventListener("abort", () => reject(new Error("provider event deadline")), { once: true }); })]);
    const policy = { policy_id: Bun.randomUUIDv7(), selector: { episode_id: sourceId }, scope: "content" as const };
    await first.setPolicy(policy, context);
    release!();
    const result = await recalling;
    expect(result.results).toEqual([]);
    expect((await first.getReceipt(result.recall_id))!.primaries).toEqual([]);
    await first.revokePolicy({ policy_id: policy.policy_id }, context);
    const allowed = await first.recallHybrid({ query: "", limit: 1 }, context);
    expect(allowed.results[0]!.sources).toEqual([sourceId]);
    // In-process receipt feedback on a real Fact-primary hybrid-recall receipt.
    const replayed = await first.recallHybrid({ query: "", limit: 1 }, context);
    expect(replayed.results.map(item => item.id)).toEqual([id]);
    const adoption = { operation_id: Bun.randomUUIDv7(), recall_id: replayed.recall_id, adopted: [id], reward: 0 };
    expect((await first.commitReceipt(adoption, context)).applied).toBe(true);
    expect((await first.commitReceipt(adoption, context)).applied).toBe(false);
    expect((await driver.executeQuery("MATCH (h:Hit {namespace:$id}) RETURN count(h) AS n", { id: replayed.recall_id })).records[0]!.get("n")).toBe(2);
    await first.setPolicy({ ...policy, policy_id: Bun.randomUUIDv7() }, context);
    const feedback = { operation_id: Bun.randomUUIDv7(), recall_id: allowed.recall_id, adopted: [id], reward: 1 };
    await expect(first.commitReceipt(feedback, context)).rejects.toThrow("policy_denied");
    const hits = await driver.executeQuery("MATCH (h:Hit {namespace:$id}) RETURN count(h) AS n", { id: allowed.recall_id });
    expect(hits.records[0]!.get("n")).toBe(0);
  } finally { release?.(); await first.close(); await second.close(); await driver.close(); await rm(root, { recursive: true }); }
}, 60000);

/** Shared fixture for the embedding-lane deferral tests: an in-process provider whose failure mode the test flips, a
 * store clock the test advances so backoff is asserted exactly instead of waited for, and the embedding ledger file. */
async function deferralFixture(name: string) {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"]!, password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]!;
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const root = await mkdtemp(`/tmp/g003-${name}-`), ledgerPath = `${root}/embedding-state.json`;
  const profile = EmbeddingProfile.parse({ model: `${name}-fixture`, model_incarnation: "e".repeat(64), dimensions: 2,
    query_prefix: "", document_prefix: "", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 });
  const state = { now: Date.parse("2026-09-24T00:00:00Z"), mode: "unavailable" as "unavailable" | "rejected" | "ok", calls: 0 };
  const provider: EmbeddingProvider = { profile, async embed() {
    state.calls++;
    if (state.mode === "unavailable") throw new EmbeddingError("provider_unavailable", "http 503");
    if (state.mode === "rejected") throw new EmbeddingError("provider_rejected", "http 400");
    return [1, 0];
  } };
  const engine = new Engine({ uri, password, objectsRoot: root, embeddingLedgerPath: ledgerPath, embeddingProvider: provider,
    extractionProvider, semanticReviewProvider, clock: () => state.now });
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  // Earlier tests leave Facts behind; start from an empty graph so drain totals are exact.
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  await engine.init();
  await engine.claimWriterEpoch();
  const generation = await factGeneration(engine, state.now);
  const remember = async (record: string) => (await materializeFact(engine, generation, `${name} ${record}`, record)).id;
  type LedgerEntry = { profile_id: string; state: "deferred" | "quarantined"; deferrals: number; retry_after: number | null; attempts: Record<string, unknown>[] };
  const ledger = async (id: string): Promise<LedgerEntry | undefined> =>
    (JSON.parse(await readFile(ledgerPath, "utf8")) as { version: 2; facts: Record<string, LedgerEntry> }).facts[id];
  const vectors = async (id: string) => (await driver.executeQuery("MATCH (v:EmbeddingVector {fact_id:$id}) RETURN count(v) AS n", { id })).records[0]!.get("n");
  const close = async () => { await engine.close(); await driver.close(); await rm(root, { recursive: true }); };
  return { engine, driver, state, context, profileId: embeddingProfileId(profile), remember, ledger, vectors, close };
}

test("a transient provider failure defers the Fact with backoff; fresh Facts go first; deterministic failures quarantine at once", async () => {
  const f = await deferralFixture("deferral");
  try {
    const flaky = await f.remember("flaky");
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 1, deferral_reason: "provider_unavailable" });
    expect(await f.ledger(flaky)).toMatchObject({ profile_id: f.profileId, state: "deferred", deferrals: 1, retry_after: f.state.now + 30_000,
      attempts: [{ state: "deferred", reason: "provider_unavailable", detail: "http 503" }] });
    expect(await f.vectors(flaky)).toBe(0);
    // Not due yet: the recovered provider is not even asked.
    f.state.mode = "ok";
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(f.state.calls).toBe(1);
    // Due, but a never-deferred Fact is served before the retry.
    f.state.now += 30_000;
    const fresh = await f.remember("fresh");
    expect(await f.engine.drainEmbeddingOutbox(1)).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    expect([await f.vectors(fresh), await f.vectors(flaky)]).toEqual([1, 0]);
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(await f.vectors(flaky)).toBe(1);
    // A successful embedding deletes the ledger entry: the vector is the only record.
    expect(await f.ledger(flaky)).toBeUndefined();
    // Deterministic rejection: quarantined in one pass, with the status recorded.
    f.state.mode = "rejected";
    const bad = await f.remember("bad");
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 1, deferred: 0, deferral_reason: null });
    expect(await f.ledger(bad)).toMatchObject({ state: "quarantined", deferrals: 0, retry_after: null,
      attempts: [{ state: "quarantined", reason: "provider_rejected", detail: "http 400" }] });
    // An operator's explicit recover defers a transient failure too, never exhausts, and its operation stays a durable no-op.
    f.state.mode = "unavailable";
    const manual = await f.engine.recoverEmbedding({ fact_id: bad, operation_id: Bun.randomUUIDv7() }, f.context);
    expect(manual).toMatchObject({ state: "deferred", reason: "provider_unavailable", detail: "http 503" });
    expect(await f.engine.embeddingStatus(manual.operation_id, f.context)).toEqual(manual);
    const calls = f.state.calls;
    expect(await f.engine.recoverEmbedding({ fact_id: bad, operation_id: manual.operation_id }, f.context)).toEqual(manual);
    expect(f.state.calls).toBe(calls);
    expect(await f.vectors(bad)).toBe(0);
    expect(await f.ledger(bad)).toMatchObject({ state: "deferred", deferrals: 1, retry_after: f.state.now + 30_000,
      attempts: [{ state: "quarantined" }, { state: "deferred", operation_id: manual.operation_id }] });
    // An explicit quarantine is excluded from discovery: nothing is left for the worker, and requeue can reach the Fact.
    f.state.mode = "rejected";
    const handled = await f.remember("handled");
    expect(await f.engine.recoverEmbedding({ fact_id: handled, operation_id: Bun.randomUUIDv7() }, f.context)).toMatchObject({ state: "quarantined", reason: "provider_rejected" });
    expect(await f.ledger(handled)).toMatchObject({ state: "quarantined" });
    const settled = f.state.calls;
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(f.state.calls).toBe(settled);
    // An explicit deferral backs off like a worker deferral; the worker owns the retry once it is due.
    f.state.mode = "unavailable";
    const wobbly = await f.remember("wobbly");
    expect(await f.engine.recoverEmbedding({ fact_id: wobbly, operation_id: Bun.randomUUIDv7() }, f.context)).toMatchObject({ state: "deferred", reason: "provider_unavailable" });
    expect(await f.ledger(wobbly)).toMatchObject({ state: "deferred", deferrals: 1, retry_after: f.state.now + 30_000 });
    f.state.mode = "ok";
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 0, deferral_reason: null });
    f.state.now += 30_000;
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 2, quarantined: 0, deferred: 0, deferral_reason: null });
    expect([await f.vectors(wobbly), await f.vectors(bad), await f.vectors(handled)]).toEqual([1, 1, 0]);
    expect([await f.ledger(wobbly), await f.ledger(bad)]).toEqual([undefined, undefined]);
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10, reasons: ["provider_rejected"] }, f.context)).toEqual({ requeued: 1 }); // handled
    expect(await f.ledger(handled)).toBeUndefined();
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(await f.vectors(handled)).toBe(1);
  } finally { await f.close(); }
}, 60000);

test("the transient budget is bounded: exhaustion quarantines as provider_unavailable_exhausted and requeue returns the Fact to discovery", async () => {
  const f = await deferralFixture("requeue");
  try {
    const stuck = await f.remember("stuck");
    for (let deferrals = 1; deferrals <= 8; deferrals++) {
      expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 0, quarantined: 0, deferred: 1, deferral_reason: "provider_unavailable" });
      expect(await f.ledger(stuck)).toMatchObject({ state: "deferred", deferrals, retry_after: f.state.now + Math.min(30_000 * 2 ** (deferrals - 1), 3_600_000) });
      f.state.now += 3_600_000;
    }
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 1, deferred: 0, deferral_reason: null });
    const entry = (await f.ledger(stuck))!;
    expect(entry).toMatchObject({ state: "quarantined", retry_after: null });
    expect(entry.attempts.map(attempt => attempt["state"])).toEqual([...Array<string>(8).fill("deferred"), "quarantined"]);
    expect(entry.attempts[8]).toMatchObject({ reason: "provider_unavailable_exhausted", detail: "http 503" });
    expect(await f.vectors(stuck)).toBe(0);
    // Requeue honors the reason filter, deletes the quarantined entry and is idempotent.
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10, reasons: ["input_too_large"] }, f.context)).toEqual({ requeued: 0 });
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10, reasons: ["provider_unavailable_exhausted"] }, f.context)).toEqual({ requeued: 1 });
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10 }, f.context)).toEqual({ requeued: 0 });
    expect(await f.ledger(stuck)).toBeUndefined();
    f.state.mode = "ok";
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 1, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(await f.vectors(stuck)).toBe(1);
    expect(await f.ledger(stuck)).toBeUndefined();
    // A Fact that holds a vector is never requeued; the limit bounds a pass in fact-id order.
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 10 }, f.context)).toEqual({ requeued: 0 });
    f.state.mode = "rejected";
    const ids: string[] = [];
    for (const record of ["r1", "r2", "r3"]) ids.push(await f.remember(record));
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 3, quarantined: 3, deferred: 0, deferral_reason: null });
    const ordered = [...ids].sort();
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 2, reasons: ["provider_rejected"] }, f.context)).toEqual({ requeued: 2 });
    expect(await Promise.all(ordered.map(id => f.ledger(id).then(item => item?.state)))).toEqual([undefined, undefined, "quarantined"]);
    expect(await f.engine.requeueQuarantinedEmbeddings({ limit: 2, reasons: ["provider_rejected"] }, f.context)).toEqual({ requeued: 1 });
    f.state.mode = "ok";
    expect(await f.engine.drainEmbeddingOutbox(100)).toEqual({ drained: 3, quarantined: 0, deferred: 0, deferral_reason: null });
    expect(await Promise.all(ids.map(id => f.vectors(id)))).toEqual([1, 1, 1]);
  } finally { await f.close(); }
}, 60000);
