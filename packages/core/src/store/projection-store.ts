import { type ManagedTransaction } from "neo4j-driver";
import { z } from "zod";
import { RpcProjectionParams, RpcProjectionResult } from "@anamnesis/protocol";
import { type InstallationContext, policySelector, requireInstallation } from "./policy.ts";
import { FACT_VALID } from "./recall-store.ts";
import type { StoreCore } from "./core.ts";
import type { ExtractionStore } from "./extraction-store.ts";

type ProjectionRow = { entity: string; name: string; properties: string | null; fact: string; content: string; value: string | null; precision: string | null; sources: string[] };

/** A Fact projects when it is valid at T known at T, timed no later than T, and no active deny selects any of its source Episodes. */
const PROJECTED = `(f.time_utc IS NULL OR f.time_utc <= $T) AND ${FACT_VALID}
        AND NOT EXISTS { MATCH (f)-[:DERIVED_FROM]->(s:Element:Episode)
          WHERE ANY(d IN $denies WHERE (d.episode_id IS NULL OR d.episode_id=s.id) AND (d.source IS NULL OR d.source=s.origin_source)) }`;

export class ProjectionStore {
  constructor(private readonly core: StoreCore, private readonly extraction: ExtractionStore) {}

  /** Read-only: one read transaction, never the Meta write barrier. */
  async snapshot(input: RpcProjectionParams, context: InstallationContext): Promise<RpcProjectionResult> {
    requireInstallation(context);
    const request = RpcProjectionParams.parse(input), generatedAt = new Date(this.core.clock()).toISOString();
    const T = request.T === undefined ? null : new Date(request.T).toISOString();
    return RpcProjectionResult.parse({ generated_at: generatedAt, T, entities: await this.core.withReadTx(tx => this.snapshotTx(tx, T ?? generatedAt)) });
  }
  private async snapshotTx(tx: ManagedTransaction, T: string): Promise<RpcProjectionResult["entities"]> {
    const selection = await this.extraction.extractionSelectionTx(tx);
    if (!selection.generation_id) return [];
    const policy = await this.core.policyStateTx(tx);
    const denies = [...policy.denies.values()].filter(deny => !policy.revoked.has(deny.policy_id)).map(deny => policySelector(deny.selector));
    const rows = await tx.run<ProjectionRow>(`MATCH (f:Element:Fact {generation:$generation})-[:MENTIONS]->(e:Element:Entity {generation:$generation})
      WHERE ${PROJECTED}
      WITH DISTINCT e,f
      RETURN e.id AS entity,e.content AS name,e.properties AS properties,f.id AS fact,f.content AS content,f.time_value AS value,f.time_precision AS precision,
        [(f)-[:DERIVED_FROM]->(s:Element:Episode) | s.id] AS sources
      ORDER BY name ASC,entity ASC,f.time_utc DESC,fact ASC`,
    { generation: selection.generation_id, T, knownBy: Date.parse(T), denies });
    const entities: RpcProjectionResult["entities"] = [];
    for (const row of rows.records.map(record => record.toObject())) {
      if (entities.at(-1)?.id !== row.entity) {
        const kind = z.object({ entity_kind: z.string().optional() }).parse(JSON.parse(row.properties ?? "{}")).entity_kind;
        entities.push({ id: row.entity, name: row.name, ...(kind === undefined ? {} : { kind }), facts: [] });
      }
      entities.at(-1)!.facts.push({ id: row.fact, content: row.content,
        time: row.value === null ? null : { value: row.value, precision: z.enum(["second", "minute", "day", "month", "year"]).parse(row.precision) },
        sources: [...new Set(row.sources)].sort() });
    }
    return entities;
  }
}
