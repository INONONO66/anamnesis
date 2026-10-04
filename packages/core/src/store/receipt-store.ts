import { EPISODE_SCHEMAS } from "@anamnesis/protocol";
import { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { RpcPolicySetParams, RpcPolicyRevokeParams, type RpcPolicyResult } from "@anamnesis/protocol";
import { EchoLineage, EpisodeLineageError, RecallLineageSelection, parseEpisodeLineage, type EpisodeLineageInput } from "@anamnesis/protocol";
import { SemanticResolvedTime, type SemanticSourceContext } from "@anamnesis/protocol";
import { canonicalExtractionBody, extractionBodyDigest } from "@anamnesis/protocol";
import { z } from "zod";
import { attributeOutcome } from "../dynamics/ranking.ts";
import { canonicalContext } from "../recall.ts";
import { receiptBodyDigestInput, canonicalReceiptJson } from "../receipt-digest.ts";
import { sha256, StorageContractError, canonicalJson, elementDigest, tupleHash, verifyEpisodeLineage } from "./digest.ts";
import { receiptTime, receiptHash, IssueReceiptInput, RecallReceipt, RecallTransportInput, RecallTransport, CommitReceiptInput, type CommitReceiptResult, type ReceiptStatus, type HitCacheVerification, type HitCacheRebuild, type HitCache, ReceiptHit, ReceiptError } from "./receipts.ts";
import { type InstallationContext, PolicyEvent, policyBody, type PolicyState, requireInstallation } from "./policy.ts";
import { type CacheEvidence, CACHE_EVIDENCE, cacheExpectations, cacheMatches } from "./hit-cache.ts";
import { type ElementNode, nodeProps, StoredHash, decodeHistoricalElement, toElement } from "./records.ts";
import type { StoreCore } from "./core.ts";

/** New hits for a feedback commit: one recall_hit per contributing source (deduplicated by idem key against `keys`) and,
 * for a first outcome, one weighted outcome hit per attributed source. */
function feedbackHits(receipt: RecallReceipt, request: { operation_id: string; reward?: number | undefined }, adopted: string[], selected: string[],
  keys: Set<string>, now: number, firstOutcome: boolean): ReceiptHit[] {
  const hits: ReceiptHit[] = [];
  const coefficients = new Map<string, number>();
  for (const item of receipt.primaries.filter((item) => adopted.includes(item.id))) {
    for (const source of item.sources) coefficients.set(source, Math.min(1, (coefficients.get(source) ?? 0) + 1 / item.sources.length));
  }
  const append = (episodeId: string, kind: "recall_hit" | "outcome", value: number): void => {
    const key = tupleHash([receipt.recall_id, episodeId, kind]);
    if (keys.has(key)) return;
    const contributing = kind === "recall_hit" ? adopted : selected;
    const base = { id: uuidv7(), episode_id: episodeId, operation_id: request.operation_id,
      namespace: receipt.recall_id, idem_key: key, t: now, config_version: receipt.config_version,
      attribution: receipt.primaries.filter((item) => contributing.includes(item.id) && item.sources.includes(episodeId)) };
    hits.push(ReceiptHit.parse(kind === "recall_hit"
      ? { ...base, kind, kappa_eff: value }
      : { ...base, kind, kappa_eff: 0, reward: request.reward, weight: value }));
  };
  for (const [id, coefficient] of coefficients) append(id, "recall_hit", coefficient);
  if (firstOutcome) for (const [id, weight] of attributeOutcome(receipt.primaries, selected)) append(id, "outcome", weight);
  return hits;
}

export class ReceiptStore {
  constructor(private readonly core: StoreCore) {}

  /** Privileged server issuer, Episodes only; not semantic recall or a wire API. */
  async issueReceipt(input: IssueReceiptInput, context: InstallationContext): Promise<RecallReceipt> {
    requireInstallation(context);
    const request = IssueReceiptInput.parse(input);
    return this.core.withWriteTx(async (tx) => this.issueReceiptTx(tx, request, context, await this.core.receiptLockTx(tx)));
  }
  async issueReceiptTx(tx: ManagedTransaction, request: z.output<typeof IssueReceiptInput>, context: InstallationContext,
    policy: PolicyState, serving?: RecallReceipt["serving"]): Promise<RecallReceipt> {
      const bodyDigest = sha256(canonicalReceiptJson(receiptBodyDigestInput({ ...request, ...(serving ? { serving } : {}) })));
      const old = await this.receiptTx(tx, request.recall_id);
      if (old) {
        await this.core.authorizeReceiptTx(tx, old, policy, context);
        if (receiptTime.parse(this.core.clock()) >= old.expires_at) throw new ReceiptError("receipt_expired");
        if (old.body_digest !== bodyDigest || old.commit_mode !== context.commit_mode) throw new ReceiptError("idempotency_conflict");
        return old;
      }
      const sourceRows = await tx.run(`MATCH (e:Element) WHERE e.id IN $ids
        OPTIONAL MATCH (e:Fact)-[:DERIVED_FROM]->(source:Episode)
        RETURN e.id AS id,coalesce(source.id,e.id) AS source`, { ids: request.primary_ids });
      if (sourceRows.records.length !== request.primary_ids.length) throw new ReceiptError("invalid_selection");
      await this.core.authorizeEpisodesTx(tx, [...new Set(sourceRows.records.map(row => row.get("source")))], policy);
      const createdAt = serving?.response.diagnostics.now ?? receiptTime.parse(this.core.clock());
      const sourceById = new Map(sourceRows.records.map(row => [row.get("id"), row.get("source")]));
      const primaries = request.primary_ids.map((id, rank) => ({ id, rank, sources: [sourceById.get(id)!] }));
      // Internal compatibility receipts do not acquire lineage authority.
      const selection = context.client_binding ? await this.lineageSelectionTx(tx, request.primary_ids) : undefined;
      const receipt = RecallReceipt.parse({ ...request, primaries, ...(serving ? { serving } : {}), format: "episode-selection-v1",
        ...(selection ? { client_binding: context.client_binding, lineage_selection: selection } : {}),
        principal: "installation", commit_mode: context.commit_mode, created_at: createdAt, expires_at: createdAt + request.receipt_ttl_ms,
        structure_revision: policy.structure_revision, policy_revision: policy.policy_revision,
        config_version: "g003-dynamics-v1", body_digest: bodyDigest,
        selection_digest: sha256(canonicalJson(selection ?? primaries.map((p) => ({ element_id: p.id, root_episode_ids: p.sources, echo_depth: 0, complete: true })))),
      });
      await tx.run(`CREATE (r:Receipt:RecallReceipt {recall_id:$id, body:$body, body_digest:$digest,
        created_at:$createdAt, expires_at:$expiresAt})`, {
        id: receipt.recall_id, body: canonicalContext(receipt), digest: bodyDigest,
        createdAt: receipt.created_at, expiresAt: receipt.expires_at,
      });
      await tx.run(`MATCH (r:RecallReceipt {recall_id:$id})
        UNWIND $primaries AS p MATCH (e:Element {id:p.id})
        CREATE (r)-[:PRIMARY {rank:p.rank}]->(e)`, { id: receipt.recall_id, primaries });
      return receipt;
  }
  async lineageTx(tx: ManagedTransaction, episodeId: string, digest: string | null): Promise<EchoLineage> {
    const rows = await tx.run<{ props: Record<string, unknown> }>(
      `MATCH (e:Episode {id:$id}) RETURN properties(e) AS props`, { id: episodeId });
    const row = rows.records[0];
    if (!row) throw new EpisodeLineageError("lineage_unavailable");
    return verifyEpisodeLineage(episodeId, digest, row.get("props"));
  }
  private async lineageSelectionTx(tx: ManagedTransaction, ids: string[]): Promise<z.infer<typeof RecallLineageSelection>> {
    const items: z.infer<typeof RecallLineageSelection> = [];
    for (const id of ids) {
      const rows = await tx.run<{ version: number | null; digest: string | null; source: string | null }>(
        `MATCH (e:Element {id:$id}) OPTIONAL MATCH (e:Fact)-[:DERIVED_FROM]->(source:Episode)
         RETURN coalesce(e.episode_digest_version,source.episode_digest_version) AS version,
           coalesce(e.lineage_digest,source.lineage_digest) AS digest,source.id AS source`, { id });
      const row = rows.records[0];
      if (!row) throw new ReceiptError("invalid_selection");
      const source = row.get("source") ?? id, version = row.get("version"), digest = row.get("digest");
      if (version === null) items.push({ element_id: id, root_episode_ids: [], echo_depth: 0, complete: false });
      else {
        if (version !== 2) throw new EpisodeLineageError("unsupported_digest_version");
        const lineage = await this.lineageTx(tx, source, digest);
        items.push({ element_id: id, root_episode_ids: lineage.root_episode_ids, echo_depth: lineage.echo_depth, complete: lineage.complete });
      }
    }
    return RecallLineageSelection.parse(items);
  }
  async admitLineageTx(tx: ManagedTransaction, episodeId: string, input: EpisodeLineageInput,
    context: InstallationContext, now: number): Promise<EchoLineage> {
    if (!context.client_binding) throw new EpisodeLineageError("lineage_binding_mismatch");
    if (input.lineage_mode === "direct") return EchoLineage.parse({ episode_id: episodeId, lineage_mode: "direct",
      parent_recall_ids: [], context_digests: [], root_episode_ids: [episodeId], echo_depth: 0, complete: true });
    const policy = await this.core.receiptLockTx(tx), roots = new Set<string>(), contextDigests: string[] = [];
    let depth = 0, complete = true, count = 0;
    for (const id of input.parent_recall_ids) {
      const parent = await this.receiptTx(tx, id);
      if (!parent) throw new ReceiptError("unknown_recall");
      if (parent.client_binding !== context.client_binding) throw new EpisodeLineageError("lineage_binding_mismatch");
      if (parent.created_at >= now) throw new EpisodeLineageError("lineage_unavailable");
      // TTL governs feedback, not provenance. Retained expired receipts remain
      // usable; deletion makes new admission unknown, never changes a child.
      await this.core.authorizeReceiptTx(tx, parent, policy, context);
      const selection = parent.lineage_selection;
      if (!selection) throw new EpisodeLineageError("lineage_unavailable");
      if (extractionBodyDigest(selection) !== parent.selection_digest
        || canonicalExtractionBody(selection.map(item => item.element_id)) !== canonicalExtractionBody(parent.primary_ids))
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
    const rows = await tx.run<{ e: ElementNode }>(`MATCH (e:Episode {id:$id}) RETURN e`, { id: sourceId });
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
  private async receiptTx(tx: ManagedTransaction, id: string): Promise<RecallReceipt | null> {
    const rows = await tx.run<{ body: string }>(`MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body`, { id });
    return rows.records[0] ? RecallReceipt.parse(JSON.parse(rows.records[0].get("body"))) : null;
  }
  async getReceipt(recallId: string): Promise<RecallReceipt | null> {
    const id = z.uuidv7().parse(recallId);
    const session = this.core.driver.session({ database: this.core.database });
    try { return await session.executeRead((tx) => this.receiptTx(tx, id)); }
    finally { await session.close(); }
  }
  /** The transport append deliberately does not consult current policy: even a
   * cancelled/denied publication must retain its truthful, content-free audit. */
  async recordRecallTransport(input: RecallTransportInput, context: InstallationContext) {
    requireInstallation(context);
    const request = RecallTransportInput.parse(input);
    return this.core.withWriteTx(async tx => {
      await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.ingest_seq=m.ingest_seq`);
      const receipt = await this.receiptTx(tx, request.recall_id);
      if (!receipt) throw new ReceiptError("unknown_recall");
      if (receipt.principal !== context.principal) throw new ReceiptError("unauthenticated");
      if (receipt.commit_mode !== context.commit_mode) throw new ReceiptError("commit_mode_mismatch");
      const prior = await tx.run<{ body: string }>(`MATCH (a:RecallTransport {recall_id:$id}) RETURN a.body AS body`, { id: request.recall_id });
      if (prior.records[0]) {
        const audit = RecallTransport.parse(JSON.parse(prior.records[0].get("body")));
        if (audit.state !== request.state) throw new ReceiptError("idempotency_conflict");
        return audit;
      }
      const audit = RecallTransport.parse({ ...request, principal: context.principal, commit_mode: context.commit_mode, created_at: this.core.clock(), boundary: "node-write-callback-v1" });
      const body = canonicalJson(audit);
      await tx.run(`MATCH (r:RecallReceipt {recall_id:$id})
        CREATE (a:Receipt:RecallTransport $props)-[:TRANSPORT_OF]->(r)`, {
        id: request.recall_id, props: { ...audit, operation_id: request.recall_id, body, body_digest: sha256(body) },
      });
      return audit;
    });
  }
  /** Audit only, after persisted local completion. Replays are explicit and
   * policy-checked; startup never infers delivery from receipt existence. */
  async exposeRecall(recallId: string, context: InstallationContext): Promise<{ applied: number }> {
    requireInstallation(context);
    if (context.commit_mode !== "auto") throw new ReceiptError("commit_mode_mismatch");
    const id = z.uuidv7().parse(recallId);
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      const receipt = await this.receiptTx(tx, id);
      if (!receipt) throw new ReceiptError("unknown_recall");
      if (receipt.commit_mode !== "auto") throw new ReceiptError("commit_mode_mismatch");
      await this.core.authorizeReceiptTx(tx, receipt, policy, context);
      const rows = await tx.run<{ body: string }>(`MATCH (a:RecallTransport {recall_id:$id}) RETURN a.body AS body`, { id });
      if (!rows.records[0]) throw new ReceiptError("invalid_selection");
      const audit = RecallTransport.parse(JSON.parse(rows.records[0].get("body")));
      if (audit.state !== "local_complete") throw new ReceiptError("invalid_selection");
      const existing = await tx.run<{ key: string }>(`MATCH (h:Hit {namespace:$id,kind:'exposure'}) RETURN h.idem_key AS key`, { id });
      const keys = new Set(existing.records.map(row => row.get("key")));
      const selected = receipt.primaries.slice(0, 3), hits: ReceiptHit[] = [];
      for (const source of new Set(selected.flatMap(item => item.sources))) {
        const key = tupleHash([id, source, "exposure"]);
        if (keys.has(key)) continue;
        hits.push(ReceiptHit.parse({ id: uuidv7(), episode_id: source, operation_id: id, namespace: id,
          idem_key: key, t: audit.created_at, kind: "exposure", kappa_eff: 0, config_version: receipt.config_version,
          attribution: selected.filter(item => item.sources.includes(source)) }));
      }
      await this.appendReceiptHitsTx(tx, hits, "RecallTransport");
      return { applied: hits.length };
    });
  }
  async getReceiptStatus(operationId: string): Promise<ReceiptStatus> {
    const id = z.uuidv7().parse(operationId);
    const rows = await this.core.run<{ bodyDigest: string; createdAt: number; result: string }>(
      `MATCH (r:RecallFeedback {operation_id:$id}) RETURN r.body_digest AS bodyDigest,
       r.created_at AS createdAt, r.result AS result`, { id });
    const row = rows[0];
    return row ? { state: "committed", operation_id: id, body_digest: row.bodyDigest,
      created_at: row.createdAt, result: JSON.parse(row.result) } : { state: "unknown", operation_id: id };
  }
  async commitReceipt(input: CommitReceiptInput, context: InstallationContext): Promise<CommitReceiptResult> {
    requireInstallation(context);
    if (context.commit_mode !== "receipt") throw new ReceiptError("commit_mode_mismatch");
    const parsed = CommitReceiptInput.parse(input);
    const request = { ...parsed, ...(parsed.adopted === undefined ? {} : { adopted: [...parsed.adopted].sort() }) };
    const bodyFields: Record<string, z.core.util.JSONType> = { operation_id: request.operation_id, recall_id: request.recall_id };
    if (request.adopted !== undefined) bodyFields["adopted"] = request.adopted;
    if (request.reward !== undefined) bodyFields["reward"] = request.reward;
    const body = canonicalJson(bodyFields);
    const digest = sha256(body);
    return this.core.withWriteTx(async (tx) => {
      const policy = await this.core.receiptLockTx(tx);
      const receipt = await this.receiptTx(tx, request.recall_id);
      if (!receipt) throw new ReceiptError("unknown_recall");
      const now = receiptTime.parse(this.core.clock());
      if (now >= receipt.expires_at) throw new ReceiptError("receipt_expired");
      await this.core.authorizeReceiptTx(tx, receipt, policy, context);
      if (receipt.commit_mode !== "receipt") throw new ReceiptError("commit_mode_mismatch");
      const prior = await tx.run<{ digest: string; result: string }>(
        `MATCH (r:RecallFeedback {operation_id:$id}) RETURN r.body_digest AS digest,r.result AS result`, { id: request.operation_id });
      if (prior.records[0]) {
        if (prior.records[0].get("digest") !== digest) throw new ReceiptError("idempotency_conflict");
        return { ...JSON.parse(prior.records[0].get("result")), applied: false };
      }
      const adopted = request.adopted ?? [];
      if (adopted.some((id) => !receipt.primary_ids.includes(id))) throw new ReceiptError("invalid_selection");
      const selected = [...(request.adopted ?? receipt.primary_ids)].sort();
      const outcomeDigest = request.reward === undefined ? null : sha256(canonicalJson({ reward: request.reward, selected }));
      const previousOutcome = await tx.run<{ digest: string }>(
        `MATCH (r:RecallOutcome {recall_id:$id}) RETURN r.outcome_digest AS digest`, { id: receipt.recall_id });
      const outcome = previousOutcome.records[0];
      if (outcome && outcomeDigest !== null && outcome.get("digest") !== outcomeDigest) throw new ReceiptError("idempotency_conflict");
      const existingHits = await tx.run<{ key: string }>(`MATCH (h:Hit {namespace:$id}) RETURN h.idem_key AS key`, { id: receipt.recall_id });
      const keys = new Set(existingHits.records.map((row) => row.get("key")));
      const hits = feedbackHits(receipt, request, adopted, selected, keys, now, outcomeDigest !== null && !outcome);
      const result: CommitReceiptResult = { operation_id: request.operation_id, recall_id: receipt.recall_id,
        adopted, reward: request.reward ?? null, applied: hits.length > 0 || (outcomeDigest !== null && !outcome) };
      await tx.run(`MATCH (r:RecallReceipt {recall_id:$recallId})
        CREATE (f:Receipt:RecallFeedback {operation_id:$operationId, recall_id:$recallId,
          body:$body, body_digest:$digest, created_at:$now, result:$result})-[:FEEDBACK_OF]->(r)`,
        { recallId: receipt.recall_id, operationId: request.operation_id, body, digest, now, result: canonicalJson({ ...result }) });
      if (outcomeDigest !== null && !outcome) {
        await tx.run(`MATCH (f:RecallFeedback {operation_id:$operationId})
          CREATE (o:RecallOutcome {recall_id:$recallId, outcome_digest:$digest, reward:$reward,
            selected:$selected, created_at:$now})-[:ACCEPTED_BY]->(f)`,
          { operationId: request.operation_id, recallId: receipt.recall_id, digest: outcomeDigest, reward: request.reward, selected, now });
      }
      await this.appendReceiptHitsTx(tx, hits, "RecallFeedback");
      return result;
    });
  }
  private async appendReceiptHitsTx(tx: ManagedTransaction, hits: ReceiptHit[], producer: "RecallFeedback" | "RecallTransport"): Promise<void> {
    for (const hit of hits) {
      const body = canonicalJson(hit);
      await tx.run(`MATCH (e:Element:Episode {id:$episodeId}),(f:${producer} {operation_id:$operationId})
        CREATE (h:Hit $props)-[:HIT_OF]->(e) CREATE (h)-[:RECORDED_BY]->(f)`, {
        episodeId: hit.episode_id, operationId: hit.operation_id,
        props: { ...hit, attribution: canonicalJson(hit.attribution), body, body_digest: sha256(body) },
      });
    }
    if (hits.length) await this.rebuildHitCacheTx(tx, [...new Set(hits.map(hit => hit.episode_id))]);
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
