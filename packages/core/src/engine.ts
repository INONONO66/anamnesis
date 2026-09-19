import { homedir } from "node:os";
import { join } from "node:path";
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import {
  MemoryElement,
  validateElementSemantics,
  type MemoryElementInput,
  type MemoryLink,
  type MemoryLinkInput,
} from "@anamnesis/protocol";
import {
  Store,
  type IntegrityIssue,
  type PutResult,
  type SearchHit,
  type StoreOptions,
  type IssueReceiptInput,
  type RecallReceipt,
  type RecallTransportInput,
  type CommitReceiptInput,
  type CommitReceiptResult,
  type ReceiptStatus,
  type HitCache,
  type HitCacheVerification,
  type HitCacheRebuild,
  type InstallationContext,
} from "./store.ts";

import { EmbeddingConfig, HttpEmbeddingProvider } from "./embedding.ts";
import { ExtractionProviderConfig, HttpExtractionProvider, ExtractionProviderError, validateModelOutput, validateSourceSpans, type ExtractionProvider } from "./extraction.ts";
import type { CreateModelTask, LeaseModelTask, CompleteExtractionAttempt, SelectExtractionGeneration, ReadExtractionCoverage, Generation, ExtractionCoverageRead } from "../../protocol/src/extraction.ts";
import { bindGenerationProfile, type GenerationProfileInput, type GenerationIdentityReceipt } from "../../protocol/src/generation-identity.ts";
import { CreateExtractionPipeline, RunExtractionPipeline, ExtractionAuditError } from '../../protocol/src/extraction-audit.ts';
import { MaterializeRetainedClaim, ProposeRetainedClaim, ReviewRetainedClaim, SemanticResolution, SemanticReviewOutput, type SemanticReviewProvider } from "../../protocol/src/materialization.ts";
import { validateSemanticClaim } from "../../protocol/src/semantic-claim.ts";
import type { RpcEmbeddingRecoverParams } from "../../protocol/src/rpc.ts";
import type { RpcPolicySetParams, RpcPolicyRevokeParams, RpcPolicyResult } from "../../protocol/src/rpc.ts";
import type { DreamLeidenAdapter } from "./dream-leiden-adapter.ts";

export const RememberInput = z
  .object(MemoryElement.shape)
  .omit({ id: true })
  .extend({
    schema: MemoryElement.shape.schema.default(
      "anamnesis.original-message/1",
    ),
    payload: z.instanceof(Uint8Array).optional(),
    payload_media_type: z.string().min(1).optional(),
    source_revision: z.string().min(1).optional(),
    /** Omit for the legacy append API; null pins a first revision. */
    expected_previous_revision_key: z.string().regex(/^[0-9a-f]{64}$/).nullable().optional(),
    /** The parent record takes precedence over inferred chronological order. */
    previous: z.string().min(1).optional(),
  })
  .strict()
  .superRefine(validateElementSemantics);
export type RememberInput = z.input<typeof RememberInput>;

export interface EngineOptions extends Partial<StoreOptions> { extractionProvider?: ExtractionProvider; semanticReviewProvider?: SemanticReviewProvider; dreamLeidenAdapter?: DreamLeidenAdapter }

/** A default password would silently ship an unauthenticated install. */
function requiredPassword(): string {
  const password = process.env["ANAMNESIS_NEO4J_PASSWORD"];
  if (password === undefined || password === "") {
    throw new Error(
      "ANAMNESIS_NEO4J_PASSWORD is required; generate one with `bun scripts/gen-password.ts`",
    );
  }
  return password;
}

export function envConfig(): StoreOptions & Pick<EngineOptions, "extractionProvider"> {
  const embedding = process.env["ANAMNESIS_EMBEDDING_CONFIG"];
  const extraction = process.env["ANAMNESIS_EXTRACTION_CONFIG"];
  return {
    ...(extraction ? { extractionProvider: new HttpExtractionProvider(ExtractionProviderConfig.parse(JSON.parse(extraction))) } : {}),
    ...(embedding ? { embeddingProvider: new HttpEmbeddingProvider(EmbeddingConfig.parse(JSON.parse(embedding))) } : {}),
    ...(process.env["ANAMNESIS_RECALL_DEFAULT_BYTES"] === undefined ? {} : { recallDefaultBytes: Number(process.env["ANAMNESIS_RECALL_DEFAULT_BYTES"]) }),
    uri: process.env["ANAMNESIS_NEO4J_URI"] ?? "bolt://127.0.0.1:7687",
    user: process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j",
    password: requiredPassword(),
    database: process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j",
    objectsRoot:
      process.env["ANAMNESIS_OBJECTS_ROOT"] ??
      join(homedir(), ".anamnesis", "objects"),
  };
}

export class Engine {
  readonly store: Store;
  private readonly extractionProvider: ExtractionProvider | undefined;
  private readonly semanticReviewProvider: SemanticReviewProvider | undefined;

  constructor(opts: EngineOptions = {}) {
    const { extractionProvider, semanticReviewProvider, dreamLeidenAdapter, ...storeOptions } = { ...envConfig(), ...opts };
    this.extractionProvider = extractionProvider;
    this.semanticReviewProvider = semanticReviewProvider;
    this.store = new Store({ ...storeOptions, ...(dreamLeidenAdapter ? { dreamLeidenAdapter } : {}) });
  }

  async materializeRetainedClaim(input: MaterializeRetainedClaim, context: InstallationContext) {
    return this.store.materializeRetainedClaim(input, context);
  }

  async proposeRetainedClaim(input: ProposeRetainedClaim, context: InstallationContext) {
    const provider = this.semanticReviewProvider;
    if (!provider) throw new Error("semantic_review_not_configured");
    const prepared = await this.store.prepareSemanticReview(input, provider.profileId, context);
    if (!prepared.created) return prepared.proposal!;
    try {
      const resolution = SemanticResolution.parse(await provider.resolve(prepared.premises));
      validateSemanticClaim(prepared.premises.request.semantic_claim, {
        generation: prepared.premises.request.generation_id, fact_language_policy: "source",
        entity_resolutions: resolution.entity_resolutions, attribution_speakers: resolution.attribution_speakers,
        allow_no_single_locus: resolution.allow_no_single_locus,
        episode: { ...prepared.premises.source, content_language: resolution.content_language },
      });
      const output = SemanticReviewOutput.parse(await provider.review({ premises: prepared.premises, resolution }));
      return await this.store.completeSemanticReview(input.proposal_id, resolution, output, context);
    } catch (error) {
      await this.store.failSemanticReview(input.proposal_id, error instanceof Error ? error.message : String(error), context);
      throw error;
    }
  }

  async reviewRetainedClaim(input: ReviewRetainedClaim, context: InstallationContext) {
    return this.store.reviewRetainedClaim(input, context);
  }

  async readExtractionSelection(context: InstallationContext) {
    return this.store.readExtractionSelection(context);
  }

  async cutoverExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext) {
    return this.store.cutoverExtractionGeneration(input, context);
  }

  /** Cutover boundary for ordered profiles: the receipt is bound to the
   * generation actually returned by the store, never to caller-supplied data. */
  async cutoverExtractionGenerationBound(input: SelectExtractionGeneration, profile: GenerationProfileInput, context: InstallationContext): Promise<{ generation: Generation; receipt: GenerationIdentityReceipt }> {
    const generation = await this.store.cutoverExtractionGeneration(input, context);
    const bound = bindGenerationProfile({ generation_id: generation.id, generation_version: generation.incarnation }, profile);
    return { generation, receipt: bound.receipt };
  }

  async rollbackExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext) {
    return this.store.rollbackExtractionGeneration(input, context);
  }

  async readExtractionCoverage(input: ReadExtractionCoverage, context: InstallationContext) {
    return this.store.readExtractionCoverage(input, context);
  }

  /** Coverage-reader boundary requiring an exact profile/coverage binding.
   * A missing or stale receipt cannot be manufactured from numeric ordering. */
  async readExtractionCoverageBound(input: ReadExtractionCoverage, profile: GenerationProfileInput, context: InstallationContext): Promise<{ coverage: ExtractionCoverageRead; receipt: GenerationIdentityReceipt }> {
    const coverage = await this.store.readExtractionCoverage(input, context);
    const generation = await this.store.getExtractionGeneration(coverage.generation_id, context);
    const bound = bindGenerationProfile({ generation_id: generation.id, generation_version: generation.incarnation }, profile);
    return { coverage, receipt: bound.receipt };
  }

  async createModelTask(input: CreateModelTask, context: InstallationContext) {
    return this.store.createModelTask(input, context);
  }

  async createExtractionPipeline(input: CreateExtractionPipeline, context: InstallationContext) {
    const request = CreateExtractionPipeline.parse(input), provider = this.extractionProvider;
    if (!provider) throw new ExtractionAuditError('extraction_not_configured');
    return this.store.createModelTask({...request,kind:'claim',pipeline:'claim-judge-audit-v1',model:provider.model,model_incarnation:provider.modelIncarnation},context);
  }

  /** Explicit, restartable audit pipeline. Terminal tasks are never re-called;
   * failed/leased tasks require the existing exact retry/settlement operations. */
  async runExtractionPipeline(input: RunExtractionPipeline, context: InstallationContext) {
    const request = RunExtractionPipeline.parse(input);
    const state = await this.store.readExtractionPipeline(request.task_id,context);
    if (state.state === 'unknown') return state;
    if (state.claim.state === 'queued') await this.runExtractionTask(request,context);
    const afterClaim = await this.store.readExtractionPipeline(request.task_id,context);
    if (afterClaim.state !== 'known' || afterClaim.claim.state !== 'succeeded') return afterClaim;
    const judge = await this.store.createExtractionJudgeTask({claim_task_id:request.task_id},context);
    if (judge.state === 'queued') await this.runExtractionTask({...request,task_id:judge.id,expected_version:judge.version},context);
    return this.store.readExtractionPipeline(request.task_id,context);
  }

  /** One explicitly requested model task, never an automatic graph/recall worker.
   * HTTP work stays outside retryable transactions; completion rechecks the lease,
   * immutable Episode bytes and installation policy under the writer barrier. */
  async runExtractionTask(input: LeaseModelTask, context: InstallationContext) {
    const provider = this.extractionProvider;
    if (!provider) throw new ExtractionAuditError('extraction_not_configured');
    const task = await this.store.leaseModelTask(input, context);
    const { text, claim_context } = await this.store.extractionTaskInput(task.id, task.lease!.epoch, context);
    let outcome: Pick<CompleteExtractionAttempt, "state" | "reason" | "output" | "spans" | "disposition">;
    try {
      if (provider.model !== task.model || provider.modelIncarnation !== task.model_incarnation) throw new ExtractionProviderError("provider_mismatch");
      if (Buffer.byteLength(text, "utf8") > 65536) throw new ExtractionProviderError("input_too_large");
      const validated = validateModelOutput(await provider.extract({ text, task: task.kind, ...(claim_context ? {claim_context} : {}) }), task.kind);
      try { validateSourceSpans(text, validated.spans); }
      catch (error) { if (error instanceof Error && error.message === "span_mismatch") throw new ExtractionProviderError("provider_mismatch"); throw error; }
      outcome = { ...validated, state: "succeeded", reason: null };
    } catch (error) {
      if (!(error instanceof ExtractionProviderError)) throw error;
      outcome = { state: "failed", reason: error.reason, output: null, spans: [], disposition: null };
    }
    return this.store.recordExtractionAttempt({ ...outcome, id: task.attempt_id!, task_id: task.id, expected_version: task.version, lease_epoch: task.lease!.epoch }, context);
  }

  async init(): Promise<void> {
    await this.store.init();
  }

  async claimWriterEpoch(): Promise<number> {
    return this.store.claimWriterEpoch();
  }

  async verifyConductingArcs(options: { maxItems?: number } = {}) {
    return this.store.verifyConductingArcs(options);
  }

  async checkConductingArcs(options: { maxItems?: number } = {}) {
    return this.store.checkConductingArcs(options);
  }

  async rebuildConductingArcs(options: { maxItems?: number } = {}) {
    return this.store.rebuildConductingArcs(options);
  }

  async graphEnvelope(seedIds: string[], options: { T?: number; maxNodes?: number; maxArcs?: number }, context: InstallationContext) {
    return this.store.graphEnvelope(seedIds, options, context);
  }

  async recoverEmbedding(input: RpcEmbeddingRecoverParams, context: InstallationContext) {
    return this.store.recoverEmbedding(input, context);
  }

  async drainEmbeddingOutbox(limit = 100) {
    return this.store.drainEmbeddingOutbox(limit);
  }

  async embeddingStatus(operationId: string, context: InstallationContext) {
    return this.store.embeddingStatus(operationId, context);
  }

  async recallHybrid(input: z.input<typeof import("../../protocol/src/rpc.ts").RpcRecallParams>, context: InstallationContext) {
    return this.store.recall(input, context);
  }

  async issueReceipt(input: IssueReceiptInput, context: InstallationContext): Promise<RecallReceipt> {
    return this.store.issueReceipt(input, context);
  }

  async recordRecallTransport(input: RecallTransportInput, context: InstallationContext) {
    return this.store.recordRecallTransport(input, context);
  }

  async exposeRecall(recallId: string, context: InstallationContext) {
    return this.store.exposeRecall(recallId, context);
  }

  async commitReceipt(input: CommitReceiptInput, context: InstallationContext): Promise<CommitReceiptResult> {
    return this.store.commitReceipt(input, context);
  }

  async setPolicy(input: RpcPolicySetParams, context: InstallationContext): Promise<RpcPolicyResult> {
    return this.store.setPolicy(input, context);
  }

  async revokePolicy(input: RpcPolicyRevokeParams, context: InstallationContext): Promise<RpcPolicyResult> {
    return this.store.revokePolicy(input, context);
  }

  async getReceipt(recallId: string): Promise<RecallReceipt | null> {
    return this.store.getReceipt(recallId);
  }

  async getReceiptStatus(operationId: string): Promise<ReceiptStatus> {
    return this.store.getReceiptStatus(operationId);
  }

  async getHitCache(episodeId: string): Promise<HitCache | null> {
    return this.store.getHitCache(episodeId);
  }

  async verifyHitCache(): Promise<HitCacheVerification> {
    return this.store.verifyHitCache();
  }

  async rebuildHitCache(): Promise<HitCacheRebuild> {
    return this.store.rebuildHitCache();
  }

  async remember(input: RememberInput, admission?: { metadata: unknown; context: InstallationContext }): Promise<PutResult> {
    const rec = RememberInput.parse(input);
    const {
      payload,
      payload_media_type: payloadMediaType,
      source_revision: sourceRevision,
      expected_previous_revision_key: expectedPreviousRevisionKey,
      previous,
      ...fields
    } = rec;
    const element: MemoryElement = { ...fields, id: uuidv7() };
    return this.store.putParsedElement(element, {
      ...(payload ? { payload } : {}),
      ...(payloadMediaType ? { payloadMediaType } : {}),
      sourceRevision: sourceRevision ?? element.origin.record,
      ...(expectedPreviousRevisionKey !== undefined ? { expectedPreviousRevisionKey } : {}),
      ...(previous ? { previous } : {}),
      enqueue: true,
      ...(admission ? { admission } : {}),
    });
  }

  /** Failed handlers leave their whole fetched batch pending for retry. */
  async digest(
    handler: (episode: MemoryElement, store: Store) => Promise<void> | void,
    batchSize = 200,
  ): Promise<number> {
    const batch = await this.store.pending(batchSize);
    if (batch.length === 0) return 0;
    for (const id of batch) {
      const episode = await this.store.getElement(id);
      if (episode) await handler(episode, this.store);
    }
    await this.store.markProcessed(batch);
    return batch.length + (await this.digest(handler, batchSize));
  }

  async recall(
    query: string,
    opts: { limit?: number; at?: string } = {},
  ): Promise<SearchHit[]> {
    return this.store.searchText(query, {
      limit: opts.limit ?? 10,
      until: opts.at ?? new Date().toISOString(),
      validOnly: true,
    });
  }

  async put(element: MemoryElementInput): Promise<PutResult> {
    return this.store.putElement(element);
  }

  async link(link: MemoryLinkInput): Promise<MemoryLink> {
    return this.store.putLink(link);
  }

  /** Re-extraction rewinds only the mutable cursor, never memory data. */
  async requeueEpisodes(
    schema = "anamnesis.original-message/1",
  ): Promise<number> {
    return this.store.requeue(schema);
  }

  async verify(): Promise<IntegrityIssue[]> {
    return this.store.verify();
  }

  async status(): Promise<{
    elements: number;
    links: number;
    pendingOutbox: number;
  }> {
    const c = await this.store.counts();
    return { elements: c.elements, links: c.links, pendingOutbox: c.pending };
  }

  async close(): Promise<void> {
    await this.store.close();
  }
}
