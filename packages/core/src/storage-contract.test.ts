import { afterAll, beforeAll, expect, test } from "bun:test";
import { createHash, randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { Engine, type RememberInput } from "./engine.ts";
import { EpisodeJournal } from "./journal.ts";

const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("Isolated test database credentials required");
const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const directory = await mkdtemp(join(tmpdir(), "storage-contract-"));
const engine = new Engine({ uri, password, objectsRoot: directory });
const hash = (value: string): string => createHash("sha256").update(value).digest("hex");

function input(record: string = randomUUID(), session: string = randomUUID(), minute = "00"): RememberInput {
  return { content: "Storage contract", time: { value: `2026-09-01T00:${minute}:00Z`, precision: "second" },
    origin: { source: "storage-contract", session, actor: "user", record } };
}

async function revision(id: string): Promise<string> {
  const result = await driver.executeQuery("MATCH (e:Element {id:$id}) RETURN e.revision_key AS key", { id });
  const row = result.records[0];
  if (!row) throw new Error("Missing fixture");
  return row.get("key");
}

async function edges(session: string): Promise<string[][]> {
  const result = await driver.executeQuery(
    `MATCH (a:Episode)-[:NEXT_EPISODE]->(b:Episode) WHERE a.origin_session=$session
     RETURN a.id AS from,b.id AS to ORDER BY from,to`, { session });
  return result.records.map((row) => [row.get("from"), row.get("to")]);
}

beforeAll(async () => { await engine.init(); });
afterAll(async () => {
  await driver.executeQuery("MATCH (e:Element {origin_source:'storage-contract'}) DETACH DELETE e");
  await engine.close();
  await driver.close();
  await rm(directory, { recursive: true });
});

test("reordered nested properties retry the same canonical revision", async () => {
  const base = input();
  const first = await engine.remember({ ...base, properties: { z: { b: 2, a: 1 }, a: [3, { y: true, x: null }] } });
  expect(await engine.remember({ ...base, properties: { a: [3, { x: null, y: true }], z: { a: 1, b: 2 } } }))
    .toEqual({ id: first.id, created: false });
});

test("new digests use RFC8785 key ordering including integer-looking keys", async () => {
  const base = input();
  const stored = await engine.remember({ ...base, properties: { "2": -0, "10": 1e30, z: "\u20ac" } });
  const rows = await driver.executeQuery(
    "MATCH (e:Element {id:$id}) RETURN e.digest AS digest,e.digest_format AS format", { id: stored.id });
  const body = '{"content":"Storage contract","payload_hash":null,"previous_revision_key":null,"properties":{"10":1e+30,"2":0,"z":"€"},"schema":"anamnesis.original-message/1","time":{"precision":"second","value":"2026-09-01T00:00:00Z"}}';
  expect(rows.records[0]?.get("digest")).toBe(hash(body));
  expect(rows.records[0]?.get("format")).toBe("rfc8785-v1");
});

test("unmarked legacy digests retain insertion ordering without migration", async () => {
  const base = input();
  const properties = { z: 1, a: 2 };
  const stored = await engine.remember({ ...base, properties });
  const digest = hash(JSON.stringify({ schema: "anamnesis.original-message/1", content: base.content,
    properties, time: base.time, payload_hash: null, previous_revision_key: null }));
  await driver.executeQuery("MATCH (e:Element {id:$id}) SET e.digest=$digest REMOVE e.digest_format", { id: stored.id, digest });
  expect(await engine.remember({ ...base, properties })).toEqual({ id: stored.id, created: false });
  await expect(engine.remember({ ...base, properties: { a: 2, z: 1 } })).rejects.toThrow("revision_conflict");
  expect((await engine.verify()).filter((issue) => issue.elementId === stored.id)).toEqual([]);
  const row = await driver.executeQuery("MATCH (e:Element {id:$id}) RETURN e.digest_format AS format", { id: stored.id });
  expect(row.records[0]?.get("format")).toBeNull();
});

test("unsupported digest formats fail closed for retry and verification", async () => {
  const base = input();
  const stored = await engine.remember(base);
  await driver.executeQuery("MATCH (e:Element {id:$id}) SET e.digest_format='unknown'", { id: stored.id });
  try {
    await expect(engine.remember(base)).rejects.toThrow("unsupported_digest_format");
    expect(await engine.verify()).toContainEqual({ elementId: stored.id, kind: "unsupported-digest-format" });
  } finally {
    await driver.executeQuery("MATCH (e:Element {id:$id}) SET e.digest_format='rfc8785-v1'", { id: stored.id });
  }
});

test("stale predecessor fails atomically while a pinned exact retry remains idempotent", async () => {
  const base = input();
  const firstInput = { ...base, source_revision: "one", expected_previous_revision_key: null };
  const first = await engine.remember(firstInput);
  const secondInput = { ...base, source_revision: "two", expected_previous_revision_key: await revision(first.id) };
  const second = await engine.remember(secondInput);
  const before = await engine.status();
  await expect(engine.remember({ ...secondInput, source_revision: "three" })).rejects.toThrow("stale_revision");
  expect(await engine.status()).toEqual(before);
  expect(await engine.remember(firstInput)).toEqual({ id: first.id, created: false });
  expect(await engine.remember(secondInput)).toEqual({ id: second.id, created: false });
  await expect(engine.remember({ ...secondInput, expected_previous_revision_key: null })).rejects.toThrow("revision_conflict");
});

test("OriginHead uniqueness and concurrent first writers admit exactly one CAS winner", async () => {
  const base = input();
  const results = await Promise.allSettled(["one", "two"].map((source_revision) =>
    engine.remember({ ...base, source_revision, expected_previous_revision_key: null })));
  expect(results.filter((r) => r.status === "fulfilled")).toHaveLength(1);
  const rejected = results.find((r) => r.status === "rejected");
  expect(rejected?.status === "rejected" ? String(rejected.reason) : "missing").toContain("stale_revision");
  const rows = await driver.executeQuery("MATCH (h:OriginHead) WHERE h.origin_key=$key RETURN count(h) AS n",
    { key: hash(JSON.stringify([base.origin.source, base.origin.session, base.origin.actor, base.origin.record])) });
  expect(rows.records[0]?.get("n")).toBe(1);
  const constraints = await driver.executeQuery("SHOW CONSTRAINTS YIELD name RETURN name");
  expect(constraints.records.map((r) => r.get("name"))).toContain("origin_head_key");
});

test("concurrent identical remembers converge on the original ID", async () => {
  const base = input();
  const results = await Promise.all(Array.from({ length: 4 }, () => engine.remember(base)));
  expect(results.filter((r) => r.created)).toHaveLength(1);
  expect(new Set(results.map((r) => r.id)).size).toBe(1);
});

test("backdated middle insertion replaces the bypass with a complete chain", async () => {
  const session = randomUUID();
  const first = await engine.remember(input("first", session, "00"));
  const last = await engine.remember(input("last", session, "20"));
  const middle = await engine.remember(input("middle", session, "10"));
  expect(await edges(session)).toEqual([[first.id, middle.id], [middle.id, last.id]].sort());
});

test("equal event times follow ingest sequence rather than client supplied IDs", async () => {
  const session = randomUUID();
  const first = await engine.store.putElement({ ...input("first", session), schema: "anamnesis.original-message/1", id: "ffffffff-ffff-7fff-bfff-ffffffffffff" });
  const second = await engine.store.putElement({ ...input("second", session), schema: "anamnesis.original-message/1", id: "00000000-0000-7000-8000-000000000000" });
  expect(await edges(session)).toEqual([[first.id, second.id]]);
});

test("concurrent session inserts form one chain ordered by event time then ingest sequence", async () => {
  const session = randomUUID();
  await Promise.all(["30", "00", "20", "10", "20"].map((minute, i) => engine.remember(input(String(i), session, minute))));
  const rows = await driver.executeQuery("MATCH (e:Episode {origin_session:$session}) RETURN e.id AS id ORDER BY e.time_utc,e.ingest_seq", { session });
  const ids = rows.records.map((r) => r.get("id"));
  expect(await edges(session)).toEqual(ids.slice(1).map((id, i) => [ids[i], id]).sort());
});

test("topology verification detects damage and rebuild restores explicit-parent and chronological edges", async () => {
  const session = randomUUID();
  const first = await engine.remember(input("first", session, "00"));
  const second = await engine.remember(input("second", session, "10"));
  const third = await engine.remember({ ...input("third", session, "20"), previous: "first" });
  await driver.executeQuery("MATCH (a:Episode {id:$id})-[l:NEXT_EPISODE]->() DELETE l", { id: first.id });
  expect((await engine.verify()).some((issue) => issue.kind === "topology-mismatch" && [second.id, third.id].includes(issue.elementId))).toBe(true);
  await engine.store.rebuildTopology();
  expect(await edges(session)).toEqual([[first.id, second.id], [first.id, third.id]].sort());
  expect((await engine.verify()).filter((issue) => [first.id, second.id, third.id].includes(issue.elementId))).toEqual([]);
});

test("journal replay preserves the expected predecessor including explicit null", async () => {
  const journal = new EpisodeJournal(join(directory, "journal"));
  const base = input();
  await journal.append({ ...base, expected_previous_revision_key: null });
  const received: RememberInput[] = [];
  await journal.replay({ remember: async (value) => { received.push(value); return { id: "recorded", created: true }; } });
  expect(received[0]).toHaveProperty("expected_previous_revision_key", null);
});

test("canonical admission rejects lone surrogates instead of hashing invalid Unicode", async () => {
  for (const properties of [{ value: "\ud800" }, { "\udfff": true }]) {
    await expect(engine.remember({ ...input(), properties })).rejects.toThrow("invalid_canonical_json");
  }
});

test("chronology crosses Episode schemas and splices before the first event", async () => {
  const session = randomUUID();
  const last = await engine.remember(input("last", session, "20"));
  const first = await engine.remember({ ...input("first", session, "00"), schema: "anamnesis.original-document/1" });
  expect(await edges(session)).toEqual([[first.id, last.id]]);
});

test("rebuild refuses unmarked legacy topology without rewriting originals or links", async () => {
  const session = randomUUID();
  const first = await engine.remember(input("first", session));
  const second = await engine.remember({ ...input("second", session, "10"), previous: "first" });
  await driver.executeQuery("MATCH (e:Episode {id:$id}) REMOVE e.topology_version", { id: second.id });
  try {
    expect(await engine.verify()).toContainEqual({ elementId: second.id, kind: "unsupported-topology-format" });
    await expect(engine.store.rebuildTopology()).rejects.toThrow("unsupported_topology_format");
    expect(await edges(session)).toEqual([[first.id, second.id]]);
  } finally {
    await driver.executeQuery("MATCH (e:Episode {id:$id}) SET e.topology_version=1", { id: second.id });
  }
});
