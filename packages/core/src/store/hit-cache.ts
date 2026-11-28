import { attributeOutcome } from "../dynamics/ranking.ts";
import { replayDynamics, type DynamicsEvent } from "../dynamics/state.ts";
import { canonicalJson, tupleHash } from "./digest.ts";
import { type HitCacheIssue, type HitCache, OutcomeHit, Usage } from "./receipts.ts";

export type CacheEvidence = {
  episodes: { id: string; mass: number; ingested_at: number }[];
  hits: { props: Record<string, unknown>; targets: string[] }[];
  outcomes: { usage: string; reward: number; t: number; selection: string | null }[];
  caches: { props: HitCache; targets: string[] }[];
};
/** One statement observes ledger, originals and cache together. No write locks or
 * repairs in verification. The writer path already holds the Meta fence lock. */
export const CACHE_EVIDENCE = `
  CALL () { MATCH (e:Element:Episode) WHERE $ids IS NULL OR e.id IN $ids
    RETURN collect({id:e.id, mass:e.mass, ingested_at:e.ingested_at}) AS episodes }
  CALL () { MATCH (h:Hit) WHERE $ids IS NULL OR h.episode_id IN $ids
    OPTIONAL MATCH (h)-[:HIT_OF]->(target)
    WITH h, collect(coalesce(target.id, 'invalid-target')) AS targets
    RETURN collect({props:properties(h), targets:targets}) AS hits }
  CALL () { MATCH (c:HitCache) WHERE $ids IS NULL OR c.episode_id IN $ids
    OPTIONAL MATCH (c)-[:CACHE_OF]->(target)
    WITH c, collect(coalesce(target.id, 'invalid-target')) AS targets
    RETURN collect({props:properties(c), targets:targets}) AS caches }
  CALL (hits) { MATCH (o:UsageOutcome) WHERE $ids IS NULL OR o.usage_id IN [h IN hits | h.props.usage_id]
    OPTIONAL MATCH (u:Usage {usage_id:o.usage_id})
    RETURN collect({usage:o.usage_id,reward:o.reward,t:o.reported_at,selection:u.selection}) AS outcomes }
  RETURN episodes, hits, caches, outcomes`;

export function cacheExpectations(evidence: CacheEvidence): { expected: HitCache[]; issues: HitCacheIssue[] } {
  const issues: HitCacheIssue[] = [];
  const events = new Map<string, DynamicsEvent[]>();
  const ids = new Set<string>();
  const keys = new Set<string>();
  const shares = new Map<string, { credit: number; t: number }>();
  const episodes = new Set(evidence.episodes.map(e => e.id));
  evidence.outcomes.forEach(outcome => {
    try {
      const selection = Usage.shape.selection.parse(JSON.parse(outcome.selection ?? "null"));
      if (!Number.isFinite(outcome.reward) || Math.abs(outcome.reward) > 1) throw new RangeError("reward");
      for (const [id, weight] of attributeOutcome(selection)) if (episodes.has(id))
        shares.set(tupleHash([outcome.usage,id,"outcome"]), { credit: 0.2 * outcome.reward * weight, t: outcome.t });
    } catch { issues.push({ code: "invalid_hit_evidence", id: outcome.usage }); }
  });
  for (const row of evidence.hits) {
    const hit = OutcomeHit.safeParse(row.props).data;
    const share = hit && shares.get(hit.idem_key);
    if (!hit || !share || share.credit !== hit.credit || share.t !== hit.t || row.targets.length !== 1 || row.targets[0] !== hit.episode_id || !evidence.episodes.some(e => e.id === hit.episode_id)
      || hit.idem_key !== tupleHash([hit.usage_id, hit.episode_id, hit.kind]) || ids.has(hit.id) || keys.has(hit.idem_key)) { issues.push({ code: "invalid_hit_evidence", id: String(row.props["id"]) }); continue; }
    ids.add(hit.id); keys.add(hit.idem_key);
    const bucket = events.get(hit.episode_id) ?? [];
    bucket.push({ id: hit.id, at: hit.t, credit: hit.credit });
    events.set(hit.episode_id, bucket);
  }
  if (!issues.length) shares.forEach((_, key) => { if (!keys.has(key)) issues.push({ code: "invalid_hit_evidence", id: key }); });
  const expected: HitCache[] = [];
  for (const episode of evidence.episodes) {
    const history = events.get(episode.id);
    if (!history) continue;
    const state = replayDynamics({ initialMass: episode.mass, ingestedAt: episode.ingested_at }, history);
    expected.push({ episode_id: episode.id, s: state.stability, t_last_hit: state.lastHit, hit_count: state.hitCount,
      u_anchor: state.utility, u_at: state.at });
  }
  return { expected, issues };
}
export function cacheMatches(row: CacheEvidence["caches"][number] | undefined, expected: HitCache): boolean {
  return !!row && row.targets.length === 1 && row.targets[0] === expected.episode_id
    && canonicalJson({ ...row.props }) === canonicalJson({ ...expected });
}
