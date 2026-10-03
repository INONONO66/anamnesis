import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { z } from "zod";
import { extractionBodyDigest } from "@anamnesis/protocol";
import { Engine } from "./engine.ts";
import { canonicalJson, sha256 } from "./store/digest.ts";
import { CONDUCTING_ROLES } from "./store/schema.ts";
import type { ConductingArcRow } from "./store/conducting.ts";

const context = { principal: "installation", commit_mode: "receipt" } as const;
const id = (n: number) => `01900000-0000-7000-8000-${n.toString(16).padStart(12, "0")}`;
const T = Date.parse("2027-01-01T00:00:00Z");
const byCodepoint = (x: string, y: string) => (x < y ? -1 : x > y ? 1 : 0);
const digest = (items: Parameters<typeof canonicalJson>[0][]) => ({ count: items.length, sha256: sha256(canonicalJson(items)) });
const Physical = z.object({ a: z.string(), b: z.string(), link_id: z.string(), role: z.enum(CONDUCTING_ROLES), generation: z.number().nullable(), source_extraction_generation: z.number().nullable() });
const Arc = z.object({ source_id: z.string(), link_id: z.string(), peer_id: z.string(), role: z.enum(CONDUCTING_ROLES), generation: z.number().nullable(), source_extraction_generation: z.number().nullable() });
const State = z.object({ ready: z.boolean().nullable(), revision: z.number().nullable(), structure_revision: z.number().nullable() });
const Labelled = z.object({ identity: z.string(), properties: z.record(z.string(), z.union([z.string(), z.number(), z.boolean(), z.null()])) });
const episode = (record: string, day: number) => ({ schema: "anamnesis.original-message/1", content: `maintenance ${record}`,
  origin: { source: "maintenance", session: "ordered", actor: "user", record },
  time: { value: `2026-01-${String(day).padStart(2, "0")}T00:00:00Z`, precision: "second" as const }, mass: 0.5, properties: {} });

async function setup() {
  const parent = join(homedir(), ".cache/anamnesis-qa");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "conducting-maintenance-"));
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner required");
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async <Row>(cypher: string, params: Record<string, unknown>, row: z.ZodType<Row>) =>
    (await driver.executeQuery(cypher, params)).records.map(record => row.parse(record.toObject()));
  const run = async (cypher: string, params: Record<string, unknown> = {}) => { await driver.executeQuery(cypher, params); };
  const options = { uri, password, objectsRoot: root };
  const engine = new Engine(options);
  await run("MATCH (n) DETACH DELETE n");
  await engine.init(); await engine.claimWriterEpoch();
  const physical = () => query(`MATCH (a)-[l]->(b) WHERE type(l) IN $roles RETURN a.id AS a,b.id AS b,l.id AS link_id,type(l) AS role,l.generation AS generation,
    CASE WHEN type(l)="HAS_MEMBER" THEN a.source_extraction_generation ELSE null END AS source_extraction_generation ORDER BY link_id`, { roles: [...CONDUCTING_ROLES] }, Physical);
  const rows = (): Promise<ConductingArcRow[]> => query("MATCH (a:ConductingArc) RETURN a.source_id AS source_id,a.link_id AS link_id,a.peer_id AS peer_id,a.role AS role,a.generation AS generation,a.source_extraction_generation AS source_extraction_generation ORDER BY source_id,link_id", {}, Arc);
  const state = async () => (await query('MATCH (m:Meta {key:"meta"}) RETURN m.conducting_arc_ready AS ready,m.conducting_arc_revision AS revision,m.structure_revision AS structure_revision', {}, State))[0]!;
  const snapshot = async () => ({
    arcs: await query("MATCH (a:ConductingArc) RETURN elementId(a) AS identity,properties(a) AS properties ORDER BY a.source_id,a.link_id", {}, Labelled),
    coverage: await query("MATCH (c:ConductingArcCoverage) RETURN elementId(c) AS identity,properties(c) AS properties ORDER BY c.stream,toString(c.generation)", {}, Labelled),
    state: await state(),
  });
  const expectMirrored = async () => {
    const links = await physical();
    const expected = links.flatMap(l => [...new Set([l.a, l.b])].map(source_id => ({ source_id, link_id: l.link_id, peer_id: source_id === l.a ? l.b : l.a,
      role: l.role, generation: l.generation, source_extraction_generation: l.source_extraction_generation })))
      .sort((x, y) => byCodepoint(x.source_id, y.source_id) || byCodepoint(x.link_id, y.link_id));
    expect(await rows()).toEqual(expected);
    return links;
  };
  const seedGeneration = async () => {
    const generation = { id: id(500), stream: "extraction" as const, incarnation: "a".repeat(64), state: "catching_up" as const, covered_ingest_seq: 0, created_at: 1, updated_at: 1 };
    await engine.store.createExtractionGeneration(generation, context);
    for (const partition of ["episodes", "active_extraction"] as const) await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 0 }, context);
    return generation;
  };
  return { engine, query, run, physical, rows, state, snapshot, expectMirrored, seedGeneration, options, async close() { await engine.close(); await driver.close(); await rm(root, { recursive: true, force: true }); } };
}

test("ConductingArc rows mirror every physical link through remember, rewire, putLink and topology rebuild", async () => {
  const f = await setup();
  try {
    expect((await f.state()).ready).toBe(true);
    const generation = await f.seedGeneration();
    expect((await f.snapshot()).coverage.map(row => row.properties)).toContainEqual({ stream: "extraction", generation: generation.id, state: "COMPLETE" });
    await f.run('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=false');
    await expect(f.engine.store.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: null, expected_selector_version: 0 }, context)).rejects.toThrow("degree_probe_unavailable");
    await f.engine.init();
    expect((await f.state()).ready).toBe(true);
    const a = await f.engine.remember(episode("a", 1)), c = await f.engine.remember(episode("c", 3));
    const [first] = await f.expectMirrored();
    await f.run("CREATE (:HubArc {hub_id:$source,link_id:$link,rank:0})", { source: a.id, link: first!.link_id });
    await f.engine.remember(episode("b", 2));
    await f.expectMirrored();
    expect(await f.query("MATCH (h:HubArc {link_id:$id}) RETURN count(h) AS n", { id: first!.link_id }, z.object({ n: z.number() }))).toEqual([{ n: 0 }]);
    await f.run(`CREATE (:Element:Fact {id:$f,generation:42}),(:Element:Entity {id:$e,generation:42}),(:Element:Entity {id:$e2,generation:42}),
      (:Element:Community {id:$c,generation:7,source_extraction_generation:42}),(:Element:Community {id:$orphan,generation:8}),
      (:Generation {stream:"community",generation:7,source_extraction_generation:42,state:"RETIRED"}),(:Generation {stream:"extraction",generation:42,state:"BUILDING"})`,
      { f: id(10), e: id(11), e2: id(12), c: id(13), orphan: id(15) });
    for (const [n, role, from, to] of [[101, "MENTIONS", a.id, id(11)], [102, "RELATES_TO", id(11), id(12)], [103, "HAS_MEMBER", id(13), id(10)], [104, "DERIVED_FROM", id(10), a.id]] as const) {
      await f.engine.store.putLink({ id: id(n), from, to, role, content: `physical ${role}` });
      await f.expectMirrored();
    }
    await expect(f.engine.store.putLink({ id: id(105), from: id(15), to: id(11), role: "HAS_MEMBER", content: "no source generation" })).rejects.toThrow("conducting_source_generation_missing");
    const links = await f.physical();
    expect(links.find(l => l.role === "HAS_MEMBER")).toMatchObject({ generation: 7, source_extraction_generation: 42 });
    expect(links.find(l => l.role === "MENTIONS")).toMatchObject({ generation: 42, source_extraction_generation: null });
    await f.engine.store.rebuildTopology();
    await f.expectMirrored();
    const before = await f.snapshot();
    expect((await f.engine.remember(episode("b", 2))).created).toBe(false);
    expect((await f.engine.store.putLink({ id: id(201), from: a.id, to: id(11), role: "MENTIONS", content: "physical MENTIONS" })).id).toBe(id(101));
    expect(await f.snapshot()).toEqual(before);
    await f.engine.store.putLink({ id: id(202), from: id(11), to: id(12), role: "RELATES_TO", content: "parallel retained physical relation" });
    await f.engine.store.putLink({ id: id(203), from: c.id, to: a.id, role: "INVALIDATES", content: "nonconducting" });
    await f.expectMirrored();
    await expect(f.engine.store.putLink({ id: id(205), from: id(10), to: id(10), role: "DERIVED_FROM", content: "self" })).rejects.toThrow("self-link is not allowed");
    await expect(f.engine.store.putLink({ id: id(101), from: id(11), to: id(12), role: "RELATES_TO", content: "cross-role collision" })).rejects.toThrow("conducting_link_id_collision");
    expect(await f.engine.verifyConductingArcs()).toMatchObject({ issues: [], ready: true, physical_links: 7, endpoint_rows: 14 });
    const authority = await f.engine.store.authoritySnapshot(context);
    const members = (await f.query("MATCH (e:Element) RETURN e.id AS id ORDER BY e.id", {}, z.object({ id: z.string() }))).map(row => row.id);
    expect(authority.members).toEqual(digest(members));
    expect(authority.members.count).toBe(8);
    const authorityLinks = (await f.physical()).map(link => ({ id: link.link_id, from: link.a, to: link.b,
      role: link.role === "DERIVED_FROM" ? "DERIVED_FROM" : "ConductingArc" }));
    expect(authority.physical_links).toEqual(digest(authorityLinks));
    expect(authority.physical_links.count).toBe(7);
    const invalidation = await f.query(`MATCH (a:Element)-[l:INVALIDATES]->(b:Element)
      RETURN l.id AS id,a.digest AS source_hash,a.id AS from,b.id AS to,l.target_id AS target_id,
        l.effective_time_utc AS effective_time_utc,l.generation AS generation ORDER BY l.id`, {},
      z.object({ id: z.string(), source_hash: z.string(), from: z.string(), to: z.string(), target_id: z.string(),
        effective_time_utc: z.string(), generation: z.union([z.string(), z.number()]).nullable() }));
    expect(authority.invalidation_evidence).toEqual(digest(invalidation.map(({ source_hash, ...outcome }) =>
      ({ id: outcome.id, source_hash, outcome_hash: extractionBodyDigest(outcome) }))));
    expect(authority.invalidation_evidence.count).toBe(1);
    const sources = (await f.query("MATCH (e:Element:Episode) RETURN e.digest AS digest ORDER BY e.id", {},
      z.object({ digest: z.string() }))).map(row => row.digest);
    expect(authority.source_hashes).toEqual(digest(sources));
    expect(authority.source_hashes.count).toBe(3);
    expect(authority.coverage).toMatchObject({ ingest_seq: 3, policy_revision: 0 });
    await expect(f.engine.store.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: null, expected_selector_version: 0 }, context)).rejects.toMatchObject({ code: "coverage_incomplete" });
  } finally { await f.close(); }
}, 120000);

test("envelope probes detect stale rows and fence; verify is read-only; rebuild is bounded, idempotent and refuses invalid authority", async () => {
  const f = await setup();
  try {
    const generation = await f.seedGeneration();
    const a = await f.engine.remember(episode("a", 1)), b = await f.engine.remember(episode("b", 2)), c = await f.engine.remember(episode("c", 3));
    await f.run("CREATE (:Element:Entity {id:$e,generation:42}),(:Element:Entity {id:$e2,generation:42})", { e: id(11), e2: id(12) });
    await f.engine.store.putLink({ id: id(101), from: a.id, to: id(11), role: "MENTIONS", content: "physical MENTIONS" });
    await expect(f.engine.graphEnvelope([b.id], {}, context)).rejects.toThrow("degree_probe_unavailable");
    await f.run('MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:"extraction_selector"}) SET g.state="active",g.body=$body,s.generation_id=$id,s.selector_version=1', { id: generation.id, body: JSON.stringify({ ...generation, state: "active" }) });
    const probe = await f.engine.store.probeConductingArcs(b.id, {}, context);
    expect(probe).toStrictEqual({ source_id: b.id, count: 2, saturated: false, coverage: "complete" });
    const envelope = await f.engine.graphEnvelope([b.id, id(11)], {}, context);
    expect(envelope.nodes).toEqual([a.id, b.id, c.id].sort());
    expect(envelope.arcs.map(row => row.role)).toEqual(["NEXT_EPISODE", "NEXT_EPISODE"]);
    expect(envelope.probes).toEqual([probe]);
    expect(envelope.truncated).toBe(false);
    expect(envelope.pin).toMatchObject({ generation_id: generation.id, covered_ingest_seq: 0 });
    const bounded = await f.engine.graphEnvelope([b.id], { maxNodes: 1, maxArcs: 0, T }, context);
    expect(bounded).toMatchObject({ nodes: [envelope.nodes[0]], arcs: [], truncated: true, overflow: { nodes: 2, arcs: 0, saturated_sources: 0 } });
    const arcBounded = await f.engine.graphEnvelope([b.id], { maxArcs: 1 }, context);
    expect(arcBounded).toMatchObject({ nodes: envelope.nodes, arcs: [envelope.arcs[0]], truncated: true, overflow: { nodes: 0, arcs: 1, saturated_sources: 0 } });
    await f.engine.setPolicy({ policy_id: id(902), scope: "content", selector: { episode_id: c.id } }, context);
    expect((await f.engine.graphEnvelope([b.id, c.id], {}, context)).nodes).toEqual([a.id, b.id].sort());
    const raw = (await f.rows()).find(row => row.source_id === b.id && row.peer_id === c.id)!;
    await f.run("MATCH (a:ConductingArc {source_id:$source,link_id:$link}) SET a.peer_id=$bad", { source: b.id, link: raw.link_id, bad: id(999) });
    const excluded = await f.engine.graphEnvelope([b.id], {}, context);
    expect(excluded.nodes).toEqual([a.id, b.id].sort());
    expect((await f.state()).ready).toBe(false);
    await expect(f.engine.graphEnvelope([b.id], {}, context)).rejects.toThrow("degree_probe_unavailable");
    await expect(f.engine.store.probeConductingArcs(b.id, {}, context)).rejects.toThrow("degree_probe_unavailable");
    expect((await f.engine.rebuildConductingArcs()).ready).toBe(true);
    await f.expectMirrored();
    const physicalBefore = await f.physical(), stateBefore = await f.state();
    expect(await f.engine.rebuildConductingArcs()).toMatchObject({ issues: [], ready: true });
    expect(await f.physical()).toEqual(physicalBefore);
    expect(await f.state()).toEqual({ ...stateBefore, revision: stateBefore.revision! + 2 });
    await f.run("MATCH (a:ConductingArc {link_id:$id}) SET a.peer_id=$bad", { id: id(101), bad: id(999) });
    const corruptSnapshot = await f.snapshot();
    const corrupt = await f.engine.verifyConductingArcs();
    expect(corrupt.issues.some(issue => issue.startsWith("mismatched-row:"))).toBe(true);
    expect(await f.snapshot()).toEqual(corruptSnapshot);
    expect((await f.engine.checkConductingArcs()).ready).toBe(false);
    await expect(f.engine.rebuildConductingArcs({ maxItems: 1 })).rejects.toThrow("maintenance_limit_exceeded");
    expect((await f.state()).ready).toBe(false);
    expect((await f.snapshot()).arcs).toEqual(corruptSnapshot.arcs);
    await f.engine.rebuildConductingArcs();
    await f.expectMirrored();
    await f.run('MATCH (c:ConductingArcCoverage {stream:"extraction",generation:42}) SET c.state="INVALID"');
    const hidden = await f.engine.verifyConductingArcs();
    expect(hidden.issues).toEqual(['coverage-incomplete:{"stream":"extraction","generation":42}']);
    expect((await f.engine.checkConductingArcs()).ready).toBe(false);
    expect((await f.engine.rebuildConductingArcs()).issues).toEqual([]);
    await f.run("MATCH (a:Element {id:$a}),(b:Element {id:$b}) CREATE (a)-[:RELATES_TO {id:\"invalid\",generation:42}]->(b)", { a: id(11), b: id(12) });
    const invalidRows = (await f.snapshot()).arcs;
    await expect(f.engine.rebuildConductingArcs()).rejects.toThrow("conducting_physical_invalid");
    expect((await f.state()).ready).toBe(false);
    expect((await f.snapshot()).arcs).toEqual(invalidRows);
    expect((await f.engine.verifyConductingArcs()).issues).toContain("invalid-physical:invalid");
    await f.run('MATCH ()-[l:RELATES_TO {id:"invalid"}]->() DELETE l');
    await f.engine.rebuildConductingArcs();
    await f.run("MATCH (a:ConductingArc) DELETE a"); await f.run("MATCH (c:ConductingArcCoverage) DELETE c");
    await f.engine.init();
    expect((await f.state()).ready).toBe(false);
    expect(await f.rows()).toEqual([]);
    await f.engine.remember(episode("d", 4));
    expect((await f.state()).ready).toBe(false);
    expect((await f.engine.verifyConductingArcs()).issues.some(issue => issue.startsWith("missing-row:"))).toBe(true);
    await f.engine.rebuildConductingArcs();
    await f.expectMirrored();
    await f.run("MATCH (a:Element {id:$source}) CREATE (a)-[:RELATES_TO {id:$id,generation:42}]->(a)", { source: id(11), id: id(206) });
    const self = await f.engine.rebuildConductingArcs();
    expect(self.issues).toContain(`self-link:${id(206)}`);
    expect((await f.rows()).filter(row => row.link_id === id(206))).toHaveLength(1);
    await f.run("MATCH ()-[l:RELATES_TO {id:$id}]->() DELETE l", { id: id(206) });
    await f.engine.rebuildConductingArcs();
    const contender = new Engine(f.options);
    try {
      await contender.claimWriterEpoch();
      const fenced = await f.snapshot();
      await expect(f.engine.rebuildConductingArcs()).rejects.toThrow("stale_writer_epoch");
      await expect(f.engine.checkConductingArcs()).rejects.toThrow("stale_writer_epoch");
      await expect(f.engine.store.authoritySnapshot(context)).rejects.toThrow("stale_writer_epoch");
      expect(await f.snapshot()).toEqual(fenced);
    } finally { await contender.close(); }
    const fresh = new Engine(f.options);
    try {
      await expect(fresh.rebuildConductingArcs()).rejects.toThrow("writer_epoch_required");
      await expect(fresh.checkConductingArcs()).rejects.toThrow("writer_epoch_required");
      await expect(fresh.store.authoritySnapshot(context)).rejects.toThrow("writer_epoch_required");
    } finally { await fresh.close(); }
  } finally { await f.close(); }
}, 180000);
