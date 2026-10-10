import assert from "node:assert/strict";
import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import { RpcEmbeddingRecoverParams, RpcEmbeddingAttempt, RpcEmbeddingRequeueParams, type RpcEmbeddingRequeueResult } from "@anamnesis/protocol";
import { EmbeddingError, EmbeddingProfile, embeddingProfileId, validateVector } from "../embedding.ts";
import { RecallError } from "../recall.ts";
import { tupleHash } from "./digest.ts";
import { ReceiptError } from "./receipts.ts";
import { type InstallationContext, type PolicyState, requireInstallation } from "./policy.ts";
import type { StoreCore } from "./core.ts";
import type { EmbeddingLedgerEntry } from "./embedding-ledger.ts";

const EMBEDDING_MAX_DEFERRALS = 8;
const embeddingRetryDelay = (deferrals: number): number => Math.min(30_000 * 2 ** (deferrals - 1), 3_600_000);
type VectorRow = {
  operation_id: string; fact_id: string; profile_id: string;
  input_revision: string; input_digest: string; model: string | null;
  model_incarnation: string | null; dimensions: number | null;
  created_at: number | null; completed_at: number | null;
};
const VECTOR_RETURN = `RETURN v.operation_id AS operation_id,v.fact_id AS fact_id,v.profile_id AS profile_id,
  v.input_revision AS input_revision,v.input_digest AS input_digest,v.model AS model,
  v.incarnation AS model_incarnation,v.dimensions AS dimensions,v.created_at AS created_at,v.completed_at AS completed_at`;
type FactInput = { content: string; revision: string; digest: string };
type ProviderOutcome = { vector: number[] | null; reason: RpcEmbeddingAttempt["reason"]; detail: string | null };

export class EmbeddingStore {
  private readonly profiles = new Map<string, EmbeddingProfile>();

  constructor(private readonly core: StoreCore) {}

  /** The profile a vector names, from the `EmbeddingProfile` row `init` persisted for it. Immutable per id, so cached. */
  private async vectorProfile(profileId: string): Promise<EmbeddingProfile> {
    const cached = this.profiles.get(profileId);
    if (cached) return cached;
    const rows = await this.core.run<{ body: string }>(`MATCH (p:EmbeddingProfile {id:$id}) RETURN p.body AS body`, { id: profileId });
    if (!rows[0]) throw new Error(`EmbeddingVector names profile ${profileId} but no EmbeddingProfile row carries it`);
    const profile = EmbeddingProfile.parse(JSON.parse(rows[0].body));
    this.profiles.set(profileId, profile);
    return profile;
  }

  /** Vectors written before the lane recorded model metadata on the row take it from their persisted profile. */
  private async vectorAttempt(row: VectorRow): Promise<RpcEmbeddingAttempt> {
    const profile = row.model === null || row.model_incarnation === null || row.dimensions === null
      ? await this.vectorProfile(row.profile_id) : null;
    return RpcEmbeddingAttempt.parse({
      operation_id: row.operation_id, fact_id: row.fact_id, profile_id: row.profile_id,
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
   * vector that carries the id. A succeeded Fact's earlier failed ids are forgotten with its entry. */
  private async priorAttempt(operationId: string): Promise<RpcEmbeddingAttempt | null> {
    const recorded = (await this.core.embeddingLedger.list()).flatMap(([, entry]) => entry.attempts)
      .find(attempt => attempt.operation_id === operationId);
    if (recorded) return recorded;
    const existing = await this.core.run<VectorRow>(`MATCH (v:EmbeddingVector {operation_id:$id}) WHERE v.fact_id IS NOT NULL ${VECTOR_RETURN}`, { id: operationId });
    return existing[0] ? await this.vectorAttempt(existing[0]) : null;
  }

  private async authorizeFactTx(tx: ManagedTransaction, factId: string, policy: PolicyState): Promise<void> {
    const sources = await tx.run<{ id: string }>(
      `MATCH (f:Fact {id:$id})-[:DERIVED_FROM]->(e:Element:Episode) RETURN DISTINCT e.id AS id`, { id: factId });
    if (!sources.records.length) throw new ReceiptError("invalid_selection");
    await this.core.authorizeEpisodesTx(tx, sources.records.map(row => row.get("id")), policy);
  }

  /** The Fact's input under the receipt lock; the returned revision pins the provider call. */
  private async factInputTx(tx: ManagedTransaction, factId: string, policy: PolicyState): Promise<FactInput> {
    await this.authorizeFactTx(tx, factId, policy);
    const rows = await tx.run<{ content: string; revision: string | null; digest: string | null }>(
      `MATCH (f:Element:Fact {id:$id}) RETURN f.content AS content,f.revision_key AS revision,f.digest AS digest`,
      { id: factId });
    const row = rows.records[0]?.toObject();
    if (!row) throw new ReceiptError("invalid_selection");
    assert(row.digest, `Fact ${factId} violates the storage contract: missing digest`);
    return { content: row.content, revision: row.revision ?? row.digest, digest: row.digest };
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
    const { prior, prepared } = await this.replayOrInput(request, profileId);
    if (prior) return prior;
    const previous = await this.core.embeddingLedger.get(request.fact_id);
    const held = await this.core.run<VectorRow>(
      `MATCH (v:EmbeddingVector {fact_id:$id,profile_id:$profile}) ${VECTOR_RETURN}`, { id: request.fact_id, profile: profileId });
    if (held[0]) {
      // The Fact already holds this profile's vector: the attempt that wrote it is the answer. The provider
      // is not called and the submitted id, having done no work, is recorded nowhere; a replay returns the same
      // attempt and the id's status stays unknown. A lingering entry of this profile is stale and goes with it;
      // another profile's entry is that profile's history and stays.
      if (previous?.profile_id === profileId) await this.core.embeddingLedger.delete(request.fact_id);
      return await this.vectorAttempt(held[0]);
    }
    // Another profile's entry lends no deferral budget: the first attempt under this profile replaces it.
    const deferrals = previous?.profile_id === profileId && previous.state === "deferred" ? previous.deferrals : 0;
    const createdAt = this.core.clock();
    const outcome = await this.embedInput(prepared.content);
    if (outcome.reason === "provider_unavailable" && !operator && deferrals >= EMBEDDING_MAX_DEFERRALS)
      outcome.reason = "provider_unavailable_exhausted";
    const { result, vectorPresent } = await this.commitAttempt(request, prepared, outcome, createdAt);
    if (result.state === "succeeded") {
      if (previous) await this.core.embeddingLedger.delete(request.fact_id);
    } else if (!vectorPresent) {
      // The ledger only tracks Facts without a vector: a failed operator retry of an embedded
      // Fact is reported to the caller but leaves no entry behind.
      await this.recordFailure(request.fact_id, result, previous?.profile_id === profileId ? previous : null, deferrals);
    }
    return result;
  }

  /** The recorded attempt an operation id replays, or the Fact's current input when the id is new. */
  private async replayOrInput(request: RpcEmbeddingRecoverParams, profileId: string):
    Promise<{ prior: RpcEmbeddingAttempt | null; prepared: FactInput }> {
    const prior = await this.priorAttempt(request.operation_id);
    if (prior && (prior.fact_id !== request.fact_id || prior.profile_id !== profileId))
      throw new ReceiptError("idempotency_conflict");
    const prepared = await this.core.withWriteTx(async tx => this.factInputTx(tx, request.fact_id, await this.core.receiptLockTx(tx)));
    if (prior && (prior.input_revision !== prepared.revision || prior.input_digest !== prepared.digest))
      throw new ReceiptError("idempotency_conflict");
    return { prior, prepared };
  }

  private recordFailure(factId: string, result: RpcEmbeddingAttempt, previous: EmbeddingLedgerEntry | null, deferrals: number): Promise<void> {
    const deferred = result.state === "deferred", count = deferred ? deferrals + 1 : deferrals;
    return this.core.embeddingLedger.set(factId, {
      profile_id: result.profile_id, state: deferred ? "deferred" : "quarantined", deferrals: count,
      retry_after: deferred ? this.core.clock() + embeddingRetryDelay(count) : null,
      attempts: [...(previous?.attempts ?? []), result].slice(-16),
    });
  }

  /** Re-checks the pinned input under the lock, then stores the vector; a concurrent winner's
   * attempt is returned instead of ours. `vectorPresent` says whether the Fact holds a vector afterwards. */
  private commitAttempt(request: RpcEmbeddingRecoverParams, prepared: FactInput, outcome: ProviderOutcome, createdAt: number):
    Promise<{ result: RpcEmbeddingAttempt; vectorPresent: boolean }> {
    const profile = this.core.embeddingProvider!.profile, profileId = embeddingProfileId(profile);
    return this.core.withWriteTx(async tx => {
      const policy = await this.core.receiptLockTx(tx);
      const current = await this.factInputTx(tx, request.fact_id, policy);
      const stale = current.revision !== prepared.revision || current.digest !== prepared.digest;
      const reason = stale ? "stale_input" : outcome.reason;
      const completedAt = this.core.clock();
      let winner: VectorRow | undefined, vectorPresent: boolean;
      if (reason === null && outcome.vector) {
        const merged = await tx.run<VectorRow>(`MERGE (v:EmbeddingVector {key:$key})
          ON CREATE SET v:Embedding_${profileId},v.fact_id=$fact,v.profile_id=$profile,
            v.input_revision=$revision,v.input_digest=$digest,v.vector=$vector,v.operation_id=$operation,
            v.model=$model,v.incarnation=$incarnation,v.dimensions=$dimensions,
            v.created_at=$created,v.completed_at=$completed
          ${VECTOR_RETURN}`,
          { key: tupleHash([request.fact_id, profileId]), fact: request.fact_id, profile: profileId,
            revision: prepared.revision, digest: prepared.digest, vector: outcome.vector, operation: request.operation_id,
            model: profile.model, incarnation: profile.model_incarnation, dimensions: neo4j.int(profile.dimensions),
            created: neo4j.int(createdAt), completed: neo4j.int(completedAt) });
        winner = merged.records[0]?.toObject(); vectorPresent = true;
        if (merged.summary.counters.updates().nodesCreated > 0)
          await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.structure_revision=coalesce(m.structure_revision,0)+1`);
      } else {
        const present = await tx.run<{ present: boolean }>(
          `RETURN EXISTS { MATCH (:EmbeddingVector {fact_id:$id,profile_id:$profile}) } AS present`,
          { id: request.fact_id, profile: profileId });
        vectorPresent = present.records[0]!.get("present");
      }
      const state = reason === null ? "succeeded" : reason === "provider_unavailable" ? "deferred" : "quarantined";
      const result = winner && winner.operation_id !== request.operation_id
        ? await this.vectorAttempt(winner)
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
        `RETURN EXISTS { MATCH (:EmbeddingVector {fact_id:$id,profile_id:$profile}) } AS present`,
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
      const rows = attempt ? null : await tx.run<VectorRow>(`MATCH (v:EmbeddingVector {operation_id:$id}) WHERE v.fact_id IS NOT NULL ${VECTOR_RETURN}`, { id });
      const found = attempt ?? (rows?.records[0] ? await this.vectorAttempt(rows.records[0].toObject()) : null);
      if (!found) return { state: "unknown" as const, operation_id: id };
      await this.authorizeFactTx(tx, found.fact_id, policy);
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
    // An entry whose Fact already holds the vector of the entry's own profile is stale: a process loss between
    // the vector commit and the entry's deletion, or a seeded ledger. Quarantined, it would hide the Fact from
    // the scan below and never be visited by `due`, so every drain first reconciles the ledger against the graph.
    const byProfile = new Map<string, string[]>();
    for (const [id, entry] of entries) {
      const ids = byProfile.get(entry.profile_id);
      if (ids) ids.push(id); else byProfile.set(entry.profile_id, [id]);
    }
    const stale = new Set<string>();
    for (const [profile, ids] of byProfile) {
      for (const row of await this.core.run<{ id: string }>(
        `UNWIND $ids AS id MATCH (:EmbeddingVector {fact_id:id,profile_id:$profile}) RETURN DISTINCT id`, { ids, profile }))
        stale.add(row.id);
    }
    for (const id of stale) await this.core.embeddingLedger.delete(id);
    // Only this profile's recorded failures hide a Fact from the scan; another profile's entry is
    // superseded by the first attempt under this one.
    const excluded = entries.flatMap(([id, entry]) => entry.profile_id === profileId && !stale.has(id) ? [id] : []);
    const fresh = await this.core.run<{ id: string }>(
      `MATCH (f:Element:Fact)
       WHERE NOT EXISTS { MATCH (:EmbeddingVector {fact_id: f.id, profile_id: $profile}) }
         AND NOT f.id IN $excluded
       RETURN f.id AS id ORDER BY f.id LIMIT $limit`,
      { profile: profileId, excluded, limit: neo4j.int(bounded) });
    const due = (await this.core.embeddingLedger.due(profileId, this.core.clock())).slice(0, bounded - fresh.length);
    let drained = 0, quarantined = 0, deferred = 0;
    let deferralReason: string | null = null;
    for (const id of [...fresh.map(row => row.id), ...due.map(([fact]) => fact)]) {
      const vector = await this.core.run<{ present: boolean }>(
        `RETURN EXISTS { MATCH (:EmbeddingVector {fact_id:$id,profile_id:$profile}) } AS present`,
        { id, profile: profileId });
      if (vector[0]?.present) {
        await this.core.embeddingLedger.delete(id);
        continue;
      }
      const attempt = await this.attemptEmbedding({ operation_id: uuidv7(), fact_id: z.uuidv7().parse(id) }, context, false);
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
