import { EPISODE_SCHEMAS, RpcReportOutcomeParams, type RpcRecallResult } from "@anamnesis/protocol";
import { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { RpcPolicySetParams, RpcPolicyRevokeParams, type RpcPolicyResult } from "@anamnesis/protocol";
import { EchoLineage, EpisodeLineageError, RecallLineageSelection, parseEpisodeLineage, type EpisodeLineageInput } from "@anamnesis/protocol";
import { SemanticResolvedTime, type SemanticSourceContext } from "@anamnesis/protocol";
import { canonicalExtractionBody, extractionBodyDigest } from "@anamnesis/protocol";
import { z } from "zod";
import { attributeOutcome } from "../dynamics/ranking.ts";
import { sha256, StorageContractError, canonicalJson, elementDigest, tupleHash, verifyEpisodeLineage } from "./digest.ts";
import { receiptTime, receiptHash, Usage, type HitCacheVerification, type HitCacheRebuild, type HitCache, ReceiptError } from "./receipts.ts";
import { type InstallationContext, PolicyEvent, policyBody, type PolicyState, requireInstallation } from "./policy.ts";
import { type CacheEvidence, CACHE_EVIDENCE, cacheExpectations, cacheMatches } from "./hit-cache.ts";
import { type ElementNode, nodeProps, StoredHash, decodeHistoricalElement, toElement } from "./records.ts";
import type { StoreCore } from "./core.ts";

export class ReceiptStore {
  constructor(private readonly core: StoreCore) {}

  async createUsageTx(tx: ManagedTransaction, result: RpcRecallResult, context: InstallationContext): Promise<void> {
    const selection = result.results.map(({ id, sources }) => ({ id, sources }));
    const rows: z.infer<typeof RecallLineageSelection> = [];
    for (const item of selection) {
      const roots = new Set<string>();
      let complete = true, depth = 0;
      for (const source of item.sources) {
        const evidence = await tx.run<{ version: number | null; digest: string | null }>(
          `MATCH (e:Element:Episode {id:$source}) RETURN e.episode_digest_version AS version,e.lineage_digest AS digest`, { source });
        const row = evidence.records[0]!;
        if (row.get("version") === null) { complete = false; continue; }
        if (row.get("version") !== 2) throw new EpisodeLineageError("unsupported_digest_version");
        const lineage = await this.lineageTx(tx, source, row.get("digest"));
        for (const root of lineage.root_episode_ids) roots.add(root);
        complete &&= lineage.complete;
        depth = Math.max(depth, lineage.echo_depth);
      }
      rows.push({ element_id: item.id, root_episode_ids: [...roots].sort().slice(0, 16), echo_depth: depth, complete: complete && roots.size <= 16 });
    }
    await tx.run(`CREATE (:Usage $props)`, { props: { usage_id: result.usage_id, created_at: result.diagnostics.now,
      policy_revision: result.diagnostics.policy_revision, selection: canonicalJson(selection), lineage_selection: canonicalJson(rows),
      selection_digest: extractionBodyDigest(rows), ...(context.client_binding ? { client_binding: context.client_binding } : {}) } });
  }
  async lineageTx(tx: ManagedTransaction, episodeId: string, digest: string | null): Promise<EchoLineage> {
    const rows = await tx.run<{ props: Record<string, unknown> }>(
      `MATCH (e:Element:Episode {id:$id}) RETURN properties(e) AS props`, { id: episodeId });
    const row = rows.records[0];
    if (!row) throw new EpisodeLineageError("lineage_unavailable");
    return verifyEpisodeLineage(episodeId, digest, row.get("props"));
  }
  async admitLineageTx(tx: ManagedTransaction, episodeId: string, input: EpisodeLineageInput,
    context: InstallationContext, now: number): Promise<EchoLineage> {
    if (!context.client_binding) throw new EpisodeLineageError("lineage_binding_mismatch");
    if (input.lineage_mode === "direct") return EchoLineage.parse({ episode_id: episodeId, lineage_mode: "direct",
      parent_recall_ids: [], context_digests: [], root_episode_ids: [episodeId], echo_depth: 0, complete: true });
    const policy = await this.core.receiptLockTx(tx), roots = new Set<string>(), contextDigests: string[] = [];
    let depth = 0, complete = true, count = 0;
    for (const id of input.parent_recall_ids) {
      const parent = await this.usageTx(tx, id);
      if (!parent) throw new ReceiptError("unknown_usage");
      if (parent.client_binding !== context.client_binding) throw new EpisodeLineageError("lineage_binding_mismatch");
      if (parent.created_at >= now) throw new EpisodeLineageError("lineage_unavailable");
      if (parent.policy_revision > policy.policy_revision) throw new ReceiptError("policy_unavailable");
      await this.core.authorizeEpisodesTx(tx, [...new Set(parent.selection.flatMap(item => item.sources))], policy);
      const selection = parent.lineage_selection;
      if (extractionBodyDigest(selection) !== parent.selection_digest
        || canonicalExtractionBody(selection.map(item => item.element_id)) !== canonicalExtractionBody(parent.selection.map(item => item.id)))
        throw new EpisodeLineageError("lineage_mismatch");
      contextDigests.push(parent.selection_digest);
      for (const item of selection) {
        count++; complete &&= item.complete;
        depth = Math.max(depth, item.echo_depth);
        for (const root of item.root_episode_ids) roots.add(root);
        await this.core.authorizeEpisodesTx(tx, item.root_episode_ids, policy);
      }
    }
    const orderedRoots = [...roots].sort();
    return EchoLineage.parse({ episode_id: episodeId, lineage_mode: "receipts", parent_recall_ids: input.parent_recall_ids,
      context_digests: contextDigests, root_episode_ids: orderedRoots.slice(0, 16), echo_depth: Math.min(8, depth + 1),
      complete: complete && count > 0 && orderedRoots.length > 0 && orderedRoots.length <= 16 && depth < 8 });
  }
  /** Retained provenance reader for lifecycle semantic adapters. No caller role,
   * lineage, source text or time replacement crosses this custody boundary. */
  async semanticEpisode(sourceId: string, context: InstallationContext): Promise<Omit<SemanticSourceContext["episode"], "content_language">> {
    return this.core.extractionTx(context, (tx, policy) => this.semanticEpisodeTx(tx, sourceId, policy));
  }
  async semanticEpisodeTx(tx: ManagedTransaction, sourceId: string, policy: PolicyState): Promise<Omit<SemanticSourceContext["episode"], "content_language">> {
    await this.core.authorizeEpisodesTx(tx, [z.uuidv7().parse(sourceId)], policy);
    const rows = await tx.run<{ e: ElementNode }>(`MATCH (e:Element:Episode {id:$id}) RETURN e`, { id: sourceId });
    const p = nodeProps(rows.records[0]!.get("e")), element = p["digest_format"] == null ? decodeHistoricalElement(p) : toElement(p);
    const version = p["episode_digest_version"] ?? null;
    if (version !== null && version !== 2) throw new EpisodeLineageError("unsupported_digest_version");
    const lineage = version === 2 ? await this.lineageTx(tx, sourceId, String(p["lineage_digest"])) : null;
    if (elementDigest(element, { payloadHash: StoredHash.parse(p["payload_hash"] ?? null),
      previousRevisionKey: StoredHash.parse(p["previous_revision_key"] ?? null), format: p["digest_format"] ?? null,
      episodeDigestVersion: version, originRole: z.string().nullable().parse(p["origin_role"] ?? null),
      lineageDigest: StoredHash.parse(p["lineage_digest"] ?? null) }) !== p["digest"])
      throw new StorageContractError("revision_conflict", sourceId);
    if (lineage) await this.core.authorizeEpisodesTx(tx, lineage.root_episode_ids, policy);
    // Legacy seconds are instants; minutes have no equivalent and fail
    // schema validation rather than gaining invented precision. Never cast into the
    // unrelated resolved-time ABI. Coarse UTC alignment is validated explicitly.
    const precision = element.time!.precision;
    const time = SemanticResolvedTime.parse({ time_value: element.time!.value, time_utc: Date.parse(element.time!.value),
      time_precision: precision === "second" ? "instant" : precision });
    const provenance: SemanticSourceContext["episode"]["provenance"] = lineage
      ? { episode_digest_version: 2, origin_role: parseEpisodeLineage({ origin_role: p["origin_role"], lineage_mode: lineage.lineage_mode, parent_recall_ids: lineage.parent_recall_ids }).origin_role,
        lineage, lineage_digest: String(p["lineage_digest"]) }
      : { episode_digest_version: 1, origin_role: null, lineage: null, lineage_digest: null };
    return { id: sourceId, schema: z.enum(EPISODE_SCHEMAS).parse(element.schema),
      revision_key: receiptHash.parse(p["revision_key"]), content_digest: sha256(element.content), content: element.content,
      ingest_seq: receiptTime.positive().parse(p["ingest_seq"]), time,
      speaker: { origin_source: element.origin.source, origin_actor: element.origin.actor }, provenance };
  }
  async setPolicy(input: RpcPolicySetParams, context: InstallationContext): Promise<RpcPolicyResult> {
    requireInstallation(context);
    const request = RpcPolicySetParams.parse(input);
    return this.changePolicy(request.policy_id, request, context);
  }
  async revokePolicy(input: RpcPolicyRevokeParams, context: InstallationContext): Promise<RpcPolicyResult> {
    requireInstallation(context);
    return this.changePolicy(RpcPolicyRevokeParams.parse(input).policy_id, null, context);
  }
  private async changePolicy(id: string, request: RpcPolicySetParams | null, context: InstallationContext): Promise<RpcPolicyResult> {
    return this.core.withWriteTx(async tx => {
      const state = await this.core.receiptLockTx(tx);
      const old = state.denies.get(id);
      const action = request ? "deny" : "revoke";
      if (request && old && policyBody({ policy_id: old.policy_id, selector: old.selector, scope: old.scope }) !== policyBody(request)) throw new ReceiptError("idempotency_conflict");
      if (!request && !old) throw new ReceiptError("unknown_policy");
      const applied = request ? !old : !state.revoked.has(id);
      const selection = request ?? { policy_id: old!.policy_id, selector: old!.selector, scope: old!.scope };
      if (applied) {
        if (request && state.denies.size - state.revoked.size >= 256) throw new ReceiptError("resource_exhausted");
        const event = PolicyEvent.parse({ ...selection, action, principal: context.principal,
          revision: state.policy_revision + 1, created_at: this.core.clock() });
        const body = policyBody(event);
        await tx.run(`CREATE (:PolicyEvent {key:$key,revision:$revision,body:$body,body_digest:$digest})
          WITH 1 AS ignored MATCH (m:Meta {key:'meta'}) SET m.policy_revision=$revision`,
          { key: `${id}:${action}`, revision: event.revision, body, digest: sha256(body) });
      }
      return { ...selection, action, applied, policy_revision: state.policy_revision + (applied ? 1 : 0), evaluator: "episode-source-v1" };
    });
  }
  private async usageTx(tx: ManagedTransaction, id: string): Promise<Usage | null> {
    const rows = await tx.run<{ props: { selection: string; lineage_selection: string } }>(
      `MATCH (u:Usage {usage_id:$id}) RETURN properties(u) AS props`, { id });
    const props = rows.records[0]?.get("props");
    return props ? Usage.parse({ ...props, selection: JSON.parse(props.selection), lineage_selection: JSON.parse(props.lineage_selection) }) : null;
  }
  async reportOutcome(input: RpcReportOutcomeParams, context: InstallationContext) {
    requireInstallation(context);
    const { usage_id, idempotency_key, reward: supplied } = RpcReportOutcomeParams.parse(input);
    const reward = Math.max(-1, Math.min(1, supplied)) || 0;
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      const usage = await this.usageTx(tx, usage_id);
      if (!usage) throw new ReceiptError("unknown_usage");
      if (usage.policy_revision > policy.policy_revision) throw new ReceiptError("policy_unavailable");
      const weights = attributeOutcome(usage.selection);
      await this.core.authorizeEpisodesTx(tx, [...weights.keys()], policy);
      const existing = await tx.run<{ usage: string; key: string; reward: number }>(
        `MATCH (o:UsageOutcome) WHERE o.usage_id=$usage_id OR o.idempotency_key=$idempotency_key
         RETURN o.usage_id AS usage,o.idempotency_key AS key,o.reward AS reward`, { usage_id, idempotency_key });
      const prior = existing.records.find(row => row.get("key") === idempotency_key);
      if (prior) {
        if (prior.get("usage") !== usage_id || prior.get("reward") !== reward) throw new ReceiptError("idempotency_conflict");
        return { reward, applied: false };
      }
      if (existing.records.length) throw new ReceiptError("outcome_already_reported");
      const t = receiptTime.parse(this.core.clock());
      await tx.run(`CREATE (:UsageOutcome $props)`, { props: { usage_id, idempotency_key, reward, reported_at: t } });
      const hits = Array.from(weights, ([episode_id, weight]) => ({ id: uuidv7(), usage_id, episode_id, t,
        kind: "outcome", idem_key: tupleHash([usage_id, episode_id, "outcome"]), credit: 0.2 * reward * weight }));
      if (hits.length) {
        await tx.run(`UNWIND $hits AS props MATCH (e:Element:Episode {id:props.episode_id}) CREATE (h:Hit)-[:HIT_OF]->(e) SET h=props`, { hits });
        await this.rebuildHitCacheTx(tx, [...weights.keys()]);
      }
      return { reward, applied: true };
    });
  }
  async getHitCache(episodeId: string): Promise<HitCache | null> {
    const rows = await this.core.run<{ props: HitCache }>(`MATCH (c:HitCache {episode_id:$id}) RETURN properties(c) AS props`, { id: z.uuidv7().parse(episodeId) });
    return rows[0]?.props ?? null;
  }
  async verifyHitCache(): Promise<HitCacheVerification> {
    const rows = await this.core.run<CacheEvidence>(CACHE_EVIDENCE, { ids: null });
    const evidence = rows[0]!;
    const { expected, issues } = cacheExpectations(evidence);
    for (const cache of expected) {
      if (!cacheMatches(evidence.caches.find((row) => row.props.episode_id === cache.episode_id), cache)) {
        issues.push({ code: "hit_cache_mismatch", id: cache.episode_id });
      }
    }
    for (const row of evidence.caches) {
      if (!expected.some((cache) => cache.episode_id === row.props.episode_id)) issues.push({ code: "hit_cache_mismatch", id: row.props.episode_id });
    }
    return { state: "verified", hits: evidence.hits.length, issues };
  }
  async rebuildHitCache(): Promise<HitCacheRebuild> {
    return this.core.withWriteTx(async (tx) => {
      await this.core.receiptLockTx(tx);
      return this.rebuildHitCacheTx(tx, null);
    });
  }
  private async rebuildHitCacheTx(tx: ManagedTransaction, ids: string[] | null): Promise<HitCacheRebuild> {
    const rows = await tx.run<CacheEvidence>(CACHE_EVIDENCE, { ids });
    const evidence = rows.records[0]!.toObject();
    const { expected, issues } = cacheExpectations(evidence);
    if (issues.length) throw new ReceiptError("invalid_hit_evidence");
    let created = 0;
    let removed = 0;
    for (const row of evidence.caches) {
      const cache = expected.find((cache) => cache.episode_id === row.props.episode_id);
      if (!cache || !cacheMatches(row, cache)) {
        await tx.run(`MATCH (c:HitCache {episode_id:$id}) DETACH DELETE c`, { id: row.props.episode_id });
        removed++;
      }
    }
    for (const cache of expected) {
      if (cacheMatches(evidence.caches.find((row) => row.props.episode_id === cache.episode_id), cache)) continue;
      await tx.run(`MATCH (e:Element:Episode {id:$id}) CREATE (c:HitCache $props)-[:CACHE_OF]->(e)`, { id: cache.episode_id, props: cache });
      created++;
    }
    return { state: "rebuilt", hits: evidence.hits.length, created, removed };
  }
}
