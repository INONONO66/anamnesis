import { replayDynamics, type DynamicsEvent } from "../dynamics/state.ts";
import { ADOPTION_NUMERIC_VERSION } from "../dynamics/adoption-numeric.ts";
import { sha256, canonicalJson, tupleHash } from "./digest.ts";
import { type HitCacheIssue, type HitCache, ReceiptHit } from "./receipts.ts";

export type CacheEvidence = {
  episodes: { id: string; mass: number; ingested_at: number }[];
  hits: { props: Record<string, unknown>; targets: string[] }[];
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
  RETURN episodes, hits, caches`;

type Hit = ReturnType<typeof ReceiptHit.parse>;
type HitRow = CacheEvidence["hits"][number];
/** The stored hit decoded and checked against its own digest, properties, target and idem key; null when any check fails. Throws non-syntax errors. */
function decodeHit(row: HitRow, evidence: CacheEvidence, ids: Set<string>, keys: Set<string>): Hit | null {
  let decoded: unknown;
  try { decoded = typeof row.props["body"] === "string" ? JSON.parse(row.props["body"]) : null; }
  catch (error) {
    if (!(error instanceof SyntaxError)) throw error;
    return null;
  }
  const parsed = ReceiptHit.safeParse(decoded);
  const hit = parsed.success ? parsed.data : null;
  const body = hit ? canonicalJson(hit) : "";
  if (!hit || row.props["body"] !== body || row.props["body_digest"] !== sha256(body)
    || Object.entries(hit).some(([key, value]) => key !== "attribution" && row.props[key] !== value)
    || row.props["attribution"] !== canonicalJson(hit.attribution)
    || row.targets.length !== 1 || row.targets[0] !== hit.episode_id
    || !evidence.episodes.some((e) => e.id === hit.episode_id)
    || hit.idem_key !== tupleHash([hit.namespace, hit.episode_id, hit.kind])
    || ids.has(hit.id) || keys.has(hit.idem_key)) return null;
  return hit;
}
function hitEvent(hit: Hit): DynamicsEvent {
  if (hit.kind === "recall_hit") return { id: hit.id, at: hit.t, kind: hit.kind, kappa: hit.kappa_eff };
  if (hit.kind === "outcome") return { id: hit.id, at: hit.t, kind: hit.kind, reward: hit.reward, weight: hit.weight };
  return { id: hit.id, at: hit.t, kind: hit.kind };
}
export function cacheExpectations(evidence: CacheEvidence): { expected: HitCache[]; issues: HitCacheIssue[] } {
  const issues: HitCacheIssue[] = [];
  const events = new Map<string, DynamicsEvent[]>();
  const ids = new Set<string>();
  const keys = new Set<string>();
  for (const row of evidence.hits) {
    const hit = decodeHit(row, evidence, ids, keys);
    if (!hit) { issues.push({ code: "invalid_hit_evidence", id: String(row.props["id"]) }); continue; }
    ids.add(hit.id); keys.add(hit.idem_key);
    const bucket = events.get(hit.episode_id) ?? [];
    bucket.push(hitEvent(hit));
    events.set(hit.episode_id, bucket);
  }
  const expected: HitCache[] = [];
  for (const episode of evidence.episodes) {
    if (!events.has(episode.id) && !evidence.caches.some((row) => row.props.episode_id === episode.id)) continue;
    const history = events.get(episode.id) ?? [];
    const state = replayDynamics({ initialMass: episode.mass, ingestedAt: episode.ingested_at, priorRewardSum: 0, priorWeight: 0 }, history);
    const ordered = [...history].sort((a, b) => a.at - b.at || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const sum = ordered.reduce((total, event) => total + (event.kind === "outcome" ? event.weight * event.reward : 0), 0);
    expected.push({ episode_id: episode.id, s: state.stability, t_last_hit: state.lastHit,
      hit_count: state.hitCount, utility_reward_sum: sum, utility_weight: state.weight,
      utility: state.utility, event_ids: state.eventIds, config_version: "g003-dynamics-v1", numeric_version: ADOPTION_NUMERIC_VERSION });
  }
  return { expected, issues };
}
export function cacheMatches(row: CacheEvidence["caches"][number] | undefined, expected: HitCache): boolean {
  return !!row && row.targets.length === 1 && row.targets[0] === expected.episode_id
    && canonicalJson({ ...row.props }) === canonicalJson({ ...expected });
}
