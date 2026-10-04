import { LINK_LATTICE, SCHEMA_LABELS, TIME_BEARING, type Celestial, type KnownSchema, type LinkRole } from "@anamnesis/protocol";

const LINK_ROLES = Object.keys(LINK_LATTICE) as LinkRole[];
export const CONDUCTING_ROLES = ["NEXT_EPISODE", "MENTIONS", "RELATES_TO", "HAS_MEMBER", "DERIVED_FROM"] as const;

export const SCHEMA_STATEMENTS = [
  `CREATE CONSTRAINT embedding_vector_key IF NOT EXISTS FOR (v:EmbeddingVector) REQUIRE v.key IS UNIQUE`,
  `CREATE INDEX embedding_vector_episode_profile IF NOT EXISTS FOR (v:EmbeddingVector) ON (v.episode_id, v.profile_id)`,
  `CREATE CONSTRAINT embedding_profile_id IF NOT EXISTS FOR (p:EmbeddingProfile) REQUIRE p.id IS UNIQUE`,
  `CREATE CONSTRAINT policy_authority_key IF NOT EXISTS FOR (p:PolicyAuthority) REQUIRE p.key IS UNIQUE`,
  `CREATE CONSTRAINT policy_event_revision IF NOT EXISTS FOR (p:PolicyEvent) REQUIRE p.revision IS UNIQUE`,
  `CREATE CONSTRAINT policy_event_key IF NOT EXISTS FOR (p:PolicyEvent) REQUIRE p.key IS UNIQUE`,
  `CREATE CONSTRAINT recall_receipt_id IF NOT EXISTS FOR (r:RecallReceipt) REQUIRE r.recall_id IS UNIQUE`,
  `CREATE CONSTRAINT recall_transport_id IF NOT EXISTS FOR (r:RecallTransport) REQUIRE r.recall_id IS UNIQUE`,
  `CREATE CONSTRAINT receipt_operation_id IF NOT EXISTS FOR (r:RecallFeedback) REQUIRE r.operation_id IS UNIQUE`,
  `CREATE CONSTRAINT recall_outcome_id IF NOT EXISTS FOR (r:RecallOutcome) REQUIRE r.recall_id IS UNIQUE`,
  `CREATE CONSTRAINT hit_id IF NOT EXISTS FOR (h:Hit) REQUIRE h.id IS UNIQUE`,
  `CREATE CONSTRAINT hit_idem_key IF NOT EXISTS FOR (h:Hit) REQUIRE h.idem_key IS UNIQUE`,
  `CREATE CONSTRAINT hit_cache_episode IF NOT EXISTS FOR (c:HitCache) REQUIRE c.episode_id IS UNIQUE`,
  `CREATE INDEX receipt_expiry IF NOT EXISTS FOR (r:RecallReceipt) ON (r.expires_at)`,
  `CREATE INDEX hit_replay IF NOT EXISTS FOR (h:Hit) ON (h.episode_id, h.t, h.id)`,
  `CREATE CONSTRAINT conducting_arc_source_link IF NOT EXISTS FOR (a:ConductingArc) REQUIRE (a.source_id, a.link_id) IS UNIQUE`,
  `CREATE CONSTRAINT graph_next_episode_id IF NOT EXISTS FOR ()-[l:NEXT_EPISODE]-() REQUIRE l.id IS UNIQUE`,
  ...CONDUCTING_ROLES.filter(role => role !== "NEXT_EPISODE").map(role =>
    `CREATE CONSTRAINT graph_${role.toLowerCase()}_id IF NOT EXISTS FOR ()-[l:${role}]-() REQUIRE l.id IS UNIQUE`),
  `CREATE CONSTRAINT conducting_arc_coverage IF NOT EXISTS FOR (c:ConductingArcCoverage) REQUIRE (c.stream,c.generation) IS UNIQUE`,
  `CREATE INDEX conducting_arc_link IF NOT EXISTS FOR (a:ConductingArc) ON (a.link_id)`,
  `CREATE INDEX hub_arc_link IF NOT EXISTS FOR (a:HubArc) ON (a.link_id)`,
  `CREATE CONSTRAINT conducting_generation_partition IF NOT EXISTS FOR (g:Generation) REQUIRE (g.stream,g.generation) IS UNIQUE`,

  `CREATE CONSTRAINT element_id IF NOT EXISTS
   FOR (e:Element) REQUIRE e.id IS UNIQUE`,

  ...LINK_ROLES.map(
    (role) => `CREATE CONSTRAINT link_idem_${role.toLowerCase()} IF NOT EXISTS
   FOR ()-[l:${role}]-() REQUIRE l.idem_key IS UNIQUE`,
  ),
  `CREATE CONSTRAINT episode_revision IF NOT EXISTS
   FOR (e:Episode) REQUIRE e.revision_key IS UNIQUE`,
  `CREATE CONSTRAINT episode_ingest_seq IF NOT EXISTS
   FOR (e:Episode) REQUIRE e.ingest_seq IS UNIQUE`,
  `CREATE CONSTRAINT payload_hash IF NOT EXISTS
   FOR (p:Payload) REQUIRE p.hash IS UNIQUE`,
  // Without it, remembers racing on a cold database each MERGE their own Meta
  // node and hand out the same ingest_seq.
  `CREATE CONSTRAINT meta_key IF NOT EXISTS
   FOR (m:Meta) REQUIRE m.key IS UNIQUE`,
  `CREATE INDEX episode_origin IF NOT EXISTS
   FOR (e:Episode) ON (e.origin_key)`,
  `CREATE INDEX episode_origin_head IF NOT EXISTS
   FOR (e:Episode) ON (e.origin_key, e.ingest_seq)`,
  `CREATE INDEX episode_session_order IF NOT EXISTS
   FOR (e:Episode) ON (e.session_key, e.time_utc, e.ingest_seq)`,
  `CREATE INDEX element_time IF NOT EXISTS
   FOR (e:Element) ON (e.time_utc)`,
  `CREATE INDEX element_schema IF NOT EXISTS
   FOR (e:Element) ON (e.schema)`,
  // valid(T) seeks invalidators by target instead of expanding adjacency.
  `CREATE CONSTRAINT extraction_generation_id IF NOT EXISTS FOR (g:ExtractionGeneration) REQUIRE g.id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_input_id IF NOT EXISTS FOR (a:AdjudicationInput) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_attempt_id IF NOT EXISTS FOR (a:AdjudicationAttempt) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_proposal_id IF NOT EXISTS FOR (a:AdjudicationProposal) REQUIRE a.id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_review_id IF NOT EXISTS FOR (a:AdjudicationReview) REQUIRE a.review_id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_review_proposal IF NOT EXISTS FOR (a:AdjudicationReview) REQUIRE a.proposal_id IS UNIQUE`,
  `CREATE CONSTRAINT adjudication_consumption_id IF NOT EXISTS FOR (a:AdjudicationConsumption) REQUIRE a.proposal_id IS UNIQUE`,
  `CREATE INDEX fact_generation_id IF NOT EXISTS FOR (f:Fact) ON (f.generation,f.id)`,
  `CREATE CONSTRAINT fact_identity IF NOT EXISTS FOR (f:Fact) REQUIRE (f.generation,f.meaning_digest,f.primary_episode_id) IS UNIQUE`,
  `CREATE INDEX entity_generation_key IF NOT EXISTS FOR (e:Entity) ON (e.generation,e.entity_key)`,
  `CREATE INDEX entity_witness IF NOT EXISTS FOR (e:Entity) ON (e.witness_generation,e.witness_policy_revision)`,
  `CREATE CONSTRAINT extraction_coverage_key IF NOT EXISTS FOR (c:ExtractionCoverage) REQUIRE c.key IS UNIQUE`,
  `CREATE INDEX invalidates_seek IF NOT EXISTS
   FOR ()-[l:INVALIDATES]-() ON (l.target_id, l.effective_time_utc, l.id)`,
  `CREATE INDEX authority_invalidates_id IF NOT EXISTS
   FOR ()-[l:INVALIDATES]-() ON (l.id)`,

  `CREATE FULLTEXT INDEX element_content IF NOT EXISTS
   FOR (e:Element) ON EACH [e.content]
   OPTIONS { indexConfig: { \`fulltext.analyzer\`: 'cjk' } }`,
];

/** Model-stated claim time becomes an explicit resolved time. Coarse precisions
 * are truncated to the UTC interval start; second/minute stay instants. */

export function celestialOf(schema: string): Celestial | null {
  return SCHEMA_LABELS[schema as KnownSchema] ?? null;
}

/** Only Episode and Fact carry an event time (docs/03 §1). */
export function carriesTime(schema: string): boolean {
  const c = celestialOf(schema);
  return c !== null && TIME_BEARING[c];
}

export function labelClause(schema: string): string {
  const c = celestialOf(schema);
  return c ? `Element:${c}` : "Element";
}

export function toUtc(isoWithOffset: string): string {
  return new Date(isoWithOffset).toISOString();
}
