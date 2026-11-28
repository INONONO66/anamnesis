import { afterAll, beforeAll, expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { RpcEpisodeRebuildParams, type MemoryElement } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";

const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("Isolated test database credentials required");
const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
const root = await mkdtemp(join(tmpdir(), "episode-derived-"));
const engine = new Engine({ uri, password, objectsRoot: root });
const context = { principal: "installation", client_binding: uuidv7() } as const;
const run = async (query: string, params: Record<string, unknown> = {}) =>
  (await driver.executeQuery(query, params)).records.map(record => record.toObject());
const labels = async (id: string) => (await run("MATCH (e:Episode {id:$id}) RETURN e.room AS room,e.space AS space", { id }))[0];
const original = async (id: string) => (await run("MATCH (e:Episode {id:$id}) RETURN e{.*,room:null,space:null} AS original", { id }))[0]?.["original"];
const remember = (source: string, room: string, record: string, properties: MemoryElement["properties"] = {}, session = room) =>
  engine.remember({
    content: `message ${record}`, time: { value: "2026-09-01T00:00:00Z", precision: "second" },
    origin: { source, session, actor: "user", record }, properties,
  }, { metadata: { origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }, context });
const rebuild = (kind: "labels" | "replies", room?: { source: string; room: string }, after = 0, limit = 1000) =>
  engine.store.rebuildEpisodeDerived(RpcEpisodeRebuildParams.parse({ after, limit, room }), kind);

beforeAll(async () => { await engine.init(); await engine.claimWriterEpoch(); }, 120000);
afterAll(async () => { await engine.close(); await driver.close(); await rm(root, { recursive: true }); });

test("pre-label Episodes gain labels and replies without changing originals or conduction", async () => {
  const room = randomUUID();
  const parent = await remember("slack", room, "parent", { room });
  const reply = await remember("slack", room, "reply", { room, thread: "parent" }, `${room}/parent`);
  // A retained pre-D58 graph: originals and lineage exist, but neither cache does.
  await run("MATCH (e:Episode) WHERE e.id IN $ids REMOVE e.room,e.space", { ids: [parent.id, reply.id] });
  await run("MATCH (:Episode {id:$id})-[r:REPLIES_TO]->() DELETE r", { id: reply.id });
  const before = await original(reply.id), authority = await engine.store.authoritySnapshot(context);
  const conduction = await engine.store.verifyConductingArcs();
  await engine.store.importRoomAssignments({ assignments: [{ source: "slack", room, space: "project:bori" }] });

  expect((await rebuild("labels")).updated).toBe(2);
  expect((await rebuild("replies")).created).toBe(1);

  expect(await labels(reply.id)).toEqual({ room, space: "project:bori" });
  expect((await engine.store.linksOf(reply.id, "REPLIES_TO")).map(link => [link.from, link.to])).toEqual([[reply.id, parent.id]]);
  expect(await original(reply.id)).toEqual(before);
  expect(await engine.store.authoritySnapshot(context)).toEqual(authority);
  expect(await engine.store.verifyConductingArcs()).toEqual(conduction);
  expect(await engine.verify()).toEqual([]);
  expect(await rebuild("labels")).toMatchObject({ updated: 0 });
  expect(await rebuild("replies")).toMatchObject({ created: 0, removed: 0, missing: 0 });
}, 120000);

test("room reassignment rebuilds only that room and includes Slack thread sessions", async () => {
  const room = randomUUID(), other = randomUUID(), space = `project:${room}`;
  await engine.store.importRoomAssignments({ assignments: [{ source: "slack", room, space, name: "Before" }] });
  const a = await remember("slack", room, "one", { room });
  const b = await remember("slack", room, "two", { room }, `${room}/thread`);
  const untouched = await remember("slack", other, "three", { room: other });
  await engine.store.importRoomAssignments({ assignments: [{ source: "slack", room, space: space + "-new" }] });
  // A different room remains deliberately stale; a global scan would repair it.
  await run("MATCH (e:Episode {id:$id}) SET e.space='sentinel:untouched'", { id: untouched.id });

  const first = await rebuild("labels", { source: "slack", room }, 0, 1);
  const resumed = await engine.store.rebuildEpisodeDerived({ after: first.after, through: first.through, limit: 1, room: { source: "slack", room } }, "labels");
  expect([first.scanned, first.updated, resumed.scanned, resumed.updated]).toEqual([1, 1, 1, 1]);
  expect(await labels(a.id)).toEqual({ room, space: space + "-new" });
  expect(await labels(b.id)).toEqual({ room, space: space + "-new" });
  expect(await labels(untouched.id)).toEqual({ room: other, space: "sentinel:untouched" });
  const listed = await engine.store.listSpaces({ after: space, limit: 100 });
  expect(listed.spaces.find(row => row.space === space + "-new")).toEqual({
    space: space + "-new", episodes: 2, rooms: 1, first: "2026-09-01T00:00:00.000Z", last: "2026-09-01T00:00:00.000Z",
  });
  expect(await run("MATCH (r:RoomAssignment {source:'slack',room:$room}) RETURN r.name AS name", { room })).toEqual([{ name: null }]);
}, 120000);

test.each(["kakao", "discord"])("%s replies resolve at admission and repair late targets idempotently", async source => {
  const room = randomUUID();
  const parent = await remember(source, room, "parent", { room });
  const reply = await remember(source, room, "reply", { room, reply: `${source}:${room}#parent` });
  expect((await engine.store.linksOf(reply.id, "REPLIES_TO")).map(link => link.to)).toEqual([parent.id]);
  const late = await remember(source, room, "late-reply", { room, reply: `${source}:${room}#late-parent` });
  await remember(source, room, "missing-reply", { room, reply: `${source}:${room}#absent` });
  expect(await rebuild("replies", { source, room })).toMatchObject({ missing: 2, created: 0 });
  const arrived = await remember(source, room, "late-parent", { room });
  const before = await original(late.id);

  expect(await rebuild("replies", { source, room })).toMatchObject({ missing: 1, created: 1, removed: 0 });
  const links = await engine.store.linksOf(late.id, "REPLIES_TO");
  expect(links.map(link => link.to)).toEqual([arrived.id]);
  expect(await rebuild("replies", { source, room })).toMatchObject({ missing: 1, created: 0, removed: 0 });
  expect(await engine.store.linksOf(late.id, "REPLIES_TO")).toEqual(links);
  expect(await original(late.id)).toEqual(before);
}, 120000);

test("fallback room and unknown space use the source namespace, not cwd", async () => {
  const room = randomUUID();
  const a = await remember("omo", room, "one", { cwd: "/project/bori" });
  const b = await remember("codex", room, "two", {});
  await engine.store.importRoomAssignments({ assignments: [{ source: "omo", room, space: "scratch:omo" }] });
  expect(await rebuild("labels", { source: "omo", room })).toMatchObject({ scanned: 1, updated: 1 });
  expect(await labels(a.id)).toEqual({ room, space: "scratch:omo" });
  expect(await labels(b.id)).toEqual({ room, space: "unknown:codex" });
}, 120000);

test("reply rebuilding selects the newest address revision and replaces only the reply edge", async () => {
  const room = randomUUID(), source = "kakao";
  const parent = await remember(source, room, "parent", { room });
  const reply = await remember(source, room, "reply", { room, reply: `${source}:${room}#parent` });
  const newer = await engine.remember({
    content: "edited parent", time: { value: "2026-09-01T00:00:00Z", precision: "second" },
    origin: { source, session: room, actor: "user", record: "parent" }, properties: { room }, source_revision: "v2",
  });
  const topology = await run("MATCH (a:Episode {origin_session:$room})-[r:NEXT_EPISODE]->(b) RETURN a.id AS a,b.id AS b,r.id AS id ORDER BY id", { room });
  expect(await rebuild("replies", { source, room })).toMatchObject({ created: 1, removed: 1, missing: 0 });
  expect((await engine.store.linksOf(reply.id, "REPLIES_TO")).map(link => link.to)).toEqual([newer.id]);
  expect(await engine.store.linksOf(parent.id, "REPLIES_TO")).toEqual([]);
  expect(await run("MATCH (a:Episode {origin_session:$room})-[r:NEXT_EPISODE]->(b) RETURN a.id AS a,b.id AS b,r.id AS id ORDER BY id", { room })).toEqual(topology);
}, 120000);
