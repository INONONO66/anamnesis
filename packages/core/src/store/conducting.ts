import { LINK_LATTICE, type LinkRole } from "@anamnesis/protocol";
import { z } from "zod";
import { sessionKey } from "./digest.ts";

export type ConductingArcRow = {
  source_id: string; link_id: string; peer_id: string; role: string;
  generation: number | null; source_extraction_generation: number | null;
};
export type ConductingArcProbe = { source_id: string; count: number; saturated: boolean; coverage: "complete" };

export type GraphEnvelope = { nodes: string[]; arcs: ConductingArcRow[]; truncated: boolean; probes: ConductingArcProbe[];
  pin: { policy_revision: number; generation_id: string; coverage_revision: number; covered_ingest_seq: number; T: number };
  overflow: { nodes: number; arcs: number; saturated_sources: number } };
export class GraphAccessError extends Error {
  readonly code: "degree_probe_unavailable" | "ordered_probe_unavailable";
  constructor(readonly reason: "degree_probe_unavailable" | "ordered_probe_unavailable") { super(reason); this.code = reason; this.name = "GraphAccessError"; }
}


export const LINK_ROLES = Object.keys(LINK_LATTICE) as LinkRole[];
export const CONDUCTING_ROLES = ["NEXT_EPISODE", "MENTIONS", "RELATES_TO", "HAS_MEMBER", "DERIVED_FROM"] as const;
export type ConductingPartition = { stream: string; generation: number | string; state?: string };
export type PhysicalConductor = ConductingArcRow & { from: string; to: string; registry_source: number | null };
export type ConductingArcVerification = {
  ready: boolean; revision: number | null; truncated: boolean; physical_links: number; endpoint_rows: number;
  partitions: ConductingPartition[]; issues: string[];
};
export const ConductingMaintenanceOptions = z.strictObject({ maxItems: z.number().int().min(1).max(50000).default(10000) });
export function conductingPartition(role: string, generation: number | null): ConductingPartition {
  return { stream: role === "NEXT_EPISODE" ? "cache" : role === "HAS_MEMBER" ? "community" : "extraction", generation: generation ?? 0 };
}
export function arcIdentity(row: ConductingArcRow): string { return JSON.stringify([row.source_id, row.link_id]); }
export function arcTuple(row: ConductingArcRow): string {
  return JSON.stringify([row.source_id, row.link_id, row.peer_id, row.role, row.generation, row.source_extraction_generation]);
}

/** Transient embedding failures keep an outbox entry queued this many times before it is quarantined as exhausted (#219). */

export interface TopologyRow {
  id: string;
  sessionKey: string;
  record: string;
  previousRecord: string | null;
  timeUtc: string;
  ingestSeq: number;
  version: number | null;
  actual: ({ from: string; key: string | null } | null)[];
}

export const TOPOLOGY_QUERY = `MATCH (e:Element:Episode)
  OPTIONAL MATCH (p)-[l:NEXT_EPISODE]->(e)
  RETURN e.id AS id, e.session_key AS sessionKey, e.origin_record AS record,
    e.topology_previous_record AS previousRecord, e.ingest_seq AS ingestSeq,
    e.topology_version AS version, e.time_utc AS timeUtc,
    collect(CASE WHEN l IS NULL THEN null ELSE {from: p.id, key: l.idem_key} END) AS actual
  ORDER BY sessionKey, timeUtc, ingestSeq`;

/** Derive expectations independently of the cache, retaining explicit-parent
 * semantics as observed at admission (later source revisions are not parents).
 */
export function topologyExpectations(rows: TopologyRow[]): (TopologyRow & { parents: string[] })[] {
  const records = new Map<string, TopologyRow[]>();
  for (const row of rows) {
    const key = JSON.stringify([row.sessionKey, row.record]);
    const bucket = records.get(key) ?? [];
    bucket.push(row);
    records.set(key, bucket);
  }
  const previousBySession = new Map<string, string>();
  return rows.map((row) => {
    const previous = previousBySession.get(row.sessionKey);
    const parents = row.previousRecord === null
      ? previous === undefined ? [] : [previous]
      : (records.get(JSON.stringify([row.sessionKey, row.previousRecord])) ?? [])
        .filter((parent) => parent.ingestSeq < row.ingestSeq).map((parent) => parent.id);
    previousBySession.set(row.sessionKey, row.id);
    return { ...row, parents };
  });
}
