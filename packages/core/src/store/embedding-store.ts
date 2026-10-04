import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import { RpcEmbeddingRecoverParams, RpcEmbeddingAttempt, RpcEmbeddingRequeueParams, type RpcEmbeddingRequeueResult } from "@anamnesis/protocol";
import { EmbeddingError, embeddingProfileId, validateVector } from "../embedding.ts";
import { RecallError } from "../recall.ts";
import { tupleHash } from "./digest.ts";
import { ReceiptError } from "./receipts.ts";
import { type InstallationContext, requireInstallation } from "./policy.ts";
import type { StoreCore } from "./core.ts";
import type { EmbeddingLedgerEntry } from "./embedding-ledger.ts";

const EMBEDDING_MAX_DEFERRALS = 8;
const embeddingRetryDelay = (deferrals: number): number => Math.min(30_000 * 2 ** (deferrals - 1), 3_600_000);
type VectorRow = {
  operation_id: string; episode_id: string; profile_id: string;
  input_revision: string; input_digest: string; model: string | null;
  model_incarnation: string | null; dimensions: number | null;
  created_at: number | null; completed_at: number | null;
};
const VECTOR_RETURN = `RETURN v.operation_id AS operation_id,v.episode_id AS episode_id,v.profile_id AS profile_id,
  v.input_revision AS input_revision,v.input_digest AS input_digest,v.model AS model,
  v.model_incarnation AS model_incarnation,v.dimensions AS dimensions,v.created_at AS created_at,v.completed_at AS completed_at`;
type EpisodeInput = { content: string; revision: string; digest: string };
type ProviderOutcome = { vector: number[] | null; reason: RpcEmbeddingAttempt["reason"]; detail: string | null };

export class EmbeddingStore {
  constructor(private readonly core: StoreCore) {}

  private vectorAttempt(row: VectorRow): RpcEmbeddingAttempt {
    const profile = this.core.embeddingProvider?.profile;
    return RpcEmbeddingAttempt.parse({
      operation_id: row.operation_id, episode_id: row.episode_id, profile_id: row.profile_id,
      input_revision: row.input_revision, input_digest: row.input_digest,
      model: row.model ?? profile?.model, model_incarnation: row.model_incarnation ?? profile?.model_incarnation,
      dimensions: row.dimensions ?? profile?.dimensions,
      created_at: row.created_at ?? this.core.clock(), completed_at: row.completed_at ?? this.core.clock(),
      state: "succeeded", reason: null, detail: null,
    });
  }

  async recoverEmbedding(input: RpcEmbeddingRecoverParams, context: InstallationContext): Promise<RpcEmbeddingAttempt> {
    return this.attemptEmbedding(input, context, true);
  }

  /** The attempt an operation id already names: a deferred or quarantined ledger attempt, or the
   * vector that carries the id. A succeeded Episode's earlier failed ids are forgotten with its entry. */
  private async priorAttempt(operationId: string): Promise<RpcEmbeddingAttempt | null> {
    const recorded = (await this.core.embeddingLedger.list()).flatMap(([, entry]) => entry.attempts)
      .find(attempt => attempt.operation_id === operationId);
    if (recorded) return recorded;
    const existing = await this.core.run<VectorRow>(`MATCH (v:EmbeddingVector {operation_id:$id}) ${VECTOR_RETURN}`, { id: operationId });
    return existing[0] ? this.vectorAttempt(existing[0]) : null;
  }

  /** The Episode's input under the receipt lock; the returned revision pins the provider call. */
  private episodeInput(episodeId: string): Promise<EpisodeInput> {
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      await this.core.authorizeEpisodesTx(tx, [episodeId], policy);
      const rows = await tx.run<EpisodeInput>(
        `MATCH (e:Element:Episode {id:$id}) RETURN e.content AS content,e.revision_key AS revision,e.digest AS digest`,
        { id: episodeId });
      return rows.records[0]!.toObject();
    });
  }

  /** One provider call; every EmbeddingError becomes an attempt reason, anything else propagates. */
  private async embedInput(content: string): Promise<ProviderOutcome> {
    const provider = this.core.embeddingProvider!;
    try { return { vector: validateVector(await provider.embed(content, "document"), provider.profile), reason: null, detail: null }; }
    catch (error) {
      if (!(error instanceof EmbeddingError)) throw error;
      return { vector: null, reason: error.reason, detail: error.detail?.slice(0, 256) ?? null };
    }
  }

  /** Provider work is outside retryable graph transactions. Pending work has no
   * persisted row: a process loss leaves the missing vector discoverable. */
  private async attemptEmbedding(input: RpcEmbeddingRecoverParams, context: InstallationContext, operator: boolean): Promise<RpcEmbeddingAttempt> {
    requireInstallation(context);
    const request = RpcEmbeddingRecoverParams.parse(input), provider = this.core.embeddingProvider;
    if (!provider) throw new RecallError("embedding_not_configured");
    const profileId = embeddingProfileId(provider.profile);
    const prior = await this.priorAttempt(request.operation_id);
    if (prior && (prior.episode_id !== request.episode_id || prior.profile_id !== profileId))
      throw new ReceiptError("idempotency_conflict");
    const prepared = await this.episodeInput(request.episode_id);
    if (prior) {
      if (prior.input_revision !== prepared.revision || prior.input_digest !== prepared.digest)
        throw new ReceiptError("idempotency_conflict");
      return prior;
    }
    const previous = await this.core.embeddingLedger.get(request.episode_id);
    const deferrals = previous?.profile_id === profileId && previous.state === "deferred" ? previous.deferrals : 0;
    const outcome = await this.embedInput(prepared.content);
    if (outcome.reason === "provider_unavailable" && !operator && deferrals >= EMBEDDING_MAX_DEFERRALS)
      outcome.reason = "provider_unavailable_exhausted";
    const { result, vectorPresent } = await this.commitAttempt(request, prepared, outcome, this.core.clock());
    if (result.state === "succeeded") {
      if (previous) await this.core.embeddingLedger.delete(request.episode_id);
    } else if (!vectorPresent) {
      // The ledger only tracks Episodes without a vector: a failed operator retry of an embedded
      // Episode is reported to the caller but leaves no entry behind.
      await this.recordFailure(request.episode_id, result, previous?.profile_id === profileId ? previous : null, deferrals);
    }
    return result;
  }

  private recordFailure(episodeId: string, result: RpcEmbeddingAttempt, previous: EmbeddingLedgerEntry | null, deferrals: number): Promise<void> {
    const deferred = result.state === "deferred", count = deferred ? deferrals + 1 : deferrals;
    return this.core.embeddingLedger.set(episodeId, {
      profile_id: result.profile_id, state: deferred ? "deferred" : "quarantined", deferrals: count,
      retry_after: deferred ? this.core.clock() + embeddingRetryDelay(count) : null,
      attempts: [...(previous?.attempts ?? []), result].slice(-16),
    });
  }

  /** Re-checks the pinned input under the lock, then stores the vector; a concurrent winner's
   * attempt is returned instead of ours. `vectorPresent` says whether the Episode holds a vector afterwards. */
  private commitAttempt(request: RpcEmbeddingRecoverParams, prepared: EpisodeInput, outcome: ProviderOutcome, createdAt: number):
    Promise<{ result: RpcEmbeddingAttempt; vectorPresent: boolean }> {
    const profile = this.core.embeddingProvider!.profile, profileId = embeddingProfileId(profile);
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      await this.core.authorizeEpisodesTx(tx, [request.episode_id], policy);
      const rows = await tx.run<{ revision: string; digest: string }>(
        `MATCH (e:Element:Episode {id:$id}) RETURN e.revision_key AS revision,e.digest AS digest`,
        { id: request.episode_id });
      const stale = rows.records[0]!.get("revision") !== prepared.revision || rows.records[0]!.get("digest") !== prepared.digest;
      const reason = stale ? "stale_input" : outcome.reason;
      const completedAt = this.core.clock();
      let winner: VectorRow | undefined, vectorPresent: boolean;
      if (reason === null && outcome.vector) {
        const merged = await tx.run<VectorRow>(`MERGE (v:EmbeddingVector {key:$key})
          ON CREATE SET v:Embedding_${profileId},v.episode_id=$episode,v.profile_id=$profile,
            v.input_revision=$revision,v.input_digest=$digest,v.vector=$vector,v.operation_id=$operation,
            v.model=$model,v.model_incarnation=$incarnation,v.dimensions=$dimensions,
            v.created_at=$created,v.completed_at=$completed
          ${VECTOR_RETURN}`,
          { key: tupleHash([request.episode_id, profileId]), episode: request.episode_id, profile: profileId,
            revision: prepared.revision, digest: prepared.digest, vector: outcome.vector, operation: request.operation_id,
            model: profile.model, incarnation: profile.model_incarnation, dimensions: neo4j.int(profile.dimensions),
            created: neo4j.int(createdAt), completed: neo4j.int(completedAt) });
        winner = merged.records[0]?.toObject(); vectorPresent = true;
        if (merged.summary.counters.updates().nodesCreated > 0)
          await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.structure_revision=coalesce(m.structure_revision,0)+1`);
      } else {
        const present = await tx.run<{ present: boolean }>(
          `RETURN EXISTS { MATCH (:EmbeddingVector {episode_id:$id,profile_id:$profile}) } AS present`,
          { id: request.episode_id, profile: profileId });
        vectorPresent = present.records[0]!.get("present");
      }
      const state = reason === null ? "succeeded" : reason === "provider_unavailable" ? "deferred" : "quarantined";
      const result = winner && winner.operation_id !== request.operation_id
        ? this.vectorAttempt(winner)
        : RpcEmbeddingAttempt.parse({
          ...request, profile_id: profileId, model: profile.model, model_incarnation: profile.model_incarnation,
          dimensions: profile.dimensions, input_revision: prepared.revision, input_digest: prepared.digest,
          created_at: createdAt, completed_at: completedAt, state, reason, detail: outcome.detail,
        });
      return { result, vectorPresent };
    });
  }

  async requeueQuarantinedEmbeddings(input: RpcEmbeddingRequeueParams, context: InstallationContext): Promise<RpcEmbeddingRequeueResult> {
    requireInstallation(context);
    const request = RpcEmbeddingRequeueParams.parse(input), provider = this.core.embeddingProvider;
    if (!provider) throw new RecallError("embedding_not_configured");
    const profileId = embeddingProfileId(provider.profile);
    let requeued = 0;
    for (const [id] of await this.core.embeddingLedger.quarantined(profileId, request.reasons)) {
      const rows = await this.core.run<{ present: boolean }>(
        `RETURN EXISTS { MATCH (:EmbeddingVector {episode_id:$id,profile_id:$profile}) } AS present`,
        { id, profile: profileId });
      await this.core.embeddingLedger.delete(id);
      if (!rows[0]?.present && ++requeued >= request.limit) break;
    }
    return { requeued };
  }

  async embeddingStatus(operationId: string, context: InstallationContext): Promise<RpcEmbeddingAttempt | { state: "unknown"; operation_id: string }> {
    requireInstallation(context);
    const id = z.uuidv7().parse(operationId);
    const attempt = (await this.core.embeddingLedger.list()).flatMap(([, entry]) => entry.attempts)
      .find(item => item.operation_id === id);
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      const rows = attempt ? null : await tx.run<VectorRow>(`MATCH (v:EmbeddingVector {operation_id:$id}) ${VECTOR_RETURN}`, { id });
      const found = attempt ?? (rows?.records[0] ? this.vectorAttempt(rows.records[0].toObject()) : null);
      if (!found) return { state: "unknown" as const, operation_id: id };
      await this.core.authorizeEpisodesTx(tx, [found.episode_id], policy);
      return found;
    });
  }

  async drainEmbeddingOutbox(limit = 100, context: InstallationContext = { principal: "installation", commit_mode: "auto" }):
    Promise<{ drained: number; quarantined: number; deferred: number; deferral_reason: string | null } | { drained: 0; reason: "embeddings_disabled" }> {
    const bounded = z.number().int().min(1).max(1000).parse(limit);
    const provider = this.core.embeddingProvider;
    if (!provider) return { drained: 0, reason: "embeddings_disabled" };
    const profileId = embeddingProfileId(provider.profile);
    const entries = await this.core.embeddingLedger.list();
    const excluded = entries.map(([id]) => id);
    const fresh = await this.core.run<{ id: string }>(
      `MATCH (e:Element:Episode)
       WHERE NOT EXISTS { MATCH (:EmbeddingVector {episode_id: e.id, profile_id: $profile}) }
         AND NOT e.id IN $excluded
       RETURN e.id AS id ORDER BY e.id LIMIT $limit`,
      { profile: profileId, excluded, limit: neo4j.int(bounded) });
    const due = (await this.core.embeddingLedger.due(profileId, this.core.clock())).slice(0, bounded - fresh.length);
    let drained = 0, quarantined = 0, deferred = 0;
    let deferralReason: string | null = null;
    for (const id of [...fresh.map(row => row.id), ...due.map(([episode]) => episode)]) {
      const vector = await this.core.run<{ present: boolean }>(
        `RETURN EXISTS { MATCH (:EmbeddingVector {episode_id:$id,profile_id:$profile}) } AS present`,
        { id, profile: profileId });
      if (vector[0]?.present) {
        await this.core.embeddingLedger.delete(id);
        continue;
      }
      const attempt = await this.attemptEmbedding({ operation_id: uuidv7(), episode_id: z.uuidv7().parse(id) }, context, false);
      if (attempt.state === "deferred") {
        deferred++; deferralReason = attempt.reason;
      } else {
        drained++;
        if (attempt.state === "quarantined") quarantined++;
      }
    }
    return { drained, quarantined, deferred, deferral_reason: deferralReason };
  }
}
