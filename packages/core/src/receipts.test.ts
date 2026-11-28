import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j, { type RecordShape, type Record as Neo4jRecord } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";
const context = { principal: "installation", client_binding: uuidv7() } as const;
const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("Isolated test database credentials required");
const root = await mkdtemp(join(tmpdir(), "g003-core-receipts-"));
const BASE = Date.UTC(2030, 0, 1);
const now = BASE;
const options = { uri, password, objectsRoot: root, clock: () => now };
const engine = new Engine(options);
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
async function query<Row extends RecordShape>(cypher: string, params: Record<string, unknown> = {}): Promise<Row[]> {
  return (await admin.executeQuery<Row>(cypher, params)).records.map((row: Neo4jRecord<Row>) => row.toObject());
}
async function episode(): Promise<string> {
  return (await engine.remember({ content: "G003 immutable source", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: "g003-core", session: root, actor: "test", record: uuidv7() }, payload: new TextEncoder().encode("G003 source bytes") })).id;
}
async function recall(id: string) {
  const result = await engine.recallHybrid({ query: id, limit: 1, window: 0 }, context);
  expect(result.results.map(item => item.id)).toEqual([id]);
  return result.usage_id;
}
const request = (usage_id: string, reward: number) => ({ usage_id, idempotency_key: uuidv7(), reward });
beforeAll(async () => { await engine.init(); await engine.claimWriterEpoch(); });
afterAll(async () => { await engine.close(); await admin.close(); await rm(root, { recursive: true }); });

test("simultaneous reports serialize; normalized retries preserve immutable outcome and Hit identities", async () => {
  const id = await episode(), usage_id = await recall(id), other = await recall(id);
  const input = request(usage_id, 7);
  const results = await Promise.all([engine.reportOutcome(input, context), engine.reportOutcome(input, context)]);
  expect(results.map(r => r.applied).sort()).toEqual([false, true]);
  const snapshot = () => query("MATCH (n) WHERE n:UsageOutcome OR n:Hit OR n:HitCache RETURN elementId(n) AS id,properties(n) AS props ORDER BY id");
  const original = await snapshot();
  expect(await engine.reportOutcome({ ...input, reward: 1 }, context)).toEqual({ applied: false, reward: 1 });
  for (const conflict of [{ ...input, reward: -1 }, { ...input, usage_id: other }])
    await expect(engine.reportOutcome(conflict, context)).rejects.toMatchObject({ code: "idempotency_conflict" });
  await expect(engine.reportOutcome(request(usage_id, 1), context)).rejects.toMatchObject({ code: "outcome_already_reported" });
  await expect(engine.reportOutcome(request(uuidv7(), 1), context)).rejects.toMatchObject({ code: "unknown_usage" });
  expect(await snapshot()).toEqual(original);
  expect(await engine.getHitCache(id)).toMatchObject({ hit_count: 1, u_anchor: 0.2, u_at: now });
});

test("denied source blocks new verdicts and accepted retries without partial credit", async () => {
  const id = await episode(), input = request(await recall(id), -2), other = request(await recall(id), 0);
  expect(await engine.reportOutcome(input, context)).toEqual({ applied: true, reward: -1 });
  const policy_id = uuidv7();
  await engine.setPolicy({ policy_id, selector: { episode_id: id }, scope: "content" }, context);
  try {
    for (const report of [input, other]) await expect(engine.reportOutcome(report, context)).rejects.toMatchObject({ code: "policy_denied" });
    expect(await query("MATCH (o:UsageOutcome {usage_id:$id}) RETURN o", { id: other.usage_id })).toEqual([]);
    expect(await engine.getHitCache(id)).toMatchObject({ hit_count: 1, u_anchor: -0.2 });
  } finally { await engine.revokePolicy({ policy_id }, context); }
});

test("zero-budget recall retains an empty selection and accepts one verdict without Hits", async () => {
  const result = await engine.recallHybrid({ query: await episode(), budget: { unit: "utf8_bytes", limit: 0 } }, context);
  expect(result.results).toEqual([]);
  const input = request(result.usage_id, -0);
  expect(await engine.reportOutcome(input, context)).toEqual({ applied: true, reward: 0 });
  expect(await query("MATCH (h:Hit {usage_id:$id}) RETURN h", { id: result.usage_id })).toEqual([]);
  expect(await engine.reportOutcome(input, context)).toEqual({ applied: false, reward: 0 });
});

test("verification is read-only; rebuilding repairs cache values without rewriting Hits", async () => {
  const id = await episode(), usage_id = await recall(id);
  await engine.reportOutcome(request(usage_id, 1), context);
  const immutable = () => query("MATCH (h:Hit {usage_id:$id})-[r:HIT_OF]->(e) RETURN elementId(h) AS hit, elementId(r) AS edge,e.id AS target,properties(h) AS props", { id: usage_id });
  const before = await immutable(), cache = await engine.getHitCache(id);
  await query("MATCH (c:HitCache {episode_id:$id}) SET c.u_anchor=0.19999999999999998", { id });
  expect((await engine.verifyHitCache()).issues).toContainEqual({ code: "hit_cache_mismatch", id });
  expect(await engine.getHitCache(id)).not.toEqual(cache);
  expect(await engine.rebuildHitCache()).toMatchObject({ created: 1, removed: 1 });
  expect(await engine.getHitCache(id)).toEqual(cache);
  expect(await immutable()).toEqual(before);
  await query("MATCH (h:Hit {usage_id:$id}) SET h.credit=-h.credit", { id: usage_id });
  try {
    expect((await engine.verifyHitCache()).issues.map(issue => issue.code)).toContain("invalid_hit_evidence");
    await expect(engine.rebuildHitCache()).rejects.toMatchObject({ code: "invalid_hit_evidence" });
    expect(await engine.getHitCache(id)).toEqual(cache);
  } finally { await query("MATCH (h:Hit {usage_id:$id}) SET h.credit=-h.credit", { id: usage_id }); }
});

test("an obsolete writer cannot report or issue Usage", async () => {
  const id = await episode(), input = request(await recall(id), 1), writer = new Engine(options);
  try {
    await writer.claimWriterEpoch();
    await expect(engine.reportOutcome(input, context)).rejects.toThrow("stale_writer_epoch");
    await expect(engine.recallHybrid({ query: id }, context)).rejects.toThrow("stale_writer_epoch");
    expect(await query("MATCH (o:UsageOutcome {usage_id:$id}) RETURN o", { id: input.usage_id })).toEqual([]);
  } finally { await writer.close(); await engine.claimWriterEpoch(); }
});

for (const label of ["RecallReceipt", "RecallTransport", "RecallFeedback", "RecallOutcome", "Hit"]) test(
  "readiness of a store recorded before schema 2 refuses retained " + label, async () => {
    const [{ id } = { id: "" }] = await query<{ id: string }>("CREATE (n:" + label + " {kind:'exposure'}) RETURN elementId(n) AS id");
    await query("MATCH (m:Meta {key:'meta'}) SET m.schema_version=1");
    try { await expect(engine.init()).rejects.toThrow("readiness refused"); }
    finally {
      await query("MATCH (n) WHERE elementId(n)=$id DELETE n", { id });
      await query("MATCH (m:Meta {key:'meta'}) SET m.schema_version=2");
    }
  });

test("a store already recorded at schema 2 starts without rescanning for receipt-era rows", async () => {
  const [{ id } = { id: "" }] = await query<{ id: string }>("CREATE (n:RecallReceipt) RETURN elementId(n) AS id");
  try { await engine.init(); }
  finally { await query("MATCH (n) WHERE elementId(n)=$id DELETE n", { id }); }
});
