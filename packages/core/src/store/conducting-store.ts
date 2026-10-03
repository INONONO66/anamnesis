import { isEpisodeSchema } from "@anamnesis/protocol";
import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { Generation, Coverage, type LinkRole } from "@anamnesis/protocol";
import { extractionBodyDigest } from "@anamnesis/protocol";
import { z } from "zod";
import { createHash } from "node:crypto";
import { canonicalJson } from "./digest.ts";
import { type ConductingArcRow, type ConductingArcProbe, type GraphEnvelope, GraphAccessError, type ConductingPartition, type PhysicalConductor, type ConductingArcVerification, ConductingMaintenanceOptions, conductingPartition, arcIdentity, arcTuple } from "./conducting.ts";
import { CONDUCTING_ROLES } from "./schema.ts";
import { receiptTime } from "./receipts.ts";
import { type InstallationContext, type PolicyState, requireInstallation } from "./policy.ts";
import type { StoreCore } from "./core.ts";

export type AuthoritySnapshot = {
  members: { count: number; sha256: string };
  retained_generations: number[];
  coverage: { ingest_seq: number; structure_revision: number; policy_revision: number };
  physical_links: { count: number; sha256: string };
  invalidation_evidence: { count: number; sha256: string };
  source_hashes: { count: number; sha256: string };
};
class AuthoritySnapshotError extends Error {
  constructor(readonly code: "authority_snapshot_unavailable", detail: string = code) { super(`${code}: ${detail}`); }
}
/** Each physical link yields one endpoint row per endpoint; malformed links, duplicate ids and self-links are reported alongside. */
function physicalLinkRows(links: PhysicalConductor[]): { expected: Map<string, ConductingArcRow>; dataIssues: string[]; violations: string[] } {
  const expected = new Map<string,ConductingArcRow>(), dataIssues: string[] = [], violations: string[] = [], ids = new Set<string>();
  for (const link of links) {
    if (!z.uuid().safeParse(link.link_id).success || !z.uuid().safeParse(link.from).success || !z.uuid().safeParse(link.to).success
      || (link.role === "HAS_MEMBER" && link.source_extraction_generation === null)) dataIssues.push(`invalid-physical:${link.link_id}`);
    if (ids.has(link.link_id)) dataIssues.push(`duplicate-link-id:${link.link_id}`);
    ids.add(link.link_id);
    if (link.from === link.to) violations.push(`self-link:${link.link_id}`);
    for (const source of new Set([link.from,link.to])) {
      const row = {source_id:source,peer_id:source===link.from?link.to:link.from,link_id:link.link_id,role:link.role,
        generation:link.generation,source_extraction_generation:link.source_extraction_generation};
      expected.set(arcIdentity(row),row);
    }
  }
  return { expected, dataIssues, violations };
}

export class ConductingStore {
  constructor(private readonly core: StoreCore) {}

  async conductingRevisionTx(tx: ManagedTransaction): Promise<void> {
    await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.conducting_arc_revision=coalesce(m.conducting_arc_revision,0)+1`);
  }
  async invalidateConductingTx(tx: ManagedTransaction, maxItems = 10000): Promise<void> {
    const changed = await tx.run(`MATCH (m:Meta {key:'meta'})
      WHERE m.conducting_arc_ready IS NULL OR m.conducting_arc_ready <> false OR m.conducting_arc_revision IS NULL
      SET m.conducting_arc_ready=false,m.conducting_arc_revision=coalesce(m.conducting_arc_revision,0)+1`);
    const coverage = await tx.run(`MATCH (c:ConductingArcCoverage) WHERE c.state <> 'UNAVAILABLE' OR c.state IS NULL
      WITH c LIMIT $limit SET c.state='UNAVAILABLE'`, {limit:neo4j.int(maxItems)});
    if (!changed.summary.counters.containsUpdates() && coverage.summary.counters.containsUpdates()) await this.conductingRevisionTx(tx);
  }
  async publishConductingTx(tx: ManagedTransaction, partitions: ConductingPartition[]): Promise<void> {
    const changed = await tx.run(`UNWIND $partitions AS p
      MERGE (c:ConductingArcCoverage {stream:p.stream,generation:p.generation})
      WITH c WHERE c.state IS NULL OR c.state <> 'COMPLETE' SET c.state='COMPLETE'`, { partitions });
    const gate = await tx.run(`MATCH (m:Meta {key:'meta'})
      WHERE m.conducting_arc_ready IS NULL OR m.conducting_arc_ready <> true OR m.conducting_arc_revision IS NULL
      SET m.conducting_arc_ready=true`);
    if (changed.summary.counters.containsUpdates() || gate.summary.counters.containsUpdates()) await this.conductingRevisionTx(tx);
  }
  /** Called only for a newly created physical link, inside its writer transaction.
   * Duplicates do not rewrite, repair, or replace either cache or coverage rows. */
  async appendConductingTx(tx: ManagedTransaction, role: LinkRole, id: string): Promise<void> {
    const result = await tx.run<PhysicalConductor>(`MATCH (a)-[l:${role}]->(b) WHERE l.id=$id
      OPTIONAL MATCH (g:Generation {stream:'community',generation:l.generation})
      RETURN a.id AS source_id,b.id AS peer_id,l.id AS link_id,type(l) AS role,l.generation AS generation,
        CASE WHEN type(l)='HAS_MEMBER' THEN coalesce(g.source_extraction_generation,a.source_extraction_generation) ELSE null END AS source_extraction_generation,
        a.id AS from,b.id AS to,g.source_extraction_generation AS registry_source`, { id });
    const link = result.records[0]!.toObject();
    if (role === "HAS_MEMBER" && link.source_extraction_generation === null) throw new Error("conducting_source_generation_missing");
    // Per-role seeks also reject cross-role identity reuse (even disjoint endpoints).
    for (const other of CONDUCTING_ROLES) {
      if (other === role) continue;
      const collision = await tx.run(`MATCH ()-[l:${other}]->() WHERE l.id=$id RETURN l.id LIMIT 1`, { id });
      if (collision.records.length) throw new Error("conducting_link_id_collision");
    }
    const partition = conductingPartition(role, link.generation);
    const coverage = await tx.run(`MATCH (c:ConductingArcCoverage {stream:$stream,generation:$generation}) RETURN c.state AS state`, partition);
    if (!coverage.records.length) {
      // A missing partition is empty only if this is its first retained link.
      // Never infer that fact from the serving selector, policy or the ready bit.
      const peers = await tx.run(`MATCH ()-[l]->() WHERE type(l) IN $roles
        AND coalesce(l.generation,0)=$generation AND l.id <> $id RETURN l.id LIMIT 1`, {
        roles: partition.stream === "cache" ? ["NEXT_EPISODE"] : partition.stream === "community" ? ["HAS_MEMBER"] : ["MENTIONS","RELATES_TO","DERIVED_FROM"],
        generation: partition.generation, id,
      });
      await tx.run(`CREATE (:ConductingArcCoverage {stream:$stream,generation:$generation,state:$state})`, {
        ...partition, state: peers.records.length ? "UNAVAILABLE" : "COMPLETE",
      });
      if (peers.records.length) await this.invalidateConductingTx(tx);
    } else if (coverage.records[0]!.get("state") !== "COMPLETE") {
      await this.invalidateConductingTx(tx);
    }
    const rows = [...new Set([link.from,link.to])].map(source => ({ source_id:source,peer_id:source === link.from ? link.to : link.from,
      link_id:link.link_id,role:link.role,generation:link.generation,source_extraction_generation:link.source_extraction_generation }));
    await tx.run(`UNWIND $rows AS row CREATE (a:ConductingArc) SET a=row`, { rows });
    await this.conductingRevisionTx(tx);
  }
  /** The only supported physical deletion today is topology replacement.
   * Future generation deletion/GC must use the same endpoint + HubArc removal
   * transaction and retire coverage only after its last physical/cache row. */
  async deleteTopologyLinksTx(tx: ManagedTransaction, ids: string[]): Promise<void> {
    if (!ids.length) return;
    await tx.run(`UNWIND $ids AS id MATCH (a:ConductingArc {link_id:id}) DELETE a`, { ids });
    await tx.run(`UNWIND $ids AS id MATCH (a:HubArc {link_id:id}) DELETE a`, { ids });
    await tx.run(`UNWIND $ids AS id MATCH ()-[l:NEXT_EPISODE]->() WHERE l.id=id DELETE l`, { ids });
    await this.conductingRevisionTx(tx);
  }
  /** Maintenance/startup scan: each retained input collection is capped before
   * collection, not the total scan work (filters may inspect unrelated rows).
   * No serving request calls this physical scan. */
  async conductingSnapshotTx(tx: ManagedTransaction, maxItems: number) {
    const result = await tx.run(`
      CALL () { MATCH (a)-[l]->(b) WHERE type(l) IN $roles
        WITH a,l,b LIMIT $limit
        OPTIONAL MATCH (g:Generation {stream:'community',generation:l.generation})
        RETURN collect({source_id:a.id,peer_id:b.id,from:a.id,to:b.id,link_id:l.id,role:type(l),generation:l.generation,
          source_extraction_generation:CASE WHEN type(l)='HAS_MEMBER' THEN coalesce(g.source_extraction_generation,a.source_extraction_generation) ELSE null END,
          registry_source:g.source_extraction_generation}) AS links }
      CALL () { MATCH (a:ConductingArc) WITH a LIMIT $limit
        RETURN collect(a { .source_id,.peer_id,.link_id,.role,.generation,.source_extraction_generation }) AS arcs }
      CALL () { MATCH (c:ConductingArcCoverage) WITH c LIMIT $limit RETURN collect(c { .stream,.generation,.state }) AS coverage }
      CALL () { MATCH (g:Generation) WHERE g.stream IN ['extraction','community'] WITH g LIMIT $limit
        RETURN collect(g { .stream,.generation }) AS generations }
      CALL () { MATCH (g:ExtractionGeneration) WITH g LIMIT $limit RETURN collect({stream:'extraction',generation:g.id}) AS extraction }
      MATCH (m:Meta {key:'meta'}) RETURN links,arcs,coverage,generations,extraction,
        m.conducting_arc_ready AS ready,m.conducting_arc_revision AS revision`, { roles: [...CONDUCTING_ROLES], limit:neo4j.int(maxItems+1) });
    const record = result.records[0]!;
    const links = record.get("links") as PhysicalConductor[], arcs = record.get("arcs") as ConductingArcRow[];
    const coverage = record.get("coverage") as ConductingPartition[], generations = record.get("generations") as ConductingPartition[], extraction = record.get("extraction") as ConductingPartition[];
    const truncated = [links,arcs,coverage,generations,extraction].some(rows => rows.length > maxItems);
    const partitions = [...new Map([{stream:"cache",generation:0},...generations,...extraction,...coverage,
      ...links.map(link => conductingPartition(link.role,link.generation))].map(p => [JSON.stringify([p.stream,p.generation]),{stream:p.stream,generation:p.generation}])).values()];
    const { expected, dataIssues, violations } = physicalLinkRows(links);
    const actual = new Map(arcs.map(row => [arcIdentity(row),row]));
    for (const [key,row] of expected) {
      if (!actual.has(key)) dataIssues.push(`missing-row:${key}`);
      else if (arcTuple(actual.get(key)!) !== arcTuple(row)) dataIssues.push(`mismatched-row:${key}`);
    }
    for (const row of arcs) if (!expected.has(arcIdentity(row))) dataIssues.push(`stale-row:${arcIdentity(row)}`);
    if (actual.size !== arcs.length) dataIssues.push("duplicate-endpoint-identity");
    const issues = [...dataIssues,...violations];
    for (const partition of partitions) if (!coverage.some(c => c.stream===partition.stream && c.generation===partition.generation && c.state==="COMPLETE")) {
      issues.push(`coverage-incomplete:${JSON.stringify(partition)}`);
    }
    if (truncated) issues.push("maintenance_limit_exceeded");
    const report: ConductingArcVerification = {ready:record.get("ready")===true,revision:record.get("revision"),truncated,
      physical_links:links.length,endpoint_rows:arcs.length,partitions:coverage,issues};
    return {report,partitions,expected:[...expected.values()],dataIssues};
  }
  /** Read-only diagnostic scan. It never repairs data or certifies publication;
   * use checkConductingArcs to fence detection and persist an unavailable gate. */
  async verifyConductingArcs(options: { maxItems?: number } = {}): Promise<ConductingArcVerification> {
    const {maxItems} = ConductingMaintenanceOptions.parse(options);
    return this.core.withReadTx(async tx => (await this.conductingSnapshotTx(tx,maxItems)).report);
  }
  async checkConductingArcs(options: { maxItems?: number } = {}): Promise<ConductingArcVerification> {
    const {maxItems} = ConductingMaintenanceOptions.parse(options);
    if (this.core.writerEpoch === undefined) throw new Error("writer_epoch_required");
    return this.core.withWriteTx(async tx => {
      const snapshot = await this.conductingSnapshotTx(tx,maxItems);
      if (snapshot.report.issues.length) await this.invalidateConductingTx(tx);
      return (await this.conductingSnapshotTx(tx,maxItems)).report;
    });
  }
  /** Atomic, collection-capped operational rebuild; total scan work is not bounded.
   * UNAVAILABLE commits first: interruption, overflow and invalid authority leave
   * PPR closed. A subsequent explicit call can resume by reconstructing afresh. */
  async rebuildConductingArcs(options: { maxItems?: number } = {}): Promise<ConductingArcVerification> {
    const {maxItems} = ConductingMaintenanceOptions.parse(options);
    if (this.core.writerEpoch === undefined) throw new Error("writer_epoch_required");
    await this.core.withWriteTx(tx => this.invalidateConductingTx(tx,maxItems));
    return this.core.withWriteTx(async tx => {
      const snapshot = await this.conductingSnapshotTx(tx,maxItems);
      if (snapshot.report.truncated || snapshot.expected.length > maxItems) throw new Error("maintenance_limit_exceeded");
      if (snapshot.dataIssues.some(issue => issue.startsWith("invalid-physical:") || issue.startsWith("duplicate-link-id:"))) throw new Error("conducting_physical_invalid");
      await tx.run(`MATCH (a:ConductingArc) DELETE a`);
      await tx.run(`UNWIND $rows AS row CREATE (a:ConductingArc) SET a=row`, {rows:snapshot.expected});
      const verified = await this.conductingSnapshotTx(tx,maxItems);
      if (verified.dataIssues.length || verified.report.truncated) throw new Error("conducting_rebuild_mismatch");
      await this.publishConductingTx(tx,verified.partitions);
      return (await this.conductingSnapshotTx(tx,maxItems)).report;
    });
  }
  /** Hash canonical JSON arrays in ID order, in 5000-row pages under one fence.
   * Counts include every row, so missing/duplicate identities cannot be skipped. */
  async authoritySnapshot(context?: InstallationContext): Promise<AuthoritySnapshot> {
    requireInstallation(context!);
    if (this.core.writerEpoch === undefined) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "writer_epoch_required");
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      const result = await tx.run(`
        MATCH (m:Meta {key:'meta'})
        CALL () { MATCH (g:Generation) WHERE g.stream IN ['extraction','community']
          WITH DISTINCT g.generation AS generation ORDER BY generation RETURN collect(generation) AS generations }
        CALL () { MATCH (e:Element) RETURN count(e) AS members }
        CALL () { MATCH ()-[l]->() WHERE type(l) IN $roles RETURN count(l) AS links }
        CALL () { MATCH (:Element)-[l:INVALIDATES]->(:Element) RETURN count(l) AS invalidation }
        CALL () { MATCH (e:Element:Episode) RETURN count(e) AS sources }
        RETURN generations,m.ingest_seq AS ingest_seq,coalesce(m.structure_revision,0) AS structure_revision,
          members,links,invalidation,sources`, { roles: [...CONDUCTING_ROLES] });
      const row = result.records[0]; if (!row) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "snapshot query returned no record");
      const generations = z.array(z.number().int().nonnegative()).parse(row.get("generations"));
      const digest = async (query: string, schema: z.ZodType<Parameters<typeof canonicalJson>[0]>, collection: string, detail: string) => {
        const hash = createHash("sha256").update("[");
        let count = 0, after = "";
        for (;;) {
          const page = await tx.run<{ id: unknown; value: unknown }>(query, { after, batch: neo4j.int(5000) });
          for (const record of page.records) {
            const id = record.get("id"), value = schema.safeParse(record.get("value"));
            if (typeof id !== "string" || id <= after || !value.success) throw new AuthoritySnapshotError("authority_snapshot_unavailable", detail);
            if (count) hash.update(",");
            hash.update(canonicalJson(value.data));
            count++; after = id;
          }
          if (page.records.length < 5000) break;
        }
        if (count !== Number(row.get(collection))) throw new AuthoritySnapshotError("authority_snapshot_unavailable", detail);
        return { count, sha256: hash.update("]").digest("hex") };
      };
      const members = await digest(`MATCH (e:Element) WHERE e.id > $after
        RETURN e.id AS id,e.id AS value ORDER BY e.id LIMIT $batch`, z.string().min(1), "members", "member identity inventory is incomplete");
      if (!members.count) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "member identity inventory is incomplete");
      // Each role uses its ID index before the bounded global merge, rather than
      // scanning all relationship types again for every page.
      const links = await digest(`CALL () { ${CONDUCTING_ROLES.map(role => `
        MATCH (a)-[l:${role}]->(b) WHERE l.id > $after
        RETURN l.id AS id,{id:l.id,from:a.id,to:b.id,role:'${role === "DERIVED_FROM" ? "DERIVED_FROM" : "ConductingArc"}'} AS value
        ORDER BY id LIMIT $batch`).join(" UNION ALL ")} }
        RETURN id,value ORDER BY id LIMIT $batch`,
      z.object({ id: z.string().min(1), from: z.string().min(1), to: z.string().min(1), role: z.enum(["DERIVED_FROM", "ConductingArc"]) }),
      "links", "physical link evidence is incomplete");
      const sources = await digest(`MATCH (e:Element) WHERE e.id > $after AND e:Episode
        RETURN e.id AS id,e.digest AS value ORDER BY e.id LIMIT $batch`,
      z.string().regex(/^[0-9a-f]{64}$/), "sources", "source hash evidence is incomplete");
      const evidence = z.object({ id: z.string().min(1), source_hash: z.string().regex(/^[0-9a-f]{64}$/),
        from: z.string().min(1), to: z.string().min(1), target_id: z.string().min(1), effective_time_utc: z.iso.datetime(),
        generation: z.union([z.string().min(1), z.number().int().nonnegative()]).nullable(),
      }).refine(v => v.target_id === v.to).transform(({ source_hash, ...outcome }) => ({ id: outcome.id, source_hash, outcome_hash: extractionBodyDigest(outcome) }));
      const invalidation = await digest(`MATCH (a:Element)-[l:INVALIDATES]->(b:Element) WHERE l.id > $after
        RETURN l.id AS id,{id:l.id,source_hash:a.digest,from:a.id,to:b.id,target_id:l.target_id,effective_time_utc:l.effective_time_utc,generation:l.generation} AS value
        ORDER BY l.id LIMIT $batch`, evidence, "invalidation", "invalidation hash evidence is incomplete");
      return { members, retained_generations: generations, coverage: { ingest_seq: Number(row.get("ingest_seq")), structure_revision: Number(row.get("structure_revision")), policy_revision: policy.policy_revision }, physical_links: links, invalidation_evidence: invalidation, source_hashes: sources };
    }).catch(async (error: unknown) => {
      // withWriteTx rejects a vanished Meta before entering the callback.
      if (error instanceof Error && error.message === "stale_writer_epoch") {
        const present = await this.core.withReadTx(tx => tx.run(`MATCH (m:Meta {key:'meta'}) RETURN m.key`));
        if (!present.records.length) throw new AuthoritySnapshotError("authority_snapshot_unavailable", "snapshot query returned no record");
      }
      throw error;
    });
  }
  /** Bounded physical ConductingArc probe. Raw rows are ordered and capped before
   * any serving predicate; unavailable coverage is never treated as degree zero. */
  async probeConductingArcs(sourceId: string, options: { limit?: number } = {}, context?: InstallationContext): Promise<ConductingArcProbe> {
    requireInstallation(context!);
    z.strictObject({ limit: z.literal(256).optional() }).parse(options);
    const source = z.uuidv7().parse(sourceId);
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      await this.graphPinTx(tx, policy, this.core.clock());
      await this.core.authorizeEpisodesTx(tx, [source], policy);
      const rows = await this.graphRawProbeTx(tx, source);
      return { source_id: source, count: rows.length, saturated: rows.length === 256, coverage: "complete" };
    });
  }
  private async graphPinTx(tx: ManagedTransaction, policy: PolicyState, T: number): Promise<GraphEnvelope["pin"]> {
    const gate = await tx.run(`MATCH (m:Meta {key:'meta'}) OPTIONAL MATCH (s:Meta {key:'extraction_selector'})
      RETURN m.conducting_arc_ready AS ready,m.conducting_arc_revision AS revision,s.generation_id AS generation`);
    const row = gate.records[0];
    if (row?.get("ready") !== true || !receiptTime.safeParse(row.get("revision")).success) throw new GraphAccessError("degree_probe_unavailable");
    const generationId = z.uuidv7().safeParse(row.get("generation"));
    if (!generationId.success) throw new GraphAccessError("degree_probe_unavailable");
    const generation = await this.core.extractionRecordTx(tx, "ExtractionGeneration", generationId.data, Generation);
    const coverage = await tx.run(`UNWIND ['episodes','active_extraction'] AS partition
      MATCH (c:ExtractionCoverage {key:$id+':'+partition}) RETURN partition,c.body AS body`, { id: generation.id });
    if (generation.state !== "active" || coverage.records.length !== 2) throw new GraphAccessError("degree_probe_unavailable");
    const values = coverage.records.map(record => {
      const value = Coverage.parse(JSON.parse(record.get("body")));
      if (value.generation_id !== generation.id || value.partition !== record.get("partition") || value.covered_ingest_seq < value.required_ingest_seq || value.covered_ingest_seq < generation.covered_ingest_seq) throw new GraphAccessError("degree_probe_unavailable");
      return value;
    });
    return { policy_revision: policy.policy_revision, generation_id: generation.id, coverage_revision: row.get("revision"),
      covered_ingest_seq: Math.min(...values.map(value => value.covered_ingest_seq)), T };
  }
  private async graphRawProbeTx(tx: ManagedTransaction, source: string): Promise<ConductingArcRow[]> {
    // Neo4j 5.26 requires the full composite order to stream this unique seek;
    // source equality makes it equivalent to link-ID order. Force SEEK so small
    // mixed stores cannot select an ordered full-index scan. Reject Top/Sort.
    const query = `MATCH (arc:ConductingArc {source_id:$source}) USING INDEX SEEK arc:ConductingArc(source_id,link_id)
      WHERE arc.link_id > '' RETURN arc.source_id AS source_id,arc.link_id AS link_id,arc.peer_id AS peer_id,
      arc.role AS role,arc.generation AS generation,arc.source_extraction_generation AS source_extraction_generation
      ORDER BY arc.source_id ASC, arc.link_id ASC LIMIT 256`;
    const planned = await tx.run(`EXPLAIN ${query}`, { source });
    const operators: Exclude<typeof planned.summary.plan, false>[] = [];
    const visit = (plan: typeof planned.summary.plan) => { if (plan) { operators.push(plan); for (const child of plan.children ?? []) visit(child); } };
    visit(planned.summary.plan);
    const limit = operators.find(p => /^Limit(?:@|$)/.test(p.operatorType));
    const seek = limit?.children?.[0];
    if (limit?.arguments["Details"] !== "256" || limit.children?.length !== 1
      || !seek || !/^NodeUniqueIndexSeek(?:@|$)/.test(seek.operatorType)
      || seek.arguments["Order"] !== "arc.source_id ASC, arc.link_id ASC"
      || operators.some(p => /Top|Sort|Scan|Expand/.test(p.operatorType))) throw new GraphAccessError("ordered_probe_unavailable");
    const rows = await tx.run<ConductingArcRow>(query, { source });
    return rows.records.map(record => record.toObject());
  }
  /** An Episode element at or before T that no unrevoked deny selects by episode id or origin source. */
  private async episodeAllowedTx(tx: ManagedTransaction, id: string, policy: PolicyState, T: number): Promise<boolean> {
    const result = await tx.run(`MATCH (e:Element {id:$id}) RETURN e.schema AS schema,e.time_utc AS time,e.origin_source AS source`, { id });
    const e = result.records[0];
    // Derived source/witness authority is not yet materialized by this
    // runtime. Unknown/derived nodes must not inherit Episode permission.
    if (!e || !isEpisodeSchema(e.get("schema")) || typeof e.get("time") !== "string" || !(Date.parse(e.get("time")) <= T)) return false;
    return ![...policy.denies.values()].some(deny => !policy.revoked.has(deny.policy_id)
      && (deny.selector.episode_id === undefined || deny.selector.episode_id === id)
      && (deny.selector.source === undefined || deny.selector.source === e.get("source")));
  }
  /** The endpoint row still describes exactly one ungenerated physical NEXT_EPISODE link between source and peer. */
  private async nextEpisodeLinkIntactTx(tx: ManagedTransaction, source: string, row: ConductingArcRow): Promise<boolean> {
    const physical = await tx.run(`MATCH (a)-[l:NEXT_EPISODE]->(b) USING INDEX l:NEXT_EPISODE(id)
      WHERE l.id=$id RETURN a.id AS a,b.id AS b,l.generation AS generation,l.source_extraction_generation AS source_generation`, { id: row.link_id });
    const link = physical.records[0];
    return physical.records.length === 1 && link !== undefined && link.get("generation") === null && link.get("source_generation") === null
      && ((link.get("a") === source && link.get("b") === row.peer_id) || (link.get("b") === source && link.get("a") === row.peer_id));
  }
  async graphEnvelope(seedIds: string[], options: { T?: number; maxNodes?: number; maxArcs?: number } = {}, context?: InstallationContext): Promise<GraphEnvelope> {
    requireInstallation(context!);
    const seeds = [...new Set(z.array(z.uuidv7()).min(1).max(128).parse(seedIds))].sort();
    const request = z.strictObject({ T: receiptTime.optional(), maxNodes: z.number().int().min(1).max(2000).optional(), maxArcs: z.number().int().min(0).max(20000).optional() }).parse(options);
    const maxNodes = request.maxNodes ?? 2000, maxArcs = request.maxArcs ?? 20000, T = request.T ?? this.core.clock();
    return this.core.withWriteTx(async tx => {
      // Same Meta fence as policy/generation writes: no torn policy or selector.
      const policy = await this.core.receiptLockTx(tx), pin = await this.graphPinTx(tx, policy, T);
      const probes: ConductingArcProbe[] = [], eligible: ConductingArcRow[] = [], ids = new Set<string>();
      let staleTopology = false;
      for (const source of seeds) {
        if (!await this.episodeAllowedTx(tx, source, policy, T)) continue;
        ids.add(source);
        const rows = await this.graphRawProbeTx(tx, source);
        probes.push({ source_id: source, count: rows.length, saturated: rows.length === 256, coverage: "complete" });
        // A saturated raw probe cannot stand in for a qualified hub shortlist.
        if (rows.length === 256) continue;
        for (const row of rows) {
          if (row.role !== "NEXT_EPISODE") continue;
          if (row.generation !== null || row.source_extraction_generation !== null || !await this.nextEpisodeLinkIntactTx(tx, source, row)) { staleTopology = true; continue; }
          if (!await this.episodeAllowedTx(tx, row.peer_id, policy, T)) continue;
          ids.add(row.peer_id); eligible.push(row);
        }
      }
      if (staleTopology) {
        // The current attempt retains its verified subset without refill. Close
        // the next attempt under the same fence, using only the affected key.
        await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.conducting_arc_ready=false
          MERGE (c:ConductingArcCoverage {stream:'cache',generation:0}) SET c.state='UNAVAILABLE'`);
        await this.conductingRevisionTx(tx);
      }
      const orderedNodes = [...ids].sort(), nodes = orderedNodes.slice(0,maxNodes), retained = new Set(nodes);
      const orderedArcs = eligible.filter(row => retained.has(row.source_id) && retained.has(row.peer_id));
      const arcs = orderedArcs.slice(0,maxArcs);
      return { nodes, arcs, probes, pin, truncated: orderedNodes.length > maxNodes || orderedArcs.length > maxArcs || probes.some(probe => probe.saturated),
        overflow: { nodes: Math.max(0,orderedNodes.length-maxNodes), arcs: Math.max(0,orderedArcs.length-maxArcs), saturated_sources: probes.filter(probe => probe.saturated).length } };
    });
  }
}
