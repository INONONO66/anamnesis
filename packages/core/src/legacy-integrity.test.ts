import { afterAll, beforeAll, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j, { EagerResult } from "neo4j-driver";
import { EpisodeJournal } from "./journal.ts";
import { Engine, RememberInput } from "./engine.ts";
import { Store } from "./store.ts";

// Reconstructed post167/pre194 append bytes, NOT an operator corpus.
// Source: c8a075f^ schema + #194 regression inputs and #163 fixed clock.
const lines = [
  '{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.original-message/1","content":"Ino prefers dark mode.","origin":{"source":"slack","session":"C0123/2026-08-21","actor":"U098765","record":"1724221402.000300"},"mass":0.5,"properties":{}}}\n',
  '{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.claim/1","content":"Ino prefers dark mode.","origin":{"source":"slack","session":"C0123/2026-08-21","actor":"U098765","record":"1724221402.000300"},"mass":0.5,"properties":{}}}\n',
  '{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.claim/1","time":{"value":"2026-08-21T14:03:22+09:00","precision":"second"},"content":"Ino prefers dark mode.","origin":{"source":"slack","session":"C0123/2026-08-21","actor":"U098765","record":"1724221402.000300"},"mass":0.5,"properties":{"sub_kind":"opinion"}}}\n',
];
const hashes = [
  "49db3f55710c6c97fd63f08fd02cc4cbd05b6087bb76b0f88172603471c2de56",
  "983f4439c13cf3bf8aeb877b73861cf4a3e0335970cffcb512df9810b893542c",
  "2149154d303dd0a84da9714b5acb3ba3bc1f580199c7c8679e3f6c3256ec10de",
];
const digests = [
  "25336ecb71d5a5703763925ee83621b5f014a2c2ea4f6051993018c390b181e1",
  "4d0514a31ade66835f45f840dd0609cf967a1b2a57cb37e30b4808f60c99c31b",
  "56ab9d7ca2f8f00995e99a6efe75fa8b8be2609b137141a630dba94db21085af",
];
const ids = ["0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e0f", "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e10", "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e11"] as const;
const originKey = "324eaac8861d8ce9d127fe7895cfa5c0c8ed20c6c6907ff97d42cfe2e07b6531";
const revisionKey = "3338e8315ea8e487094db0da6b32efa6f9a0619fc4a6318b848f55f9018430bf";
const hash = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("Isolated test database credentials required");
const root = await mkdtemp(join(tmpdir(), "legacy-integrity-"));
const options = { uri, password, user: "neo4j", objectsRoot: join(root, "objects") };
const admin = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const observed = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const summaries: { query: string; updates: boolean; systemUpdates: boolean }[] = [];
const execute = observed.executeQuery.bind(observed);
observed.executeQuery = async function<T = EagerResult>(...args: Parameters<typeof observed.executeQuery<T>>) {
  const result = await execute<T>(...args);
  if (!(result instanceof EagerResult)) throw new Error("Expected real eager query summary");
  summaries.push({ query: String(args[0]), updates: result.summary.counters.containsUpdates(), systemUpdates: result.summary.counters.containsSystemUpdates() });
  return result;
};
const store = new Store(options, observed);
const engine = new Engine(options);

async function snapshot() {
  const nodes = await admin.executeQuery("MATCH (n) RETURN elementId(n) AS key, labels(n) AS labels, properties(n) AS props ORDER BY key");
  const edges = await admin.executeQuery("MATCH (a)-[r]->(b) RETURN elementId(r) AS key,elementId(a) AS a,elementId(b) AS b,type(r) AS type,properties(r) AS props ORDER BY key");
  return { nodes: nodes.records.map(r => r.toObject()), edges: edges.records.map(r => r.toObject()) };
}
async function verifyReadOnly() {
  const before = await snapshot();
  summaries.length = 0;
  const issues = await store.verify();
  expect(summaries.length).toBeGreaterThan(0);
  expect(summaries.every(s => !s.updates && !s.systemUpdates)).toBe(true);
  expect(await snapshot()).toEqual(before);
  console.log(JSON.stringify({ verification: "read-only", summaries, issues }));
  return issues;
}
async function journalFor(bytes: Buffer) {
  const directory = await mkdtemp(join(root, "journal-"));
  const file = join(directory, "journal-2026-09.jsonl");
  await writeFile(file, bytes);
  return { journal: new EpisodeJournal(directory), file };
}

beforeAll(async () => {
  await engine.init();
  for (const [i, line] of lines.entries()) {
    const element = JSON.parse(line).element;
    await admin.executeQuery(`CREATE (e:Element {id:$id,schema:$schema,content:$content,mass:$mass,properties:$properties,
      origin_source:$source,origin_session:$session,origin_actor:$actor,origin_record:$record,
      time_value:$time,time_precision:$precision,digest:$digest})`, {
      id: ids[i], schema: element.schema, content: element.content, mass: element.mass,
      properties: JSON.stringify(element.properties), ...element.origin,
      time: element.time?.value ?? null, precision: element.time?.precision ?? null, digest: digests[i],
    });
  }
  await admin.executeQuery(`MATCH (e:Element {id:$id}) SET e:Episode, e.origin_key=$origin,e.revision_key=$revision,e.ingest_seq=1
    CREATE (h:OriginHead {origin_key:$origin,revision_key:$revision})
    CREATE (o:Outbox {element_id:$id,enqueued_at:'historical-fixture'})-[:OF]->(e)
    MERGE (m:Meta {key:'meta'}) SET m.ingest_seq=1`, { id: ids[0], origin: originKey, revision: revisionKey });
  await admin.executeQuery("MATCH (e:Element) WHERE e.id IN $ids SET e:Fact", { ids: ids.slice(1) });
});
afterAll(async () => {
  try { await engine.close(); } finally {
    try { await store.close(); } finally {
      try { await admin.close(); } finally { await rm(root, { recursive: true }); }
    }
  }
});

test("explicit historical inspection preserves exact buffers, offsets and eligibility without replay", async () => {
  const bytes = Buffer.from(lines.join(""));
  const { journal, file } = await journalFor(bytes);
  const before = await snapshot();
  const rows = await journal.inspect("post167-pre194");
  expect(rows).toHaveLength(3);
  let offset = 0;
  for (const [i, row] of rows.entries()) {
    expect(row.file).toBe("journal-2026-09.jsonl");
    expect(row.offset).toBe(offset);
    expect(row.raw).toEqual(Buffer.from(lines[i]!));
    expect(row.sha256).toBe(hashes[i]!);
    expect(hash(row.raw)).toBe(hashes[i]!);
    expect(row.entry).toEqual(JSON.parse(lines[i]!));
    expect(row.eligibility).toEqual([i === 2 ? "invalid-sub-kind" : "missing-time"]);
    offset += row.raw.length;
  }
  expect(Buffer.concat(rows.map(row => row.raw))).toEqual(bytes);
  expect(await readFile(file)).toEqual(bytes);
  expect(await snapshot()).toEqual(before);
});

test("unmarked raw integrity is distinct from modern semantic eligibility and never writes", async () => {
  const issues = await verifyReadOnly();
  for (const [i, id] of ids.entries()) {
    expect(issues).toContainEqual({ elementId: id, kind: "semantic-ineligibility", reasons: [i === 2 ? "invalid-sub-kind" : "missing-time"] });
    expect(issues.some(issue => issue.elementId === id && issue.kind === "digest-mismatch")).toBe(false);
  }
  expect(issues).toContainEqual({ elementId: ids[0], kind: "unsupported-topology-format" });
});

test("historical acceptance never weakens new append, remember or put", async () => {
  const directory = join(root, "strict-not-created");
  const journal = new EpisodeJournal(directory);
  const before = await snapshot();
  for (const [i, line] of lines.entries()) {
    const input = JSON.parse(line).element;
    expect(RememberInput.safeParse(input).success).toBe(false);
    await expect(journal.append(input)).rejects.toThrow();
    await expect(engine.remember(input)).rejects.toThrow();
    await expect(engine.put({ ...input, id: ids[i] })).rejects.toThrow();
  }
  expect((await readdir(root)).includes("strict-not-created")).toBe(false);
  expect(await snapshot()).toEqual(before);
});

test("inspection rejects unsupported provenance and malformed bytes without changing them", async () => {
  for (const format of ["", "pre167", "unknown", "rfc8785-v1"]) {
    await expect(new EpisodeJournal(join(root, "absent")).inspect(format)).rejects.toThrow("unsupported-legacy-format");
  }
  const entry = JSON.parse(lines[0]!);
  const { mass, ...noMass } = entry.element;
  const { properties, ...noProperties } = entry.element;
  const invalid = [
    Buffer.from("{broken}\n"), Buffer.from(lines[0]!.trimEnd()), Buffer.from([0xff, 0x0a]),
    ...[
      { ...entry, unknown: true }, { ...entry, element: { ...entry.element, unknown: true } },
      { ...entry, element: noMass }, { ...entry, element: noProperties },
      ...[[-1], [256], [1.5], "AA=="].map(payload => ({ ...entry, element: { ...entry.element, payload } })),
      { ...entry, element: { ...entry.element, expected_previous_revision_key: null } },
    ].map(value => Buffer.from(JSON.stringify(value) + "\n")),
  ];
  for (const bytes of invalid) {
    const { journal, file } = await journalFor(bytes);
    await expect(journal.inspect("post167-pre194")).rejects.toThrow();
    expect(await readFile(file)).toEqual(bytes);
  }
  const bytes = Buffer.from(JSON.stringify({ ...entry, element: { ...entry.element, payload: [0, 127, 255], properties: { z: 1, a: 2 } } }) + "\n");
  const { journal, file } = await journalFor(bytes);
  const rows = await journal.inspect("post167-pre194");
  expect(rows[0]?.entry.element.payload).toEqual([0, 127, 255]);
  expect(Object.keys(rows[0]!.entry.element.properties)).toEqual(["z", "a"]);
  expect(rows[0]?.raw).toEqual(bytes);
  expect(await readFile(file)).toEqual(bytes);
});

test("changed raw content mismatches, malformed storage and unknown formats fail closed", async () => {
  for (const change of [
    { field: "content", value: "changed bytes", kind: "digest-mismatch", original: "Ino prefers dark mode." },
    { field: "properties", value: "{broken}", kind: "malformed-element", original: "{}" },
    { field: "digest_format", value: "UNKNOWN", kind: "unsupported-digest-format", original: null },
    { field: "payload_hash", value: "../../outside", kind: "malformed-element", original: null },
  ] as const) {
    await admin.executeQuery(`MATCH (e:Element {id:$id}) SET e.${change.field}=$value`, { id: ids[0], value: change.value });
    try { expect(await verifyReadOnly()).toContainEqual({ elementId: ids[0], kind: change.kind }); }
    finally { await admin.executeQuery(`MATCH (e:Element {id:$id}) SET e.${change.field}=$value`, { id: ids[0], value: change.original }); }
  }
});

test("payload integrity reports missing and mismatching bytes without repair", async () => {
  const bytes = Buffer.from([0, 127, 255]);
  const payloadHash = hash(bytes);
  const path = join(options.objectsRoot, payloadHash.slice(0, 2), payloadHash);
  const body = JSON.stringify({ schema: "anamnesis.original-message/1", content: "Ino prefers dark mode.", properties: {}, time: null, payload_hash: payloadHash, previous_revision_key: null });
  await admin.executeQuery("MATCH (e:Element {id:$id}) SET e.payload_hash=$hash,e.digest=$digest", { id: ids[0], hash: payloadHash, digest: hash(body) });
  await admin.executeQuery("CREATE (:Payload {hash:$hash})", { hash: payloadHash });
  try {
    expect(await verifyReadOnly()).toContainEqual({ elementId: ids[0], kind: "missing-payload" });
    await mkdir(join(options.objectsRoot, payloadHash.slice(0, 2)), { recursive: true });
    await writeFile(path, bytes);
    expect((await verifyReadOnly()).filter(i => i.elementId === ids[0] && i.kind.includes("payload"))).toEqual([]);
    expect(await readFile(path)).toEqual(bytes);
    expect((await readdir(join(options.objectsRoot, payloadHash.slice(0, 2)))).includes(`${payloadHash}.json`)).toBe(false);
    await writeFile(path, "damaged");
    expect(await verifyReadOnly()).toContainEqual({ elementId: ids[0], kind: "payload-hash-mismatch" });
    expect(await readFile(path, "utf8")).toBe("damaged");
  } finally {
    await admin.executeQuery("MATCH (p:Payload {hash:$hash}) DELETE p", { hash: payloadHash });
    await rm(path, { force: true });
    await admin.executeQuery("MATCH (e:Element {id:$id}) REMOVE e.payload_hash SET e.digest=$digest", { id: ids[0], digest: digests[0] });
  }
});

test("canonical format remains canonical and valid-time legacy retries preserve original identity", async () => {
  const input = { schema: "anamnesis.original-message/1", content: "valid legacy", time: { value: "2026-09-02T10:00:00Z", precision: "second" as const }, origin: { source: "fixture", session: "valid", actor: "user", record: "r" }, properties: { z: 1, a: 2 } };
  const saved = await engine.remember(input);
  expect((await verifyReadOnly()).filter(i => i.elementId === saved.id)).toEqual([]);
  const digest = hash(JSON.stringify({ schema: input.schema, content: input.content, properties: input.properties, time: input.time, payload_hash: null, previous_revision_key: null }));
  await admin.executeQuery("MATCH (e:Element {id:$id}) SET e.digest=$digest REMOVE e.digest_format", { id: saved.id, digest });
  const before = await snapshot();
  expect(await engine.remember(input)).toEqual({ id: saved.id, created: false });
  expect(await snapshot()).toEqual(before);
  await expect(engine.remember({ ...input, properties: { a: 2, z: 1 } })).rejects.toThrow("revision_conflict");
  expect((await verifyReadOnly()).filter(i => i.elementId === saved.id)).toEqual([]);
});
