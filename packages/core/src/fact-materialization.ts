import { createHash } from "node:crypto";

export type RetainedClaim = { text: string; start?: number; end?: number; time: string };
export type RetainedExtractionAttempt = {
  id: string; episodeId: string; generation: number; policyRevision: number;
  state: "succeeded" | "failed"; gate: "independent_shadow_review_required"; claims: RetainedClaim[];
};
export type RetainedEpisode = { id: string; content: string; generation: number; policyRevision: number };
export type MaterializedFact = {
  id: string; schema: "anamnesis.claim/1"; content: string; time: { value: string; precision: "second" };
  generation: number; policyRevision: number; source_episode_ids: string[]; primary_episode_id: string;
  lineage: { episode_id: string; attempt_id: string; span?: [number, number] };
};
export type ConductingArc = { source_id: string; link_id: string; peer_id: string; role: "DERIVED_FROM"; generation: number; policyRevision?: number };
export type PhysicalConductingArcLookup = (sourceId: string, linkId: string) => ConductingArc | undefined;

const id = (value: string) => createHash("sha256").update(value).digest("hex").slice(0, 32);

export function materializeFacts(
  attempt: RetainedExtractionAttempt, episode: RetainedEpisode,
  options: {
    semanticWrites: boolean; independentShadowReview?: boolean;
    conductingArc?: ConductingArc; conductingArcs?: readonly ConductingArc[];
    conductingArcLookup?: PhysicalConductingArcLookup;
  },
): { state: "blocked"; reason: string; facts: never[] } | { state: "materialized"; facts: MaterializedFact[] } {
  if (!options.semanticWrites || !options.independentShadowReview) {
    return { state: "blocked", reason: "independent_shadow_review_required", facts: [] };
  }
  if (attempt.state !== "succeeded" || attempt.generation !== episode.generation || attempt.policyRevision !== episode.policyRevision || attempt.episodeId !== episode.id) {
    return { state: "blocked", reason: "stale_evidence", facts: [] };
  }
  const facts = attempt.claims.map((claim, index) => ({
    id: id(`${attempt.id}:${index}:${claim.text}:${claim.time}`), schema: "anamnesis.claim/1" as const,
    content: claim.text, time: { value: claim.time, precision: "second" as const }, generation: attempt.generation, policyRevision: attempt.policyRevision,
    source_episode_ids: [episode.id], primary_episode_id: episode.id,
    lineage: { episode_id: episode.id, attempt_id: attempt.id, ...(claim.start !== undefined && claim.end !== undefined ? { span: [claim.start, claim.end] as [number, number] } : {}) },
  }));
  const arcs = options.conductingArcs ?? (options.conductingArc ? [options.conductingArc] : []);
  if (!arcs.length || !options.conductingArcLookup) return { state: "blocked", reason: "conducting_arc_unavailable", facts: [] };
  const valid = facts.every((fact) => {
    const supplied = arcs.find(arc => arc.link_id && arc.source_id === fact.id);
    if (!supplied) return false;
    const persisted = options.conductingArcLookup!(fact.id, supplied.link_id);
    return !!persisted && persisted.source_id === fact.id && persisted.link_id === supplied.link_id
      && persisted.peer_id === episode.id && persisted.role === "DERIVED_FROM"
      && persisted.generation === attempt.generation
      && (persisted.policyRevision === undefined || persisted.policyRevision === episode.policyRevision);
  });
  if (!valid) return { state: "blocked", reason: "conducting_arc_stale", facts: [] };
  return { state: "materialized", facts };
}

export function recallDerived(
  facts: readonly MaterializedFact[], arcs: readonly ConductingArc[],
  options: { generation: number; policyRevision: number; limit: number; conductingArcLookup?: PhysicalConductingArcLookup },
): MaterializedFact[] {
  if (!options.conductingArcLookup) return [];
  const ids = new Set(arcs.filter((arc) => {
    const persisted = options.conductingArcLookup!(arc.source_id, arc.link_id);
    const fact = facts.find(candidate => candidate.id === arc.source_id);
    return !!fact && !!persisted && persisted.source_id === fact.id && persisted.link_id === arc.link_id
      && persisted.peer_id === fact.primary_episode_id && persisted.role === "DERIVED_FROM"
      && persisted.generation === options.generation
      && (persisted.policyRevision === undefined || persisted.policyRevision === options.policyRevision);
  }).map(a => a.source_id));
  return facts.filter(f => f.generation === options.generation && f.policyRevision === options.policyRevision && ids.has(f.id)).slice(0, Math.max(0, Math.min(options.limit, 64)));
}
