import { afterAll, afterEach, beforeEach, expect, setDefaultTimeout, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import type { InstallationContext } from "@anamnesis/core";
import type { RpcRememberParams } from "@anamnesis/protocol";
import { fakeAuthorityAdapter, sha256 } from "./authority-adapter.fixture.ts";
import type { Installation } from "./config.ts";
import { Runtime, type BackgroundLane } from "./runtime.ts";
import { sourceRevisionKey } from "./source.ts";

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

const receiptContext: InstallationContext = { principal: "installation", commit_mode: "receipt", client_binding: uuidv7() };
const autoContext: InstallationContext = { principal: "installation", commit_mode: "auto", client_binding: uuidv7() };
const unbound: InstallationContext = { principal: "installation", commit_mode: "receipt" };

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
function identityOf(params: RpcRememberParams, incarnation: string) {
  const canonical = (value: unknown): string => {
    if (value === null || typeof value !== "object") return JSON.stringify(value);
    if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
    return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, member]) => `${JSON.stringify(key)}:${canonical(member)}`).join(",")}}`;
  };
  return { revision_key: sourceRevisionKey(params), body_digest: sha256(canonical({ digest_version: 1, params })), data_incarnation: incarnation };
}

type Remembered = Awaited<ReturnType<Runtime["remember"]>>;
function asCommitted(result: Remembered): Extract<Remembered, { state: "committed" }> {
  if (result.state !== "committed") throw new Error(`expected a committed delivery, saw ${result.state}`);
  return result;
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
async function drainToRest(runtime: Runtime): Promise<number> {
  for (let turns = 1; turns <= 1000; turns++) if (!(await runtime.drainTurn())) return turns;
  throw new Error("drain did not come to rest within 1000 turns");
}
async function spoolOffline(installation: Installation, ...deliveries: RpcRememberParams[]): Promise<void> {
  const offline = await boot(installation, DEAD_URI);
  for (const params of deliveries) {
    const result = await offline.runtime.remember(params);
    if (result.state !== "spooled") throw new Error(`expected a spooled delivery, saw ${result.state}`);
  }
  await shutdown(offline.runtime);
}
const bumpWriterEpoch = () => query("MATCH (m:Meta {key:'meta'}) SET m.writer_epoch = m.writer_epoch + 1000");

beforeEach(async () => { if (LIVE) await query("MATCH (n) DETACH DELETE n"); });
afterEach(async () => {
  const closed = await Promise.allSettled(opened.splice(0).map(runtime => runtime.close()));
  await Promise.all(roots.splice(0).map(root => rm(root, { recursive: true, force: true })));
  const failure = closed.find((outcome): outcome is PromiseRejectedResult => outcome.status === "rejected");
  if (failure) throw failure.reason;
});
afterAll(async () => { await admin?.close(); });

dbTest("a live boot reports ready, commits deliveries once, and answers re-deliveries and ingest status from the store", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime, lanes } = await boot(installation);
  expect(lanes).toEqual(["spool"]);
  expect(runtime.capabilities).toMatchObject({ recall: true, policy: true, extraction: false, embeddings: false });
  const ready = await runtime.status(0, false);
  expect(ready).toMatchObject({ version: 1, state: "ready", storage: "available", data_incarnation: installation.incarnation, fs_epoch: installation.epoch,
    spool: { pending: 0, blocked: 0, quarantined: 0, bytes: 0 }, outbox_pending: 0, workers: { extraction: { state: "unconfigured" }, embedding: { pending: 0, drained_total: 0, quarantined_total: 0, last_error: null } } });
  expect((await runtime.status(3, true))).toMatchObject({ state: "stopping", queue: { pending: 3 } });

  const first = delivery({ record: "first" });
  const committed = asCommitted(await runtime.remember(first));
  expect(committed).toMatchObject({ state: "committed", created: true, ...identityOf(first, installation.incarnation) });
  expect(lanes).toEqual(["spool", "spool"]);
  expect(await runtime.remember(first)).toEqual({ ...committed, created: false });
  await expect(runtime.remember(delivery({ record: "first", content: "a different body" }))).rejects.toMatchObject({ code: "revision_conflict" });

  const identity = identityOf(first, installation.incarnation);
  expect(await runtime.ingestStatus(identity)).toEqual({ ...committed, created: false });
  expect(await runtime.ingestStatus({ ...identity, body_digest: "0".repeat(64) })).toEqual({ state: "unknown", ...identity, body_digest: "0".repeat(64), storage: "available" });
  expect(await runtime.ingestStatus({ ...identity, revision_key: "1".repeat(64) })).toMatchObject({ state: "unknown", storage: "available" });
  await expect(runtime.ingestStatus({ ...identity, data_incarnation: uuidv7() })).rejects.toMatchObject({ code: "incarnation_mismatch" });
  await drainToRest(runtime);
  expect(await runtime.drainTurn()).toBe(false);
  expect((await runtime.status(0, false)).spool).toMatchObject({ pending: 0, blocked: 0 });
});

dbTest("storage loss spools deliveries, reports degraded, and the same incarnation drains the backlog in dependency order once storage returns", async () => {
  const installation = installationAt(await freshRoot());
  const offline = await boot(installation, DEAD_URI);
  expect(await offline.runtime.status(0, false)).toMatchObject({ state: "degraded", storage: "unavailable", outbox_pending: null });
  const parent = delivery({ record: "doc" });
  const child = delivery({ record: "doc", revision: "v2", predecessor: sourceRevisionKey(parent) });
  const spooledChild = await offline.runtime.remember(child);
  expect(spooledChild).toMatchObject({ state: "spooled", spool_seq: 1, fs_epoch: installation.epoch, ...identityOf(child, installation.incarnation) });
  expect(await offline.runtime.remember(parent)).toMatchObject({ state: "spooled", spool_seq: 2 });
  expect(await offline.runtime.remember(child)).toEqual(spooledChild);
  expect(await offline.runtime.ingestStatus(identityOf(child, installation.incarnation))).toEqual(spooledChild);
  expect(await offline.runtime.status(0, false)).toMatchObject({ state: "degraded", spool: { pending: 2 } });
  expect(await offline.runtime.drainTurn()).toBe(false);
  expect(await offline.runtime.embeddingTurn()).toBe("stalled");
  expect(await offline.runtime.extractionTurn()).toBe("stalled");
  await expect(offline.runtime.recall({ query: "doc", limit: 5 }, receiptContext)).rejects.toMatchObject({ code: "storage_unavailable", retryable: true });
  await expect(offline.runtime.remember({ ...delivery({ record: "lineage" }), origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, receiptContext))
    .rejects.toMatchObject({ code: "storage_unavailable", retryable: true });
  await shutdown(offline.runtime);

  const live = await boot(installation);
  expect(live.lanes).toEqual(["spool"]);
  await drainToRest(live.runtime);
  expect(await live.runtime.ingestStatus(identityOf(parent, installation.incarnation))).toMatchObject({ state: "committed", created: false });
  expect(await live.runtime.ingestStatus(identityOf(child, installation.incarnation))).toMatchObject({ state: "committed", created: false });
  expect(await live.runtime.remember(child)).toMatchObject({ state: "committed", created: false });
  expect(await live.runtime.status(0, false)).toMatchObject({ state: "ready", spool: { pending: 0, blocked: 0, quarantined: 0 } });
  expect(await query("MATCH (e:Episode) RETURN count(e) AS episodes")).toEqual([{ episodes: 2 }]);
});

dbTest("a spooled successor whose predecessor never arrives stays blocked and a live retry surfaces the stale CAS", async () => {
  const installation = installationAt(await freshRoot());
  const orphan = delivery({ record: "orphan", revision: "v2", predecessor: sha256("never delivered") });
  await spoolOffline(installation, orphan);
  const { runtime } = await boot(installation);
  await drainToRest(runtime);
  const identity = identityOf(orphan, installation.incarnation);
  expect(await runtime.ingestStatus(identity)).toEqual({ state: "blocked", ...identity, fs_epoch: installation.epoch, spool_seq: 1, expected_previous_revision_key: sha256("never delivered"), reason: "missing_predecessor" });
  expect(await runtime.status(0, false)).toMatchObject({ state: "ready", spool: { pending: 1, blocked: 1, quarantined: 0 } });
  await expect(runtime.remember(orphan)).rejects.toMatchObject({ code: "stale_revision" });
});

dbTest("mutually dependent spool entries are blocked as a dependency cycle and a tail entering the cycle as a missing predecessor", async () => {
  const installation = installationAt(await freshRoot());
  const keyA = sourceRevisionKey(delivery({ record: "cycle", revision: "a" }));
  const keyB = sourceRevisionKey(delivery({ record: "cycle", revision: "b" }));
  const a = delivery({ record: "cycle", revision: "a", predecessor: keyB });
  const b = delivery({ record: "cycle", revision: "b", predecessor: keyA });
  const tail = delivery({ record: "cycle", revision: "c", predecessor: keyA });
  await spoolOffline(installation, a, b, tail);
  const { runtime } = await boot(installation);
  await drainToRest(runtime);
  expect(await runtime.ingestStatus(identityOf(a, installation.incarnation))).toMatchObject({ state: "blocked", reason: "dependency_cycle", spool_seq: 1 });
  expect(await runtime.ingestStatus(identityOf(b, installation.incarnation))).toMatchObject({ state: "blocked", reason: "dependency_cycle", spool_seq: 2 });
  expect(await runtime.ingestStatus(identityOf(tail, installation.incarnation))).toMatchObject({ state: "blocked", reason: "missing_predecessor", spool_seq: 3 });
  expect((await runtime.status(0, false)).spool).toMatchObject({ pending: 3, blocked: 3 });
});

dbTest("drain blocks a stale successor, a body conflicting with a committed revision, and the entry behind them without stopping the cohort", async () => {
  const installation = installationAt(await freshRoot());
  const base = delivery({ record: "stale" });
  const stale = delivery({ record: "stale", revision: "v2", predecessor: sourceRevisionKey(base) });
  const behindStale = delivery({ record: "stale", revision: "v3", predecessor: sourceRevisionKey(stale) });
  const conflicting = delivery({ record: "conflict", content: "spooled body" });
  const liveBody = delivery({ record: "conflict", content: "live body" });
  await spoolOffline(installation, stale, behindStale, conflicting);
  // A lost delivery binding lets a different body for the same revision reach the store first; the spooled body
  // then meets the committed one at drain time instead of at admission.
  await rm(join(installation.root, "deliveries", `${sourceRevisionKey(conflicting)}.json`));
  const { runtime } = await boot(installation);
  expect(await runtime.remember(base)).toMatchObject({ state: "committed", created: true });
  expect(await runtime.remember(delivery({ record: "stale", revision: "v1b", predecessor: sourceRevisionKey(base) }))).toMatchObject({ state: "committed", created: true });
  expect(await runtime.remember(liveBody)).toMatchObject({ state: "committed", created: true });
  await drainToRest(runtime);
  expect(await runtime.ingestStatus(identityOf(stale, installation.incarnation))).toMatchObject({ state: "blocked", reason: "stale_revision" });
  expect(await runtime.ingestStatus(identityOf(behindStale, installation.incarnation))).toMatchObject({ state: "blocked", reason: "missing_predecessor" });
  expect(await runtime.ingestStatus(identityOf(conflicting, installation.incarnation))).toMatchObject({ state: "unknown", storage: "available" });
  expect(await runtime.ingestStatus(identityOf(liveBody, installation.incarnation))).toMatchObject({ state: "committed", created: false });
  await expect(runtime.remember(conflicting)).rejects.toMatchObject({ code: "revision_conflict" });
  expect((await runtime.status(0, false)).spool).toMatchObject({ pending: 3, blocked: 3 });
  expect(await query("MATCH (e:Episode {origin_record:'conflict'}) RETURN e.content AS content")).toEqual([{ content: "live body" }]);
});

dbTest("entries from another incarnation are quarantined while this incarnation keeps serving", async () => {
  const root = await freshRoot();
  const foreign = installationAt(root);
  const stranded = delivery({ record: "stranded" });
  await spoolOffline(foreign, stranded);
  const installation = installationAt(root);
  const { runtime } = await boot(installation);
  await drainToRest(runtime);
  const identity = identityOf(stranded, installation.incarnation);
  expect(await runtime.ingestStatus(identity)).toEqual({ state: "quarantined", ...identity, reason: "incarnation_mismatch" });
  expect(await runtime.status(0, false)).toMatchObject({ state: "degraded", storage: "available", spool: { pending: 1, quarantined: 1 } });
  await expect(runtime.remember(stranded)).rejects.toMatchObject({ code: "incarnation_mismatch" });
  expect(await runtime.remember(delivery({ record: "served" }))).toMatchObject({ state: "committed", created: true });
});

dbTest("a quarantined spool stops admission and drain, and reports every pending delivery as quarantined", async () => {
  const installation = installationAt(await freshRoot());
  const pending = delivery({ record: "pending" });
  await spoolOffline(installation, pending);
  const { runtime } = await boot(installation);
  await writeFile(join(installation.root, "spool", "spool.durable"), "0");
  expect(await drainToRest(runtime)).toBe(1);
  const identity = identityOf(pending, installation.incarnation);
  expect(await runtime.ingestStatus(identity)).toEqual({ state: "quarantined", ...identity, reason: "spool_corrupt" });
  await expect(runtime.remember(delivery({ record: "refused" }))).rejects.toMatchObject({ code: "spool_corrupt" });
  expect(await runtime.status(0, false)).toMatchObject({ state: "degraded", spool: { quarantined: 1 } });
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
  const committed = asCommitted(await runtime.remember(v2, receiptContext));
  expect(committed).toMatchObject({ state: "committed", created: true });
  expect(await runtime.remember(v2, receiptContext)).toEqual({ ...committed, created: false });
  const identity = identityOf(v2, installation.incarnation);
  expect(await runtime.ingestStatus(identity)).toEqual({ ...committed, created: false });
  await query("MATCH (l:EchoLineage {episode_id:$id}) SET l.digest = $digest", { id: committed.id, digest: "f".repeat(64) });
  await expect(runtime.ingestStatus(identity)).rejects.toMatchObject({ code: "lineage_mismatch" });
  await query("MATCH (l:EchoLineage {episode_id:$id}) DETACH DELETE l", { id: committed.id });
  await expect(runtime.ingestStatus(identity)).rejects.toMatchObject({ code: "lineage_unavailable" });
});

dbTest("stored rows that no longer verify are reported as digest faults instead of commits", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const params = delivery({ record: "verified" });
  const committed = asCommitted(await runtime.remember(params));
  const identity = identityOf(params, installation.incarnation);
  const [row] = await query("MATCH (e:Episode {id:$id}) RETURN e.digest AS digest, e.digest_format AS format", { id: committed.id });
  const digest = row?.["digest"], format = row?.["format"];
  if (typeof digest !== "string") throw new Error("the committed episode carries no digest");
  await query("MATCH (e:Episode {id:$id}) SET e.episode_digest_version = 3", { id: committed.id });
  await expect(runtime.ingestStatus(identity)).rejects.toMatchObject({ code: "unsupported_digest_version" });
  await query("MATCH (e:Episode {id:$id}) REMOVE e.episode_digest_version SET e.digest_format = 'unknown-format'", { id: committed.id });
  await expect(runtime.ingestStatus(identity)).rejects.toMatchObject({ code: "unsupported_digest_version" });
  if (typeof format === "string") await query("MATCH (e:Episode {id:$id}) SET e.digest_format = $format", { id: committed.id, format });
  else await query("MATCH (e:Episode {id:$id}) REMOVE e.digest_format", { id: committed.id });
  await query("MATCH (e:Episode {id:$id}) SET e.digest = $digest", { id: committed.id, digest: "e".repeat(64) });
  await expect(runtime.ingestStatus(identity)).rejects.toMatchObject({ code: "revision_conflict" });
  await query("MATCH (e:Episode {id:$id}) SET e.digest = $digest, e.content = 'tampered'", { id: committed.id, digest });
  await expect(runtime.ingestStatus(identity)).rejects.toMatchObject({ code: "revision_conflict" });
});

dbTest("a changed writer epoch is ownership loss for status and the spool lane, and cancelDrain stalls every lane", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  await runtime.remember(delivery({ record: "owned" }));
  await bumpWriterEpoch();
  await expect(runtime.status(0, false)).rejects.toMatchObject({ code: "ownership_lost" });
  await expect(runtime.drainTurn()).rejects.toMatchObject({ code: "ownership_lost" });
  await expect(runtime.remember(delivery({ record: "later" }))).rejects.toMatchObject({ code: "ownership_lost" });
  runtime.cancelDrain();
  expect(await runtime.drainTurn()).toBe(false);
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
  const runtime = await withEnv(env, () => Runtime.create(installation, lane => { lanes.push(lane); }));
  opened.push(runtime);
  expect(runtime.capabilities).toMatchObject({ extraction: true, embeddings: true });
  await runtime.init();
  expect([...lanes].sort()).toEqual(["embedding", "extraction", "spool"]);
  expect(await runtime.embeddingTurn()).toBe("idle");
  expect(await runtime.extractionTurn()).toBe("idle");
  expect(await runtime.extractionTurn()).toBe("idle");

  const committed = asCommitted(await runtime.remember(delivery({ record: "embedded" })));
  expect(lanes.slice(3).sort()).toEqual(["embedding", "extraction", "spool"]);
  expect(await runtime.embeddingTurn()).toBe("more");
  const deferred = await runtime.status(0, false);
  expect(deferred.workers.embedding).toMatchObject({ pending: 1, drained_total: 0, quarantined_total: 0 });
  expect(deferred.workers.embedding.last_error).toContain("deferred");
  expect(deferred.workers.extraction).toMatchObject({ pacing: { max_in_flight: 4, min_interval_ms: 0, jitter_fraction: 0.5, calls_total: 0, waited_total_ms: 0 } });
  expect(await runtime.embeddingTurn()).toBe("idle");
  expect((await runtime.status(0, false)).workers.embedding).toMatchObject({ pending: 1, last_error: null });
  expect(await runtime.requeueQuarantinedEmbeddings({ limit: 10 }, receiptContext)).toEqual({ requeued: 0 });
  expect(lanes.at(-1)).toBe("embedding");
  const operation = uuidv7();
  const attempt = await runtime.recoverEmbedding({ operation_id: operation, episode_id: committed.id }, receiptContext);
  expect(attempt).toMatchObject({ operation_id: operation, episode_id: committed.id, state: "deferred", reason: "provider_unavailable" });
  expect(await runtime.embeddingStatus(operation, receiptContext)).toEqual(attempt);
  const absent = uuidv7();
  expect(await runtime.embeddingStatus(absent, receiptContext)).toEqual({ state: "unknown", operation_id: absent });
  await expect(runtime.createExtractionPipeline({ id: uuidv7(), generation_id: uuidv7(), source_id: committed.id }, receiptContext)).rejects.toThrow("unknown_ExtractionGeneration");

  await runtime.remember(delivery({ record: "second" }));
  await bumpWriterEpoch();
  await expect(runtime.embeddingTurn()).rejects.toMatchObject({ code: "ownership_lost" });
  await expect(runtime.extractionTurn()).rejects.toMatchObject({ code: "ownership_lost" });
});

dbTest("storage-backed RPCs pass through to the engine under the caller's custody", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const committed = asCommitted(await runtime.remember(delivery({ record: "recallable", content: "the quick brown fox" })));
  const recall = await runtime.recall({ query: "quick brown fox", limit: 5 }, autoContext);
  expect(recall.results.map(item => item.id)).toEqual([committed.id]);
  expect(recall).toMatchObject({ renderer: "canonical-jsonl-v1", diagnostics: { pipeline: "originals-hybrid-v1", vector_reason: "not_configured", channels_used: ["bm25"] } });
  expect(await runtime.recordRecallTransport({ recall_id: recall.recall_id, state: "local_complete" }, autoContext))
    .toMatchObject({ recall_id: recall.recall_id, state: "local_complete", principal: "installation", commit_mode: "auto", boundary: "node-write-callback-v1" });
  expect(await runtime.exposeRecall(recall.recall_id, autoContext)).toEqual({ applied: 1 });
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
  await expect(runtime.recoverEmbedding({ operation_id: uuidv7(), episode_id: committed.id }, receiptContext)).rejects.toMatchObject({ code: "embedding_not_configured" });
  expect(await runtime.embeddingStatus(uuidv7(), receiptContext)).toMatchObject({ state: "unknown" });
});

dbTest("a prior revision denied by episode id is withheld from the primary's supersedes view", async () => {
  const installation = installationAt(await freshRoot());
  const { runtime } = await boot(installation);
  const original = delivery({ record: "revised", content: "the sleepy owl hoots" });
  const first = asCommitted(await runtime.remember(original));
  const second = asCommitted(await runtime.remember(delivery({ record: "revised", revision: "v2", predecessor: sourceRevisionKey(original), content: "the sleepy owl hoots at dusk" })));
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
  expect(calls).toEqual(["stop", "restoreOffline", "verifyPhysicalLinks", "startAndReady", "restoredAuthoritySnapshot", "rebindSource"]);
});
