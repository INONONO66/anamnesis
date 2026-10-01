import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import { RpcEmbeddingRecoverParams, RpcEmbeddingAttempt, RpcEmbeddingRequeueParams, type RpcEmbeddingRequeueResult } from "@anamnesis/protocol";
import { EmbeddingError, embeddingProfileId, validateVector } from "../embedding.ts";
import { RecallError } from "../recall.ts";
import { canonicalJson, tupleHash } from "./digest.ts";
import { ReceiptError } from "./receipts.ts";
import { type InstallationContext, requireInstallation } from "./policy.ts";
import type { StoreCore } from "./core.ts";

const EMBEDDING_MAX_DEFERRALS = 8;
/** Backoff before a deferred outbox entry is due again: 30 s doubling per deferral, capped at one hour. */
const embeddingRetryDelay = (deferrals: number): number => Math.min(30_000 * 2 ** (deferrals - 1), 3_600_000);

export class EmbeddingStore {
  constructor(private readonly core: StoreCore) {}

  /** One explicit retry operation per Episode. Reusing a completed operation is
   * a no-op; retry a quarantined or deferred attempt with a new operation ID. Pending
   * rows can resume after process loss. Provider work never runs in a retried DB tx.
   * An operator-driven recover never exhausts: transient failures always defer. A terminal
   * outcome retires the Episode's queued outbox entry, so the worker has nothing left to do. */
  async recoverEmbedding(input: RpcEmbeddingRecoverParams, context: InstallationContext): Promise<RpcEmbeddingAttempt> {
    return this.attemptEmbedding(input, context, true);
  }
  /** `deferrable` false turns a transient failure into the terminal provider_unavailable_exhausted quarantine (outbox budget spent). */
  private async attemptEmbedding(input: RpcEmbeddingRecoverParams, context: InstallationContext, deferrable: boolean): Promise<RpcEmbeddingAttempt> {
    requireInstallation(context);
    const request = RpcEmbeddingRecoverParams.parse(input), provider = this.core.embeddingProvider;
    if (!provider) throw new RecallError("embedding_not_configured");
    const profile = provider.profile, profileId = embeddingProfileId(profile);
    const prepared = await this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      await this.core.authorizeEpisodesTx(tx, [request.episode_id], policy);
      const rows = await tx.run<{ content: string; revision: string; digest: string }>(
        `MATCH (e:Element:Episode {id:$id}) RETURN e.content AS content,e.revision_key AS revision,e.digest AS digest`, { id: request.episode_id });
      const row = rows.records[0]!;
      const oldRows = await tx.run<{ body: string }>(`MATCH (a:EmbeddingAttempt {operation_id:$id}) RETURN a.body AS body`, { id: request.operation_id });
      const old = oldRows.records[0] ? RpcEmbeddingAttempt.parse(JSON.parse(oldRows.records[0].get("body"))) : null;
      if (old && (old.episode_id !== request.episode_id || old.profile_id !== profileId || old.input_revision !== row.get("revision") || old.input_digest !== row.get("digest")))
        throw new ReceiptError("idempotency_conflict");
      const attempt = old ?? RpcEmbeddingAttempt.parse({ ...request, profile_id: profileId, model: profile.model,
        model_incarnation: profile.model_incarnation, dimensions: profile.dimensions,
        input_revision: row.get("revision"), input_digest: row.get("digest"), created_at: this.core.clock(), completed_at: null, state: "pending", reason: null, detail: null });
      if (!old) await tx.run(`CREATE (:EmbeddingAttempt {operation_id:$id,episode_id:$episode,profile_id:$profile,state:'pending',body:$body})`,
        { id: request.operation_id, episode: request.episode_id, profile: profileId, body: canonicalJson(attempt) });
      return { attempt, content: row.get("content") };
    });
    if (prepared.attempt.state !== "pending") return prepared.attempt;
    let vector: number[] | null = null, reason: RpcEmbeddingAttempt["reason"] = null, detail: string | null = null;
    try { vector = validateVector(await provider.embed(prepared.content, "document"), profile); }
    catch (error) { if (!(error instanceof EmbeddingError)) throw error; reason = error.reason; detail = error.detail?.slice(0, 256) ?? null; }
    // provider_unavailable is the only transient reason: it defers (the Episode stays queued) until the caller's budget is spent.
    if (reason === "provider_unavailable" && !deferrable) reason = "provider_unavailable_exhausted";
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      await this.core.authorizeEpisodesTx(tx, [request.episode_id], policy);
      const rows = await tx.run<{ body: string; revision: string; digest: string }>(
        `MATCH (a:EmbeddingAttempt {operation_id:$operation}),(e:Element:Episode {id:$episode})
         RETURN a.body AS body,e.revision_key AS revision,e.digest AS digest`, { operation: request.operation_id, episode: request.episode_id });
      const row = rows.records[0]!, prior = RpcEmbeddingAttempt.parse(JSON.parse(row.get("body")));
      if (prior.state !== "pending") return prior;
      if (row.get("revision") !== prior.input_revision || row.get("digest") !== prior.input_digest) reason = "stale_input";
      const state = reason === null ? "succeeded" : reason === "provider_unavailable" ? "deferred" : "quarantined";
      const attempt = RpcEmbeddingAttempt.parse({ ...prior, state, reason, detail, completed_at: this.core.clock() });
      if (!reason && vector) {
        const key = tupleHash([request.episode_id, profileId]);
        // First valid vector for this immutable input/model wins. Neither a
        // failed nor a concurrent retry can overwrite it or another model.
        await tx.run(`MERGE (v:EmbeddingVector {key:$key})
          ON CREATE SET v:Embedding_${profileId},v.episode_id=$episode,v.profile_id=$profile,
            v.input_revision=$revision,v.input_digest=$digest,v.vector=$vector,v.operation_id=$operation
          WITH v MATCH (m:Meta {key:'meta'})
          SET m.structure_revision=coalesce(m.structure_revision,0)+CASE WHEN v.operation_id=$operation THEN 1 ELSE 0 END`,
          { key, episode: request.episode_id, profile: profileId, revision: prior.input_revision, digest: prior.input_digest, vector, operation: request.operation_id });
      }
      await tx.run(`MATCH (a:EmbeddingAttempt {operation_id:$id}) SET a.body=$body,a.state=$state,a.reason=$reason`,
        { id: request.operation_id, body: canonicalJson(attempt), state: attempt.state, reason: attempt.reason });
      // A terminal outcome retires the Episode's live outbox entry in the same transaction, whoever attempted it: an
      // explicit quarantine leaves nothing for the worker. A deferral keeps the entry, and its retry budget, untouched.
      if (attempt.state !== "deferred") await tx.run(
        `MATCH (o:Outbox {element_id:$episode}) WHERE o.processed_at IS NULL SET o.processed_at=$now`,
        { episode: request.episode_id, now: new Date().toISOString() });
      return attempt;
    });
  }
  /** Returns quarantined Episodes of the configured profile to the outbox as fresh entries (retry budget reset);
   * their attempt rows stay for audit. Episodes that already hold a vector or an unprocessed entry are skipped. */
  async requeueQuarantinedEmbeddings(input: RpcEmbeddingRequeueParams, context: InstallationContext): Promise<RpcEmbeddingRequeueResult> {
    requireInstallation(context);
    const request = RpcEmbeddingRequeueParams.parse(input), provider = this.core.embeddingProvider;
    if (!provider) throw new RecallError("embedding_not_configured");
    const profileId = embeddingProfileId(provider.profile);
    return this.core.withWriteTx(async tx => {
      // Rows written before a.reason existed carry the reason only in the body; lift it once so the filter sees it.
      const legacy = await tx.run<{ id: string; body: string }>(
        `MATCH (a:EmbeddingAttempt {state:'quarantined'}) WHERE a.reason IS NULL RETURN a.operation_id AS id,a.body AS body`);
      if (legacy.records.length) await tx.run(`UNWIND $rows AS row MATCH (a:EmbeddingAttempt {operation_id:row.id}) SET a.reason=row.reason`,
        { rows: legacy.records.map(row => ({ id: row.get("id"), reason: RpcEmbeddingAttempt.parse(JSON.parse(row.get("body"))).reason })) });
      const rows = await tx.run<{ n: number }>(
        `MATCH (a:EmbeddingAttempt {profile_id:$profile,state:'quarantined'}) WHERE $reasons IS NULL OR a.reason IN $reasons
         WITH DISTINCT a.episode_id AS episode_id
         MATCH (e:Element:Episode {id:episode_id})
         WHERE NOT EXISTS { MATCH (v:EmbeddingVector {episode_id:episode_id,profile_id:$profile}) }
           AND NOT EXISTS { MATCH (o:Outbox {element_id:episode_id}) WHERE o.processed_at IS NULL }
         WITH e ORDER BY e.id LIMIT $limit
         CREATE (o:Outbox {element_id:e.id,enqueued_at:$now,processed_at:null})-[:OF]->(e)
         RETURN count(o) AS n`,
        { profile: profileId, reasons: request.reasons ?? null, limit: neo4j.int(request.limit), now: new Date().toISOString() });
      return { requeued: rows.records[0]!.get("n") };
    });
  }
  async embeddingStatus(operationId: string, context: InstallationContext): Promise<RpcEmbeddingAttempt | { state: "unknown"; operation_id: string }> {
    requireInstallation(context); const id = z.uuidv7().parse(operationId);
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      const rows = await tx.run<{ body: string }>(`MATCH (a:EmbeddingAttempt {operation_id:$id}) RETURN a.body AS body`, { id });
      if (!rows.records[0]) return { state: "unknown", operation_id: id };
      const attempt = RpcEmbeddingAttempt.parse(JSON.parse(rows.records[0].get("body")));
      await this.core.authorizeEpisodesTx(tx, [attempt.episode_id], policy);
      return attempt;
    });
  }
  async pending(limit = 100): Promise<string[]> {
    const rows = await this.core.run<{ id: string }>(
      `MATCH (o:Outbox) WHERE o.processed_at IS NULL
       RETURN o.element_id AS id ORDER BY id LIMIT $limit`,
      { limit: neo4j.int(limit) },
    );
    return rows.map((r) => r.id);
  }
  /** Unprocessed entries whose retry backoff has elapsed, never-deferred entries first so retries cannot starve fresh work. */
  private async dueOutbox(limit: number, now: number): Promise<{ id: string; deferrals: number }[]> {
    return this.core.run<{ id: string; deferrals: number }>(
      `MATCH (o:Outbox) WHERE o.processed_at IS NULL AND (o.retry_after IS NULL OR o.retry_after <= $now)
       RETURN o.element_id AS id, coalesce(o.deferrals, 0) AS deferrals ORDER BY deferrals, id LIMIT $limit`,
      { now: neo4j.int(now), limit: neo4j.int(limit) },
    );
  }
  private async deferOutbox(elementId: string, deferrals: number, retryAfter: number): Promise<void> {
    await this.core.withWriteTx((tx) => tx.run(
      `MATCH (o:Outbox {element_id:$id}) WHERE o.processed_at IS NULL SET o.deferrals=$deferrals, o.retry_after=$retry_after`,
      { id: elementId, deferrals: neo4j.int(deferrals), retry_after: neo4j.int(retryAfter) },
    ).then(() => undefined));
  }
  /** A transient provider failure defers the entry: it stays in the outbox with exponential backoff and a per-entry
   * budget of EMBEDDING_MAX_DEFERRALS; the transient failure after that quarantines it as provider_unavailable_exhausted.
   * Deterministic failures quarantine at once. Retries happen on later passes, never in a loop of their own.
   * A terminal attempt retires its entry itself (see attemptEmbedding). */
  async drainEmbeddingOutbox(limit = 100, context: InstallationContext = { principal: "installation", commit_mode: "auto" }):
    Promise<{ drained: number; quarantined: number; deferred: number; deferral_reason: string | null } | { drained: 0; reason: "embeddings_disabled" }> {
    const bounded = z.number().int().min(1).max(1000).parse(limit);
    if (!this.core.embeddingProvider) return { drained: 0, reason: "embeddings_disabled" };
    let drained = 0, quarantined = 0, deferred = 0;
    let deferralReason: string | null = null;
    for (const entry of await this.dueOutbox(bounded, this.core.clock())) {
      const attempt = await this.attemptEmbedding({ operation_id: uuidv7(), episode_id: z.uuidv7().parse(entry.id) }, context, entry.deferrals < EMBEDDING_MAX_DEFERRALS);
      if (attempt.state === "succeeded" || attempt.state === "quarantined") {
        drained++;
        if (attempt.state === "quarantined") quarantined++;
      } else {
        const deferrals = entry.deferrals + 1;
        await this.deferOutbox(entry.id, deferrals, this.core.clock() + embeddingRetryDelay(deferrals));
        deferred++; deferralReason = attempt.reason ?? attempt.state;
      }
    }
    return { drained, quarantined, deferred, deferral_reason: deferralReason };
  }
  async markProcessed(elementIds: string[]): Promise<void> {
    await this.core.withWriteTx((tx) => tx.run(
      `MATCH (o:Outbox) WHERE o.element_id IN $ids
       SET o.processed_at = $now`,
      { ids: elementIds, now: new Date().toISOString() },
    ).then(() => undefined));
  }
  async requeue(schema: string): Promise<number> {
    return this.core.withWriteTx(async (tx) => {
      const rows = await tx.run<{ n: number }>(
      `MATCH (e:Element { schema: $schema })
       CREATE (o:Outbox { element_id: e.id, enqueued_at: $now,
                          processed_at: null })-[:OF]->(e)
       RETURN count(o) AS n`,
      { schema, now: new Date().toISOString() },
      );
      const record = rows.records[0];
      if (!record) throw new Error("requeue count returned no result");
      return record.get("n");
    });
  }
}
