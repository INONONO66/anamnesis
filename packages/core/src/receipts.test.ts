import { afterAll, beforeAll, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j, { type RecordShape, type Record as Neo4jRecord } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine as CoreEngine } from "./engine.ts";
import { CommitReceiptInput, IssueReceiptInput, ReceiptHit } from "./store.ts";
import { adopt, initialStability } from "./dynamics/retention.ts";
import { ADOPTION_NUMERIC_VERSION } from "./dynamics/adoption-numeric.ts";

// These privileged core fixtures supply the same server-owned context as an
// authenticated receipt-mode connection; no evaluator or revision is supplied.
const context = { principal: "installation", commit_mode: "receipt" } as const;
class Engine extends CoreEngine {
  override issueReceipt(input: IssueReceiptInput) { return super.issueReceipt(input, context); }
  override commitReceipt(input: CommitReceiptInput) { return super.commitReceipt(input, context); }
}

const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("Isolated test database credentials required");
const root = await mkdtemp(join(tmpdir(), "g003-core-receipts-"));
const BASE = Date.UTC(2030, 0, 1);
let now = BASE;
const options = { uri, password, objectsRoot: root, clock: () => now };
const engine = new Engine(options);
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const hash = (body: string) => createHash("sha256").update(body).digest("hex");
async function query<Row extends RecordShape>(cypher: string, params: Record<string, unknown> = {}): Promise<Row[]> {
  return (await admin.executeQuery<Row>(cypher, params)).records.map((row: Neo4jRecord<Row>) => row.toObject());
}
async function episode(): Promise<string> {
  return (await engine.remember({ content: "G003 immutable source", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "g003-core", session: root, actor: "test", record: uuidv7() }, payload: new TextEncoder().encode("G003 source bytes") })).id;
}
async function issued(ids: string[]) {
  return engine.issueReceipt({ recall_id: uuidv7(), primary_ids: ids });
}
async function sourceSnapshot() {
  return query(`MATCH (n) WHERE NOT n:Receipt AND NOT n:RecallOutcome AND NOT n:Hit AND NOT n:HitCache
    RETURN elementId(n) AS identity, labels(n) AS labels, properties(n) AS props ORDER BY identity`);
}
async function authoritySnapshot() {
  return query(`MATCH (n) WHERE NOT n:HitCache
    OPTIONAL MATCH (n)-[r]->(target) WHERE NOT target:HitCache
    WITH n,r,target ORDER BY elementId(r)
    WITH n, collect({identity:elementId(r), type:type(r), target:elementId(target), props:properties(r)}) AS edges
    RETURN elementId(n) AS identity, labels(n) AS labels, properties(n) AS props, edges ORDER BY identity`);
}
async function graphSnapshot() {
  return query(`MATCH (n) OPTIONAL MATCH (n)-[r]->(target)
    WITH n,r,target ORDER BY elementId(r)
    WITH n, collect({identity:elementId(r), type:type(r), target:elementId(target), props:properties(r)}) AS edges
    RETURN elementId(n) AS identity, labels(n) AS labels, properties(n) AS props, edges ORDER BY identity`);
}
async function hits(recallId: string) {
  return query<{ body: string; episode: string; operation: string }>(`MATCH (h:Hit {namespace:$id})-[:HIT_OF]->(e:Element:Episode)
    MATCH (h)-[:RECORDED_BY]->(f:Receipt:RecallFeedback)
    RETURN h.body AS body,e.id AS episode,f.operation_id AS operation ORDER BY h.t,h.id`, { id: recallId });
}
beforeAll(async () => { await engine.init(); await engine.claimWriterEpoch(); });
afterAll(async () => { await engine.close(); await admin.close(); await rm(root, { recursive: true }); });

test("core receipt persistence and cache APIs exist on a real initialized database", async () => {
  const id = await episode();
  console.log(JSON.stringify({ fixture: { episode_id: id, recall_id: uuidv7(), primary_ids: [id] } }));
  for (const name of ["issueReceipt", "commitReceipt", "getReceipt", "getReceiptStatus", "verifyHitCache", "rebuildHitCache"] as const) {
    expect(typeof Reflect.get(engine, name)).toBe("function");
    expect(typeof Reflect.get(engine.store, name)).toBe("function");
  }
});

test("Neo4j constraints enforce immutable identity and replay indexes are online", async () => {
  const constraints = await query<{ name: string }>("SHOW CONSTRAINTS YIELD name RETURN name");
  const names = constraints.map((row) => row.name);
  for (const name of ["recall_receipt_id", "receipt_operation_id", "recall_outcome_id", "hit_id", "hit_idem_key", "hit_cache_episode"]) expect(names).toContain(name);
  const indexes = await query<{ name: string; state: string }>("SHOW INDEXES YIELD name,state WHERE name IN ['receipt_expiry','hit_replay'] RETURN name,state");
  expect(indexes).toHaveLength(2);
  expect(indexes.every((row) => row.state === "ONLINE")).toBe(true);
  const receipt = await issued([await episode()]);
  const result = await engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: receipt.primary_ids, reward: -1 });
  const hit = ReceiptHit.parse(JSON.parse((await hits(receipt.recall_id))[0]!.body));
  for (const [label, field, value] of [
    ["RecallReceipt", "recall_id", receipt.recall_id], ["RecallFeedback", "operation_id", result.operation_id],
    ["RecallOutcome", "recall_id", receipt.recall_id], ["Hit", "id", hit.id], ["Hit", "idem_key", hit.idem_key],
  ]) await expect(query(`CREATE (n:${label} {${field}:$value})`, { value })).rejects.toThrow();
  const linked = await query<{ rank: number; operation: string }>(`MATCH (r:Receipt:RecallReceipt {recall_id:$id})-[p:PRIMARY]->(:Element:Episode)
    MATCH (f:Receipt:RecallFeedback)-[:FEEDBACK_OF]->(r) MATCH (:RecallOutcome)-[:ACCEPTED_BY]->(f)
    RETURN p.rank AS rank,f.operation_id AS operation`, { id: receipt.recall_id });
  expect(linked).toEqual([{ rank: 0, operation: result.operation_id }]);
  console.log(JSON.stringify({ constraints: names, indexes, receipt, hit, linked }));
});

test("issuance derives sources and ranks; immutable canonical bodies survive restart and conflict atomically", async () => {
  const ids = [await episode(), await episode()];
  const before = await sourceSnapshot();
  const input = { recall_id: uuidv7(), primary_ids: ids };
  const receipt = await engine.issueReceipt(input);
  expect(receipt.primaries).toEqual(ids.map((id, rank) => ({ id, rank, sources: [id] })));
  expect(receipt.body_digest).toBe(hash(`{"primary_ids":${JSON.stringify(ids)},"recall_id":"${input.recall_id}","receipt_ttl_ms":3600000}`));
  expect(receipt.expires_at).toBe(BASE + 3_600_000);
  expect(receipt.policy_revision).toBe(0);
  const snapshot = await graphSnapshot();
  expect(await engine.issueReceipt({ primary_ids: ids, recall_id: input.recall_id, receipt_ttl_ms: 3_600_000 })).toEqual(receipt);
  await expect(engine.issueReceipt({ ...input, primary_ids: [...ids].reverse() })).rejects.toThrow("idempotency_conflict");
  expect(await graphSnapshot()).toEqual(snapshot);
  const restarted = new Engine(options);
  try { expect(await restarted.getReceipt(input.recall_id)).toEqual(receipt); }
  finally { await restarted.close(); }
  expect(await sourceSnapshot()).toEqual(before);
});

test("feedback only references issued receipts and rejects forged sources, malformed IDs and unknown selections", async () => {
  const id = await episode();
  const receipt = await issued([id]);
  const request = { operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: [id] };
  const before = await graphSnapshot();
  for (const forged of [{ ...request, sources: [id] }, { ...request, rank: 0 }, { ...request, kappa_eff: 1 }, { ...request, t: BASE }, { ...request, adopted: [id, id] }, { ...request, reward: NaN }, { ...request, reward: 2 }, { ...request, reward: null }, { operation_id: uuidv7(), recall_id: receipt.recall_id }]) {
    expect(CommitReceiptInput.safeParse(forged).success).toBe(false);
  }
  expect(IssueReceiptInput.safeParse({ recall_id: uuidv7(), primary_ids: [id], sources: [id] }).success).toBe(false);
  expect(IssueReceiptInput.safeParse({ recall_id: "550e8400-e29b-41d4-a716-446655440000", primary_ids: [id] }).success).toBe(false);
  await expect(engine.commitReceipt({ ...request, recall_id: uuidv7() })).rejects.toThrow("unknown_recall");
  await expect(engine.commitReceipt({ ...request, adopted: [uuidv7()] })).rejects.toThrow("invalid_selection");
  await expect(engine.issueReceipt({ recall_id: uuidv7(), primary_ids: [uuidv7()] })).rejects.toThrow("invalid_selection");
  expect(await graphSnapshot()).toEqual(before);
  expect(await engine.getReceipt(uuidv7())).toBeNull();
  expect(await engine.getReceiptStatus(request.operation_id)).toEqual({ state: "unknown", operation_id: request.operation_id });
});

test("same-operation concurrent duplicates converge; different bodies never SET immutable rows or consume sequence", async () => {
  const receipt = await issued([await episode(), await episode()]);
  const request = { operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: receipt.primary_ids, reward: -1 };
  const before = await sourceSnapshot();
  const results = await Promise.all(Array.from({ length: 4 }, () => engine.commitReceipt(request)));
  expect(results.filter((result) => result.applied)).toHaveLength(1);
  const snapshot = await graphSnapshot();
  expect(await engine.commitReceipt({ reward: -1, adopted: [...receipt.primary_ids].reverse(), recall_id: receipt.recall_id, operation_id: request.operation_id })).toEqual({ ...results[0]!, applied: false });
  await expect(engine.commitReceipt({ ...request, reward: 0 })).rejects.toThrow("idempotency_conflict");
  expect(await graphSnapshot()).toEqual(snapshot);
  const status = await engine.getReceiptStatus(request.operation_id);
  expect(status.state).toBe("committed");
  if (status.state !== "committed") throw new Error("Missing committed fixture");
  expect(status.result).toEqual(results.find((result) => result.applied)!);
  expect(status.created_at).toBe(BASE);
  const canonical = `{"adopted":${JSON.stringify([...request.adopted].sort())},"operation_id":"${request.operation_id}","recall_id":"${request.recall_id}","reward":-1}`;
  expect(status.body_digest).toBe(hash(canonical));
  expect(await hits(receipt.recall_id)).toHaveLength(4);
  expect(await sourceSnapshot()).toEqual(before);
});

test("cross-operation adoption is once per source; outcome conflict precedes any new adoption", async () => {
  const ids = [await episode(), await episode()];
  const receipt = await issued(ids);
  const commit = (adopted: string[], reward?: number) => engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, adopted, ...(reward === undefined ? {} : { reward }) });
  await commit([ids[0]!], -1);
  expect((await commit([ids[0]!], -1)).applied).toBe(false);
  const before = await graphSnapshot();
  await expect(commit(ids, -1)).rejects.toThrow("idempotency_conflict");
  await expect(commit([ids[1]!], 1)).rejects.toThrow("idempotency_conflict");
  expect(await graphSnapshot()).toEqual(before);
  expect(await engine.getHitCache(ids[1]!)).toBeNull();
  expect((await commit(ids)).applied).toBe(true);
  expect((await commit(ids)).applied).toBe(false);
  const ledger = (await hits(receipt.recall_id)).map((row) => ReceiptHit.parse(JSON.parse(row.body)));
  expect(ledger.filter((hit) => hit.kind === "recall_hit")).toHaveLength(2);
  expect(ledger.filter((hit) => hit.kind === "outcome")).toHaveLength(1);
});

test("negative reward changes utility only; selected subset keeps original rank and missing adopted selects all", async () => {
  const ids = [await episode(), await episode()];
  const receipt = await issued(ids);
  const adopted = await engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: [ids[1]!] });
  expect(adopted.applied).toBe(true);
  const before = await engine.getHitCache(ids[1]!);
  const sources = await sourceSnapshot();
  await engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, reward: -1 });
  const after = await engine.getHitCache(ids[1]!);
  expect(after?.s).toBe(before?.s);
  expect(after?.t_last_hit).toBe(before?.t_last_hit);
  expect(after?.utility_weight).toBeCloseTo(1 / 3, 15);
  expect(after?.utility_reward_sum).toBeCloseTo(-1 / 3, 15);
  expect(after?.utility).toBeCloseTo(-1 / 13, 15);
  const first = await engine.getHitCache(ids[0]!);
  expect(first?.utility_weight).toBeCloseTo(2 / 3, 15);
  expect(first?.utility).toBeCloseTo(-1 / 7, 15);
  const original = await query<{ mass: number; ingested: number }>("MATCH (e:Episode {id:$id}) RETURN e.mass AS mass,e.ingested_at AS ingested", { id: ids[0] });
  expect(first?.s).toBe(initialStability(original[0]!.mass));
  expect(first?.t_last_hit).toBe(original[0]!.ingested);
  const subset = await issued(ids);
  await engine.commitReceipt({ operation_id: uuidv7(), recall_id: subset.recall_id, adopted: [ids[1]!], reward: -1 });
  const outcome = (await hits(subset.recall_id)).map((row) => ReceiptHit.parse(JSON.parse(row.body))).find((hit) => hit.kind === "outcome");
  expect(outcome).toMatchObject({ weight: 1, reward: -1, kappa_eff: 0, attribution: [{ id: ids[1], rank: 1, sources: [ids[1]] }] });
  expect(await sourceSnapshot()).toEqual(sources);
});

test("reward zero contributes weight, explicit empty adopted and empty receipt retain only control verdicts", async () => {
  const id = await episode();
  const zero = await issued([id]);
  await engine.commitReceipt({ operation_id: uuidv7(), recall_id: zero.recall_id, reward: 0 });
  expect(await engine.getHitCache(id)).toMatchObject({ utility_reward_sum: 0, utility_weight: 1, utility: 0, hit_count: 1 });
  for (const receipt of [await issued([id]), await issued([])]) {
    const result = await engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: [], reward: -1 });
    expect(result.applied).toBe(true);
    expect(await hits(receipt.recall_id)).toEqual([]);
    const outcomes = await query<{ reward: number; selected: string[] }>("MATCH (o:RecallOutcome {recall_id:$id}) RETURN o.reward AS reward,o.selected AS selected", { id: receipt.recall_id });
    expect(outcomes).toEqual([{ reward: -1, selected: [] }]);
  }
});

test("expiry is exact, rejects same and different operation retries, and survives engine restart", async () => {
  const receipt = await issued([await episode()]);
  const request = { operation_id: uuidv7(), recall_id: receipt.recall_id, reward: -1 };
  now = receipt.expires_at - 1;
  try {
    await engine.commitReceipt(request);
    const before = await graphSnapshot();
    now = receipt.expires_at;
    const restarted = new Engine(options);
    try {
      await expect(restarted.commitReceipt(request)).rejects.toThrow("receipt_expired");
      await expect(restarted.commitReceipt({ ...request, operation_id: uuidv7() })).rejects.toThrow("receipt_expired");
      expect(await restarted.getReceipt(receipt.recall_id)).toEqual(receipt);
    } finally { await restarted.close(); }
    expect(await graphSnapshot()).toEqual(before);
  } finally { now = BASE; }
});

test("verify is read-only, rebuild repairs values despite equal counts and preserves every source and Hit identity", async () => {
  const receipt = await issued([await episode()]);
  await engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: receipt.primary_ids, reward: -1 });
  const id = receipt.primary_ids[0]!;
  const expected = await engine.getHitCache(id);
  const authority = await authoritySnapshot();
  await query("MATCH (c:HitCache {episode_id:$id}) SET c.s=999,c.utility_reward_sum=999,c.event_ids=[]", { id });
  const damaged = await graphSnapshot();
  const verified = await engine.verifyHitCache();
  expect(verified.issues).toContainEqual({ code: "hit_cache_mismatch", id });
  expect(await graphSnapshot()).toEqual(damaged);
  const rebuilt = await engine.rebuildHitCache();
  expect(rebuilt).toMatchObject({ state: "rebuilt", created: 1, removed: 1 });
  expect(await engine.getHitCache(id)).toEqual(expected);
  expect(await authoritySnapshot()).toEqual(authority);
  expect((await engine.verifyHitCache()).issues).toEqual([]);
  const snapshot = await graphSnapshot();
  expect(await engine.rebuildHitCache()).toEqual({ ...rebuilt, created: 0, removed: 0 });
  expect(await graphSnapshot()).toEqual(snapshot);
  console.log(JSON.stringify({ verified, rebuilt, cache: expected, authority_preserved: true }));
});

test("retained Hits alone reconstruct caches after receipt removal, including deterministic out-of-order adoption", async () => {
  const id = await episode();
  const first = await issued([id]);
  const second = await issued([id]);
  now = BASE + 2000;
  try {
    await engine.commitReceipt({ operation_id: uuidv7(), recall_id: first.recall_id, adopted: [id] });
    now = BASE + 1000;
    await engine.commitReceipt({ operation_id: uuidv7(), recall_id: second.recall_id, adopted: [id], reward: -1 });
  } finally { now = BASE; }
  const original = (await query<{ mass: number; ingested: number }>("MATCH (e:Episode {id:$id}) RETURN e.mass AS mass,e.ingested_at AS ingested", { id }))[0]!;
  let state = { stability: initialStability(original.mass), lastHit: original.ingested, hitCount: 0 };
  state = adopt(state, BASE + 1000, 1); state = adopt(state, BASE + 2000, 1);
  const expected = await engine.getHitCache(id);
  expect(expected?.s).toBe(state.stability);
  expect(expected?.t_last_hit).toBe(BASE + 2000);
  await query("MATCH (r:Receipt) WHERE r.recall_id IN $ids DETACH DELETE r", { ids: [first.recall_id, second.recall_id] });
  const authority = await authoritySnapshot();
  await query("MATCH (c:HitCache {episode_id:$id}) DETACH DELETE c", { id });
  expect((await engine.verifyHitCache()).issues).toContainEqual({ code: "hit_cache_mismatch", id });
  await engine.rebuildHitCache();
  expect(await engine.getHitCache(id)).toEqual(expected);
  expect(await authoritySnapshot()).toEqual(authority);
  expect((await engine.verifyHitCache()).issues).toEqual([]);
});

test("numeric-version rebuild preserves utility and authority; even one-ULP corruption still fails exact verification", async () => {
  const id = await episode(), receipt = await issued([id]);
  await engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: [id], reward: -1 });
  const expected = (await engine.getHitCache(id))!;
  expect(expected.numeric_version).toBe(ADOPTION_NUMERIC_VERSION);
  const authority = await authoritySnapshot();
  const original = (await query<{ mass: number; ingested: number }>("MATCH (e:Episode {id:$id}) RETURN e.mass AS mass,e.ingested_at AS ingested", { id }))[0]!;
  const s = initialStability(original.mass), gap = Math.max(0, BASE - original.ingested) / 86400000;
  const legacy = Math.min(3650, s + s * 5 * (Math.exp(1 - Math.pow(1 + (19 / 81) * gap / s, -0.5)) - 1) * Math.pow(s, -0.1));
  await query("MATCH (c:HitCache {episode_id:$id}) SET c.s=$s REMOVE c.numeric_version", { id, s: legacy });
  const legacyCache = (await engine.getHitCache(id))!;
  expect((await engine.verifyHitCache()).issues).toContainEqual({ code: "hit_cache_mismatch", id });
  expect(await engine.getHitCache(id)).toEqual(legacyCache); // Verification never migrates implicitly.
  expect(await engine.rebuildHitCache()).toMatchObject({ created: 1, removed: 1 });
  expect(await engine.getHitCache(id)).toEqual(expected);
  for (const field of ["s", "utility", "utility_reward_sum", "utility_weight"] as const) {
    const bits = new DataView(new ArrayBuffer(8)); bits.setFloat64(0, expected[field]);
    bits.setBigUint64(0, bits.getBigUint64(0) + 1n);
    await query(`MATCH (c:HitCache {episode_id:$id}) SET c.${field}=$value`, { id, value: bits.getFloat64(0) });
    expect((await engine.verifyHitCache()).issues).toContainEqual({ code: "hit_cache_mismatch", id });
    expect(await engine.rebuildHitCache()).toMatchObject({ created: 1, removed: 1 });
    expect(await engine.getHitCache(id)).toEqual(expected);
  }
  expect(await authoritySnapshot()).toEqual(authority);
  expect((await engine.verifyHitCache()).issues).toEqual([]);
  console.log(JSON.stringify({ numeric_version: expected.numeric_version, legacy_s: legacyCache.s, canonical_s: expected.s,
    utility_preserved: legacyCache.utility === expected.utility && legacyCache.utility_reward_sum === expected.utility_reward_sum && legacyCache.utility_weight === expected.utility_weight,
    exact_corruption_checks: 4, authority_preserved: true }));
});

test("damaged immutable Hit bodies and target relationships fail verification and rebuild without laundering evidence", async () => {
  const receipt = await issued([await episode()]);
  await engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, reward: -1 });
  const body = (await hits(receipt.recall_id))[0]!.body;
  const hit = ReceiptHit.parse(JSON.parse(body));
  await query("MATCH (h:Hit {id:$id}) SET h.body='{'", { id: hit.id });
  const damaged = await graphSnapshot();
  expect((await engine.verifyHitCache()).issues).toContainEqual({ code: "invalid_hit_evidence", id: hit.id });
  await expect(engine.rebuildHitCache()).rejects.toThrow("invalid_hit_evidence");
  expect(await graphSnapshot()).toEqual(damaged);
  // Restore the exact immutable bytes captured before this deliberate corruption.
  await query("MATCH (h:Hit {id:$id}) SET h.body=$body", { id: hit.id, body });
  await query("MATCH (h:Hit {id:$id})-[r:HIT_OF]->(e) CREATE (h)-[:HIT_OF]->(e)", { id: hit.id });
  expect((await engine.verifyHitCache()).issues).toContainEqual({ code: "invalid_hit_evidence", id: hit.id });
  await expect(engine.rebuildHitCache()).rejects.toThrow("invalid_hit_evidence");
  await query("MATCH (h:Hit {id:$id})-[r:HIT_OF]->() WITH r ORDER BY elementId(r) DESC SKIP 1 DELETE r", { id: hit.id });
  expect((await engine.verifyHitCache()).issues).toEqual([]);
});

test("every new mutation is epoch-fenced; structure and policy revisions are not changed or fabricated", async () => {
  const receipt = await issued([await episode()]);
  const newer = new Engine(options);
  try {
    await newer.claimWriterEpoch();
    const before = await graphSnapshot();
    const mutations = [
      () => engine.issueReceipt({ recall_id: uuidv7(), primary_ids: receipt.primary_ids }),
      () => engine.commitReceipt({ operation_id: uuidv7(), recall_id: receipt.recall_id, reward: 0 }),
      () => engine.rebuildHitCache(),
    ];
    for (const mutation of mutations) await expect(mutation()).rejects.toThrow("stale_writer_epoch");
    expect(await graphSnapshot()).toEqual(before);
    expect(await engine.getReceipt(receipt.recall_id)).toEqual(receipt);
    await query("MATCH (m:Meta {key:'meta'}) SET m.structure_revision=73");
    const source = await sourceSnapshot();
    const revised = await newer.issueReceipt({ recall_id: uuidv7(), primary_ids: receipt.primary_ids });
    expect(revised.structure_revision).toBe(73);
    expect(revised.policy_revision).toBe(0);
    await newer.commitReceipt({ operation_id: uuidv7(), recall_id: revised.recall_id, reward: 0 });
    await newer.rebuildHitCache();
    expect(await sourceSnapshot()).toEqual(source);
    await query("MATCH (m:Meta {key:'meta'}) SET m.policy_revision=1");
    const policySnapshot = await graphSnapshot();
    await expect(newer.commitReceipt({ operation_id: uuidv7(), recall_id: revised.recall_id, reward: 0 })).rejects.toThrow("policy_unavailable");
    await expect(newer.issueReceipt({ recall_id: uuidv7(), primary_ids: receipt.primary_ids })).rejects.toThrow("policy_unavailable");
    expect(await graphSnapshot()).toEqual(policySnapshot);
  } finally {
    await query("MATCH (m:Meta {key:'meta'}) REMOVE m.structure_revision SET m.policy_revision=0");
    await newer.close(); await engine.claimWriterEpoch();
  }
});
