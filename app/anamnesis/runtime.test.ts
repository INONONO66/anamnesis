import { afterAll, afterEach, beforeEach, expect, setDefaultTimeout, test } from "bun:test";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine, type InstallationContext } from "@anamnesis/core";
import { extractionBodyDigest, type RpcRememberParams } from "@anamnesis/protocol";
import { fakeAuthorityAdapter, sha256 } from "./authority-adapter.fixture.ts";
import type { Installation } from "./config.ts";
import { Runtime, type BackgroundLane } from "./runtime.ts";

setDefaultTimeout(60_000);

const TEST_DB = {
  uri: process.env["ANAMNESIS_TEST_NEO4J_URI"] ?? "",
  user: process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j",
  password: process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"] ?? "",
};
const LIVE = Boolean(TEST_DB.uri && TEST_DB.password);
const dbTest = test.skipIf(!LIVE);
const DEAD_URI = "bolt://127.0.0.1:1";
const admin = LIVE ? neo4j.driver(TEST_DB.uri, neo4j.auth.basic(TEST_DB.user, TEST_DB.password), { disableLosslessIntegers: true }) : undefined;
async function query(text: string, params: Record<string, string | number> = {}): Promise<Record<string, unknown>[]> {
  if (!admin) throw new Error("the isolated harness database is required");
  return (await admin.executeQuery(text, params)).records.map(record => record.toObject());
}

const receiptContext: InstallationContext = { principal: "installation", client_binding: uuidv7() };
const autoContext: InstallationContext = { principal: "installation", client_binding: uuidv7() };
const unbound: InstallationContext = { principal: "installation" };

interface Delivery { record: string; revision?: string; predecessor?: string | null; content?: string }
function delivery({ record, revision = "v1", predecessor = null, content }: Delivery): RpcRememberParams {
  return {
    episode: {
      schema: "anamnesis.original-message/1", time: { value: "2026-09-09T00:00:00Z", precision: "second" },
      content: content ?? `content of ${record}@${revision}`, mass: 0.5, properties: {},
      origin: { source: "runtime-test", session: "session", actor: "actor", record },
    },
    source_revision: revision, expected_previous_revision_key: predecessor,
  };
}
// Independent oracle: runtime.ts keeps its body-digest derivation private, so the test recomputes it from the wire
// format instead of importing it; a change in that derivation fails here on purpose.
function revisionKeyOf(params: RpcRememberParams): string {
  const o = params.episode.origin;
  return sha256(JSON.stringify([sha256(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision]));
}
function identityOf(params: RpcRememberParams, incarnation: string) {
  const canonical = (value: unknown): string => {
    if (value === null || typeof value !== "object") return JSON.stringify(value);
    if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
    return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, member]) => `${JSON.stringify(key)}:${canonical(member)}`).join(",")}}`;
  };
  return { revision_key: revisionKeyOf(params), body_digest: sha256(canonical({ digest_version: 1, params })), data_incarnation: incarnation };
}

const roots: string[] = [];
const opened: Runtime[] = [];
async function freshRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-runtime-"));
  roots.push(root);
  return root;
}
function installationAt(root: string): Installation {
  return { root, token: "token", incarnation: uuidv7(), epoch: uuidv7(), assertOwned: async () => {}, release: async () => {} };
}
async function withEnv<T>(overrides: Record<string, string | undefined>, run: () => Promise<T>): Promise<T> {
  const saved = Object.keys(overrides).map(key => [key, process.env[key]] as const);
  for (const [key, value] of Object.entries(overrides)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
  try { return await run(); }
  finally { for (const [key, value] of saved) { if (value === undefined) delete process.env[key]; else process.env[key] = value; } }
}
const dbEnv = (uri: string) => ({ ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_USER: TEST_DB.user, ANAMNESIS_NEO4J_PASSWORD: TEST_DB.password, ANAMNESIS_NEO4J_DATABASE: undefined });

interface Booted { runtime: Runtime; lanes: BackgroundLane[] }
async function boot(installation: Installation, uri = TEST_DB.uri, adapter?: ReturnType<typeof fakeAuthorityAdapter>["adapter"]): Promise<Booted> {
  const lanes: BackgroundLane[] = [];
  const runtime = await withEnv(dbEnv(uri), async () => new Runtime(installation, lane => { lanes.push(lane); }, {}, adapter ? { authorityAdapter: adapter } : {}));
  opened.push(runtime);
  await runtime.init();
  return { runtime, lanes };
}
async function shutdown(runtime: Runtime): Promise<void> {
  const index = opened.indexOf(runtime);
  if (index !== -1) opened.splice(index, 1);
  await runtime.close();
}
const bumpWriterEpoch = () => query("MATCH (m:Meta {key:'meta'}) SET m.writer_epoch = m.writer_epoch + 1000");

/** Materialize a retained Fact through the extraction pipeline. */
async function seedFact(root: string): Promise<string> {
  const content = "runtime Fact embedding fixture";
  const incarnation = extractionBodyDigest("runtime-extraction");
  const engine = new Engine({ uri: TEST_DB.uri, user: TEST_DB.user, password: TEST_DB.password, objectsRoot: root,
    materializationStatePath: join(root, "materialization-state.json"),
    extractionProvider: { model: "runtime-extraction", modelIncarnation: incarnation, async extract(input) {
      if (input.task === "claim") return { task: "claim" as const, language: "en", modality: "text" as const,
        claims: [{ text: content, confidence: 0.7, evidence: { start: 0, end: Buffer.byteLength(content), text: content } }] };
      if (input.task !== "judge_claims" || !input.claim_context) throw new Error(`unexpected ${input.task}`);
      return { task: "judge_claims" as const, language: "en", modality: "text" as const, claim_body_digest: input.claim_context.body_digest,
        decisions: input.claim_context.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain" as const, evidence: claim.evidence })) };
    } } });
  try {
    await engine.init();
    await engine.claimWriterEpoch();
    const generation = uuidv7();
    await engine.store.createExtractionGeneration({ id: generation, stream: "extraction", incarnation,
      state: "catching_up", covered_ingest_seq: 0, created_at: 100, updated_at: 100 }, receiptContext);
    const source = await engine.remember({ content, time: { value: "2026-09-01T00:00:00Z", precision: "second" },
      origin: { source: root, session: "runtime", actor: "user", record: "fact" } },
    { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context: receiptContext });
    const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation, source_id: source.id }, receiptContext);
    const audit = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: "runtime", lease_ms: 30000 }, receiptContext);
    if (audit.state !== "known" || !audit.semantic_writes) throw new Error("Fact extraction failed");
    const custody = await engine.store.materializationState.byOccurrence(extractionBodyDigest([generation, source.id]));
    const id = custody?.fact_ids[0];
    if (!id || custody?.fact_ids.length !== 1) throw new Error("expected one extracted Fact");
    return id;
  } finally { await engine.close(); }
}

beforeEach(async () => { if (LIVE) await query("MATCH (n) DETACH DELETE n"); });
afterEach(async () => {
  const closed = await Promise.allSettled(opened.splice(0).map(runtime => runtime.close()));
  await Promise.all(roots.splice(0).map(root => rm(root, { recursive: true, force: true })));
  const failure = closed.find((outcome): outcome is PromiseRejectedResult => outcome.status === "rejected");
  if (failure) throw failure.reason;
});
afterAll(async () => { await admin?.close(); });

dbTest("a live boot reports ready, commits deliveries once, and answers re-deliveries from the store", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime, lanes } = await boot(installation);
  expect(lanes).toEqual(["identity"]);
  expect(runtime.capabilities).toMatchObject({ recall: true, policy: true, extraction: false, embeddings: false });
  const ready = await runtime.status(0, false);
  expect(ready).toMatchObject({ version: 2, state: "ready", storage: "available", data_incarnation: installation.incarnation, fs_epoch: installation.epoch,
    outbox_pending: 0, workers: { extraction: { state: "unconfigured" }, embedding: { pending: 0, drained_total: 0, quarantined_total: 0, last_error: null } } });
  expect((await runtime.status(3, true))).toMatchObject({ state: "stopping", queue: { pending: 3 } });

  const first = delivery({ record: "first" });
  const committed = await runtime.remember(first);
  expect(committed).toMatchObject({ state: "committed", created: true, ...identityOf(first, installation.incarnation) });
  expect(lanes).toEqual(["identity", "identity"]);
  await shutdown(runtime);

  const restarted = (await boot(installation)).runtime;
  expect(await restarted.remember(first)).toEqual({ ...committed, created: false });
  await expect(restarted.remember(delivery({ record: "first", content: "a different body" }))).rejects.toMatchObject({ code: "revision_conflict" });
});

dbTest("a payload-bearing delivery is admitted only once its object is committed", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const bytes = new TextEncoder().encode("attached object");
  const params = { ...delivery({ record: "attached" }), payload_hash: createHash("sha256").update(bytes).digest("hex") };
  await expect(runtime.remember(params)).rejects.toMatchObject({ code: "object_not_found" });
  await runtime.uploads.store.put(bytes, "text/plain");
  expect(await runtime.remember(params)).toMatchObject({ state: "committed", created: true });
  expect(await runtime.remember(params)).toMatchObject({ state: "committed", created: false });
});

dbTest("lineage metadata needs authenticated custody and is re-verified from the retained lineage on every later sighting", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const v2 = { ...delivery({ record: "lineage" }), origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] };
  await expect(runtime.remember(v2)).rejects.toMatchObject({ code: "lineage_binding_mismatch" });
  await expect(runtime.remember(v2, unbound)).rejects.toMatchObject({ code: "lineage_binding_mismatch" });
  const committed = await runtime.remember(v2, receiptContext);
  expect(committed).toMatchObject({ state: "committed", created: true });
  expect(await runtime.remember(v2, receiptContext)).toEqual({ ...committed, created: false });
  await query("MATCH (e:Episode {id:$id}) SET e.context_digests = [$digest]", { id: committed.id, digest: "f".repeat(64) });
  await expect(runtime.remember(v2, receiptContext)).rejects.toMatchObject({ code: "lineage_mismatch" });
  await query("MATCH (e:Episode {id:$id}) REMOVE e.lineage_mode", { id: committed.id });
  await expect(runtime.remember(v2, receiptContext)).rejects.toMatchObject({ code: "lineage_unavailable" });
});

dbTest("remember.batch answers each delivery in order: a refusal answers its delivery alone and a retryable failure fails the rest", async () => {
  const installation = installationAt(await freshRoot());
  const live = await boot(installation);
  const first = delivery({ record: "batch-1" }), second = delivery({ record: "batch-2" });
  const lineage = { ...delivery({ record: "batch-lineage" }), origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] };
  const { results } = await live.runtime.rememberBatch([first, second, delivery({ record: "batch-1", content: "a different body" }), lineage, first], receiptContext);
  expect(results.map(result => result.state)).toEqual(["committed", "committed", "refused", "committed", "committed"]);
  expect(results[2]).toMatchObject({ state: "refused", code: "revision_conflict" });
  const [stored] = results;
  if (stored?.state !== "committed") throw new Error("the first delivery did not commit");
  expect(results[4]).toEqual({ ...stored, created: false });
  await shutdown(live.runtime);

  const offline = await boot(installation, DEAD_URI);
  const third = delivery({ record: "batch-3" }), fourth = delivery({ record: "batch-4" });
  const offlineLineage = { ...delivery({ record: "batch-lineage-2" }), origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] };
  await expect(offline.runtime.rememberBatch([third, offlineLineage, fourth], receiptContext)).rejects.toMatchObject({ code: "storage_unavailable", retryable: true });
  const rows = await query("MATCH (e:Episode) RETURN e.origin_record AS record ORDER BY record");
  expect(rows.map(row => row["record"])).toEqual(["batch-1", "batch-2", "batch-lineage"]);
});

dbTest("stored rows that no longer verify are reported as digest faults instead of commits", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const params = delivery({ record: "verified" });
  const committed = await runtime.remember(params);
  const [row] = await query("MATCH (e:Episode {id:$id}) RETURN e.digest AS digest, e.digest_format AS format", { id: committed.id });
  const digest = row?.["digest"], format = row?.["format"];
  if (typeof digest !== "string") throw new Error("the committed episode carries no digest");
  await query("MATCH (e:Episode {id:$id}) SET e.episode_digest_version = 3", { id: committed.id });
  await expect(runtime.remember(params)).rejects.toMatchObject({ code: "unsupported_digest_version" });
  await query("MATCH (e:Episode {id:$id}) REMOVE e.episode_digest_version SET e.digest_format = 'unknown-format'", { id: committed.id });
  await expect(runtime.remember(params)).rejects.toMatchObject({ code: "unsupported_digest_version" });
  if (typeof format === "string") await query("MATCH (e:Episode {id:$id}) SET e.digest_format = $format", { id: committed.id, format });
  else await query("MATCH (e:Episode {id:$id}) REMOVE e.digest_format", { id: committed.id });
  await query("MATCH (e:Episode {id:$id}) SET e.digest = $digest", { id: committed.id, digest: "e".repeat(64) });
  await expect(runtime.remember(params)).rejects.toMatchObject({ code: "revision_conflict" });
  await query("MATCH (e:Episode {id:$id}) SET e.digest = $digest, e.content = 'tampered'", { id: committed.id, digest });
  await expect(runtime.remember(params)).rejects.toMatchObject({ code: "revision_conflict" });
});

dbTest("a changed writer epoch is ownership loss for status and admission, and stopBackground stalls the workers", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  await runtime.remember(delivery({ record: "owned" }));
  await bumpWriterEpoch();
  await expect(runtime.status(0, false)).rejects.toMatchObject({ code: "ownership_lost" });
  await expect(runtime.refresh()).rejects.toMatchObject({ code: "ownership_lost" });
  await expect(runtime.remember(delivery({ record: "later" }))).rejects.toMatchObject({ code: "ownership_lost" });
  runtime.stopBackground();
  expect(await runtime.embeddingTurn()).toBe("stalled");
  expect(await runtime.extractionTurn()).toBe("stalled");
});

dbTest("Runtime.create wires providers from the environment and drives the embedding and extraction lanes", async () => {
  const installation = installationAt(await freshRoot());
  const plain = await withEnv({ ...dbEnv(TEST_DB.uri), ANAMNESIS_LLM_BASE_URL: undefined, ANAMNESIS_EMBEDDING_BASE_URL: undefined }, () => Runtime.create(installation));
  opened.push(plain);
  expect(plain.capabilities).toMatchObject({ extraction: false, embeddings: false });
  await shutdown(plain);
  await expect(withEnv({ ...dbEnv(TEST_DB.uri), ANAMNESIS_LLM_BASE_URL: "http://127.0.0.1:1", ANAMNESIS_LLM_API_KEY_FILE: undefined }, () => Runtime.create(installation)))
    .rejects.toThrow("ANAMNESIS_LLM_API_KEY_FILE required");
  const keyFile = join(installation.root, "llm-key.json");
  await writeFile(keyFile, JSON.stringify({ bearer: "fixture-bearer" }));
  const lanes: BackgroundLane[] = [];
  const env = { ...dbEnv(TEST_DB.uri), ANAMNESIS_LLM_BASE_URL: "http://127.0.0.1:1", ANAMNESIS_LLM_API_KEY_FILE: keyFile, ANAMNESIS_LLM_MODEL: "fixture-chat",
    ANAMNESIS_EMBEDDING_BASE_URL: "http://127.0.0.1:1", ANAMNESIS_EMBEDDING_MODEL: "fixture-embedding", ANAMNESIS_EMBEDDING_DIMENSIONS: "3",
    ANAMNESIS_EXTRACTION_MAX_IN_FLIGHT: undefined, ANAMNESIS_LLM_MIN_INTERVAL_MS: undefined, ANAMNESIS_LLM_JITTER_FRACTION: undefined };
  const empty = await withEnv(env, () => Runtime.create(installation));
  opened.push(empty);
  await empty.init();
  expect(await empty.embeddingTurn()).toBe("idle");
  expect((await empty.status(0, false)).workers.embedding).toMatchObject({ pending: 0, drained_total: 0 });
  await shutdown(empty);
  const factId = await seedFact(installation.root);
  let embeddingWoken = () => {};
  const runtime = await withEnv(env, () => Runtime.create(installation, lane => { lanes.push(lane); if (lane === "embedding") embeddingWoken(); }));
  opened.push(runtime);
  expect(runtime.capabilities).toMatchObject({ extraction: true, embeddings: true });
  await runtime.init();
  expect([...lanes].sort()).toEqual(["embedding", "extraction", "identity"]);
  // The turn only finds the batch; its provider call runs outside the turn and the batch's end wakes the lane.
  const batchEnded = new Promise<void>(resolve => { embeddingWoken = resolve; });
  expect(await runtime.embeddingTurn()).toBe("waiting");
  await batchEnded;
  expect(await runtime.extractionTurn()).toBe("waiting");

  const woken = lanes.length;
  const committed = await runtime.remember(delivery({ record: "embedded" }));
  expect(lanes.slice(woken).sort()).toEqual(["embedding", "extraction", "identity"]);
  const deferred = await runtime.status(0, false);
  expect(deferred.workers.embedding).toMatchObject({ pending: 1, drained_total: 0, quarantined_total: 0 });
  expect(deferred.workers.embedding.last_error).toContain("deferred");
  expect(await runtime.embeddingTurn()).toBe("idle");
  expect(deferred.workers.extraction).toMatchObject({ pacing: { max_in_flight: 4, min_interval_ms: 0, jitter_fraction: 0.5, calls_total: 0, waited_total_ms: 0 } });
  expect(await runtime.embeddingTurn()).toBe("idle");
  expect((await runtime.status(0, false)).workers.embedding).toMatchObject({ pending: 1, last_error: null });
  expect(await runtime.requeueQuarantinedEmbeddings({ limit: 10 }, receiptContext)).toEqual({ requeued: 0 });
  expect(lanes.at(-1)).toBe("embedding");
  const operation = uuidv7();
  const attempt = await runtime.recoverEmbedding({ operation_id: operation, fact_id: factId }, receiptContext);
  expect(attempt).toMatchObject({ operation_id: operation, fact_id: factId, state: "deferred", reason: "provider_unavailable" });
  expect(await runtime.embeddingStatus(operation, receiptContext)).toEqual(attempt);
  const absent = uuidv7();
  expect(await runtime.embeddingStatus(absent, receiptContext)).toEqual({ state: "unknown", operation_id: absent });
  await expect(runtime.createExtractionPipeline({ id: uuidv7(), generation_id: uuidv7(), source_id: committed.id }, receiptContext)).rejects.toThrow("unknown_ExtractionGeneration");

  await runtime.remember(delivery({ record: "second" }));
  await bumpWriterEpoch();
  await expect(runtime.embeddingTurn()).rejects.toMatchObject({ code: "ownership_lost" });
  await expect(runtime.extractionTurn()).rejects.toMatchObject({ code: "ownership_lost" });
});

dbTest("Runtime.create keeps providers wired while the extraction lane is off", async () => {
  const installation = installationAt(await freshRoot());
  const keyFile = join(installation.root, "llm-key.json");
  await writeFile(keyFile, JSON.stringify({ bearer: "fixture-bearer" }));
  let requests = 0;
  const provider = createServer((_request, response) => {
    requests++;
    response.end("{}");
  });
  const listening = once(provider, "listening");
  provider.listen(0, "127.0.0.1");
  await listening;
  try {
    const port = (provider.address() as AddressInfo).port;
    const lanes: BackgroundLane[] = [];
    const env = {
      ...dbEnv(TEST_DB.uri),
      ANAMNESIS_LLM_BASE_URL: `http://127.0.0.1:${port}`,
      ANAMNESIS_LLM_API_KEY_FILE: keyFile,
      ANAMNESIS_LLM_MODEL: "fixture-chat",
      ANAMNESIS_EMBEDDING_BASE_URL: "http://127.0.0.1:1",
      ANAMNESIS_EMBEDDING_MODEL: "fixture-embedding",
      ANAMNESIS_EMBEDDING_DIMENSIONS: "3",
      ANAMNESIS_EXTRACTION_LANE: "off",
    };
    const runtime = await withEnv(env, () => Runtime.create(installation, lane => { lanes.push(lane); }));
    opened.push(runtime);
    expect(runtime.capabilities).toMatchObject({ extraction: false, embeddings: true });
    await runtime.init();
    expect([...lanes].sort()).toEqual(["embedding", "identity"]);
    expect((await runtime.status(0, false)).workers.extraction).toEqual({ state: "unconfigured" });
    expect(await runtime.extractionTurn()).toBe("stalled");
    expect(requests).toBe(0);
    await runtime.remember(delivery({ record: "lane-off" }));
    expect(lanes.filter(lane => lane === "extraction")).toEqual([]);
    await expect(runtime.createExtractionPipeline({ id: uuidv7(), generation_id: uuidv7(), source_id: uuidv7() }, receiptContext))
      .rejects.toMatchObject({ code: "extraction_not_configured" });
  } finally {
    provider.close();
    await once(provider, "close");
  }
});

dbTest("storage-backed RPCs pass through to the engine under the caller's custody", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const committed = await runtime.remember(delivery({ record: "recallable", content: "the quick brown fox" }));
  const recall = await runtime.recall({ query: "quick brown fox", limit: 5 }, autoContext);
  expect(recall.results.map(item => item.id)).toEqual([committed.id]);
  expect(recall).toMatchObject({ renderer: "canonical-jsonl-v1", diagnostics: { pipeline: "derived-hybrid-v1", vector_reason: "not_configured", channels_used: ["bm25"] } });
  expect(await runtime.reportOutcome({ usage_id: recall.usage_id, idempotency_key: uuidv7(), reward: -1 }, autoContext)).toEqual({ reward: -1, applied: true });
  await expect(runtime.graphEnvelope({ seed_ids: [committed.id] }, receiptContext)).rejects.toMatchObject({ code: "degree_probe_unavailable" });
  await expect(runtime.graphEnvelope({ seed_ids: [committed.id], T: 1_757_376_000_000 }, receiptContext)).rejects.toMatchObject({ code: "degree_probe_unavailable" });
  const policyId = uuidv7();
  expect(await runtime.setPolicy({ policy_id: policyId, selector: { source: "runtime-test" }, scope: "content" }, receiptContext)).toMatchObject({ action: "deny", applied: true });
  expect(await runtime.revokePolicy({ policy_id: policyId }, receiptContext)).toMatchObject({ action: "revoke", applied: true });
  expect(await runtime.verifyHitCache()).toMatchObject({ state: "verified", issues: [] });
  expect(await runtime.rebuildHitCache()).toMatchObject({ state: "rebuilt", created: 0, removed: 0 });
  expect(await runtime.extractionPipelineStatus(uuidv7(), receiptContext)).toMatchObject({ state: "unknown" });
  expect(await runtime.runExtractionPipeline({ task_id: uuidv7(), expected_version: 1, worker_id: "runtime-test", lease_ms: 1000 }, receiptContext)).toMatchObject({ state: "unknown" });
  await expect(runtime.createExtractionPipeline({ id: uuidv7(), generation_id: uuidv7(), source_id: committed.id }, receiptContext)).rejects.toMatchObject({ code: "extraction_not_configured" });
  expect(await runtime.embeddingStatus(uuidv7(), receiptContext)).toMatchObject({ state: "unknown" });
});

dbTest("embedding recovery rejects a materialized Fact when the provider is not configured", async () => {
  const installation = installationAt(await freshRoot());
  const factId = await seedFact(installation.root);
  const { runtime } = await boot(installation);
  await expect(runtime.recoverEmbedding({ operation_id: uuidv7(), fact_id: factId }, receiptContext))
    .rejects.toMatchObject({ code: "embedding_not_configured" });
});

dbTest("a prior revision denied by episode id is withheld from the primary's supersedes view", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const original = delivery({ record: "revised", content: "the sleepy owl hoots" });
  const first = await runtime.remember(original);
  const second = await runtime.remember(delivery({ record: "revised", revision: "v2", predecessor: revisionKeyOf(original), content: "the sleepy owl hoots at dusk" }));
  const view = (recall: Awaited<ReturnType<Runtime["recall"]>>) => recall.results.map(item => [item.id, item.provenance.supersedes, item.provenance.supersedes_redacted]);
  const query = { query: "sleepy owl hoots", limit: 5 };
  expect(view(await runtime.recall(query, autoContext))).toEqual([[second.id, [{ id: first.id, content: "the sleepy owl hoots" }], false]]);
  expect(await runtime.setPolicy({ policy_id: uuidv7(), selector: { episode_id: first.id }, scope: "content" }, receiptContext)).toMatchObject({ action: "deny", applied: true });
  const withheld = await runtime.recall(query, autoContext);
  expect(view(withheld)).toEqual([[second.id, [], true]]);
  expect(withheld.results[0]!.provenance.warnings.map(warning => warning.code)).toEqual(["supersedes_withheld"]);
});

dbTest("backup and restore need an injected adapter and authenticated custody, then report operation state", async () => {
  const installation = installationAt(await freshRoot());
  const unequipped = await boot(installation);
  const operation = uuidv7();
  const never = join(await freshRoot(), "never");
  expect(await unequipped.runtime.backupStatus(operation)).toEqual({ state: "unknown", operation_id: operation, reason: "adapter_unavailable" });
  expect(await unequipped.runtime.restoreStatus(operation)).toEqual({ state: "unknown", operation_id: operation, reason: "adapter_unavailable" });
  await expect(unequipped.runtime.backup(unbound, never, operation)).rejects.toMatchObject({ code: "unauthenticated" });
  await expect(unequipped.runtime.backup(receiptContext, "", operation)).rejects.toMatchObject({ code: "invalid_params" });
  await expect(unequipped.runtime.backup(receiptContext, never, operation)).rejects.toMatchObject({ code: "backup_adapter_unavailable" });
  await expect(unequipped.runtime.restore(unbound, never, operation)).rejects.toMatchObject({ code: "unauthenticated" });
  await expect(unequipped.runtime.restore(receiptContext, never, operation)).rejects.toMatchObject({ code: "restore_adapter_unavailable" });
  await shutdown(unequipped.runtime);

  const archivedConfig = Buffer.from(JSON.stringify({ uri: TEST_DB.uri, user: TEST_DB.user, database: "neo4j" }));
  const { adapter, calls } = fakeAuthorityAdapter(installation.incarnation, { config: archivedConfig });
  const { runtime } = await boot(installation, TEST_DB.uri, adapter);
  await runtime.uploads.store.put(new TextEncoder().encode("archived object"), "text/plain");
  const parent = await freshRoot();
  const destination = join(parent, "archive");
  const archiveEnv = dbEnv(TEST_DB.uri);
  expect(await withEnv(archiveEnv, () => runtime.backup(receiptContext, destination, operation))).toEqual({ state: "complete", operation_id: operation });
  expect(await runtime.backupStatus(operation)).toEqual({ state: "complete", operation_id: operation });
  expect(await runtime.backupStatus(uuidv7())).toMatchObject({ state: "unknown", reason: "not_found" });
  expect(calls.splice(0)).toEqual(["revokeWriters", "authoritySnapshot", "dumpOffline", "materializeMembers", "startAndReady"]);
  const failed = uuidv7();
  await expect(withEnv(archiveEnv, () => runtime.backup(receiptContext, destination, failed))).rejects.toMatchObject({ code: "destination_exists" });
  expect(await runtime.backupStatus(failed)).toMatchObject({ state: "failed", error: expect.stringContaining("destination_exists") });
  expect(calls.splice(0)).toEqual(["revokeWriters", "authoritySnapshot"]);

  const missing = uuidv7();
  await expect(runtime.restore(receiptContext, join(parent, "absent"), missing)).rejects.toMatchObject({ code: "ENOENT" });
  expect(await runtime.restoreStatus(missing)).toMatchObject({ state: "failed", error: expect.stringContaining("ENOENT") });
  expect(await runtime.restoreStatus(uuidv7())).toMatchObject({ state: "unknown", reason: "not_found" });
  expect(calls.splice(0)).toEqual([]);
  expect(await runtime.restore(receiptContext, destination, operation)).toMatchObject({ state: "complete", operation_id: operation });
  expect(await runtime.restoreStatus(operation)).toEqual({ state: "complete", operation_id: operation });
  expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startRestored", "restoredAuthoritySnapshot", "rebindSource"]);
});
