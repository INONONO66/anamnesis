import { planSchemaMigration } from "./schema-migrations.ts";
import { Driver } from "neo4j-driver";
import { MemoryElement, MemoryLink, ExtractionAttempt, Generation, ModelTask, Coverage, type MemoryElementInput, type MemoryLinkInput, type LinkRole } from "@anamnesis/protocol";
import { type RpcReportOutcomeParams, RpcPolicySetParams, RpcPolicyRevokeParams, type RpcPolicyResult } from "@anamnesis/protocol";
import { type SemanticSourceContext } from "@anamnesis/protocol";
import { CreateModelTask, ModelTaskCAS, LeaseModelTask, SettleModelTask, CompleteExtractionAttempt, AdvanceExtractionCoverage, SelectExtractionGeneration, ExtractionSelection, ReadExtractionCoverage, ExtractionCoverageRead, FactRelationJudgement, type ExtractionFailureDetail } from "@anamnesis/protocol";
import { ExtractionClaimContext, ExtractionPipeline, ExtractionDisposition, FactRelationContext } from "@anamnesis/protocol";
import { z } from "zod";
import { RpcEmbeddingRecoverParams, RpcEmbeddingAttempt, RpcEmbeddingRequeueParams, type RpcEmbeddingRequeueResult, RpcRecallParams, RpcRecallResult, type RpcProjectionParams, type RpcProjectionResult } from "@anamnesis/protocol";
import { embeddingProfileId } from "./embedding.ts";
import { type ConductingArcProbe, type GraphEnvelope, type ConductingArcVerification } from "./store/conducting.ts";
import { SCHEMA_STATEMENTS } from "./store/schema.ts";
import { canonicalJson } from "./store/digest.ts";
import type { HitCacheVerification, HitCacheRebuild, HitCache } from "./store/receipts.ts";
import { type InstallationContext } from "./store/policy.ts";
import { type ElementWriteOptions } from "./store/records.ts";
import { StoreCore, type StoreOptions } from "./store/core.ts";
import { ReceiptStore } from "./store/receipt-store.ts";
import { ConductingStore } from "./store/conducting-store.ts";
import { ElementStore } from "./store/element-store.ts";
import { EmbeddingStore, type EmbeddingDrain } from "./store/embedding-store.ts";
import { ExtractionStore } from "./store/extraction-store.ts";
import { MaterializationStore } from "./store/materialization-store.ts";
import { RecallStore } from "./store/recall-store.ts";
import { ProjectionStore } from "./store/projection-store.ts";
import { IntegrityStore } from "./store/integrity-store.ts";
import { EpisodeDerivedStore } from "./store/episode-derived.ts";
import { IdentityStore } from "./store/identity-store.ts";
import type { RpcSpacesImportParams, RpcSpacesListParams, RpcEpisodeRebuildParams } from "@anamnesis/protocol";
import { ActivationStore } from "./store/activation-store.ts";
import type { AuthoritySnapshot } from "./store/conducting-store.ts";
import type { PutResult } from "./store/element-store.ts";
import type { IntegrityIssue } from "./store/integrity-store.ts";
export { type ConductingArcProbe, type GraphEnvelope, type ConductingArcVerification } from "./store/conducting.ts";
export { luceneQuery, type HitCacheVerification, type HitCacheRebuild, type HitCache } from "./store/receipts.ts";
export { type InstallationContext } from "./store/policy.ts";
export type { AuthoritySnapshot } from "./store/conducting-store.ts";
export { GenerationReadinessError } from "./store/extraction-store.ts";
export type { StoreOptions } from "./store/core.ts";
export type { PutResult } from "./store/element-store.ts";
export type { IntegrityIssue } from "./store/integrity-store.ts";

export class Store {
  private readonly core: StoreCore;
  private readonly receipts: ReceiptStore;
  /** scripts/qa/graph-ordered-probe and graph-conducting-maintenance fixtures reach graphRawProbeTx through this field; renaming it breaks them. */
  private readonly conducting: ConductingStore;
  private readonly elements: ElementStore;
  private readonly embeddings: EmbeddingStore;
  private readonly extraction: ExtractionStore;
  private readonly materialization: MaterializationStore;
  private readonly recalls: RecallStore;
  private readonly projections: ProjectionStore;
  private readonly integrity: IntegrityStore;
  private readonly activation: ActivationStore;
  private readonly episodeDerived: EpisodeDerivedStore;
  readonly identity: IdentityStore;

  constructor(opts: StoreOptions, driver?: Driver) {
    this.core = new StoreCore(opts, driver);
    this.receipts = new ReceiptStore(this.core);
    this.conducting = new ConductingStore(this.core);
    this.episodeDerived = new EpisodeDerivedStore(this.core);
    this.elements = new ElementStore(this.core, this.receipts, this.conducting, this.episodeDerived);
    this.embeddings = new EmbeddingStore(this.core);
    this.extraction = new ExtractionStore(this.core, this.conducting);
    this.materialization = new MaterializationStore(this.core, this.receipts, this.extraction, this.elements);
    this.recalls = new RecallStore(this.core, this.extraction, this.receipts);
    this.projections = new ProjectionStore(this.core, this.extraction);
    this.integrity = new IntegrityStore(this.core, this.receipts, this.elements);
    this.activation = new ActivationStore(this.core, this.materialization, this.extraction, this.conducting);
    this.identity = new IdentityStore(this.core);
  }

  get databaseName(): string {
    return this.core.database;
  }

  get extractionJournal() {
    return this.core.extractionJournal;
  }

  get materializationState() {
    return this.core.materializationState;
  }

  claimWriterEpoch(): Promise<number> {
    return this.core.claimWriterEpoch();
  }

  private async refuseReceiptEraRows(): Promise<void> {
    const [boundary] = await this.core.run<{ legacy: boolean; hits: boolean }>(`RETURN
      EXISTS { MATCH (n:RecallReceipt|RecallTransport|RecallFeedback|RecallOutcome) } AS legacy,
      EXISTS { MATCH (h:Hit) WHERE NOT coalesce(
        h.kind = 'outcome' AND size(keys(h)) = 7 AND
        all(id IN [h.id, h.episode_id, h.usage_id] WHERE id =~ $uuid) AND
        h.idem_key =~ '[0-9a-f]{64}' AND
        h.t = toIntegerOrNull(h.t) AND 0 <= h.t <= 9007199254740991 AND
        -0.2 <= h.credit <= 0.2, false) } AS hits`, { uuid: "(?i)[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}" });
    if (boundary!.legacy) throw new Error("D62: legacy recall receipt found; readiness refused");
    if (boundary!.hits) throw new Error("D62: incompatible Hit found; readiness refused");
  }

  async init(): Promise<void> {
    // D62 boundary, checked once: a store recorded before schema 2 may hold receipt-era rows, which are refused,
    // never converted or dropped. The init transaction below records schema 2, so later starts skip the scan.
    const [meta] = await this.core.run<{ version: number | null }>(`MATCH (m:Meta {key:'meta'}) RETURN m.schema_version AS version`);
    if (meta && (meta.version ?? 0) < 2) await this.refuseReceiptEraRows();
    for (const stmt of SCHEMA_STATEMENTS) await this.core.run(stmt);

    if (this.core.embeddingProvider) {
      const profile = this.core.embeddingProvider.profile, id = embeddingProfileId(profile);
      await this.core.run(`MERGE (p:EmbeddingProfile {id:$id}) ON CREATE SET p.body=$body`, { id, body: canonicalJson(profile) });
      // A pre-D56 store carries this schema under vec_episode_<id>; IF NOT EXISTS would then
      // silently keep that name and recall would query a vec_fact_<id> that never exists.
      // The index is derived from the retained EmbeddingVector rows, so dropping it loses nothing.
      const legacy = await this.core.run<{ name: string; labels: string[]; properties: string[] }>(
        `SHOW VECTOR INDEXES YIELD name, labelsOrTypes AS labels, properties RETURN name, labels, properties`);
      for (const index of legacy) {
        if (index.name === `vec_fact_${id}` || !index.labels.includes(`Embedding_${id}`) || !index.properties.includes("vector")) continue;
        await this.core.run(`DROP INDEX \`${index.name.replaceAll("`", "``")}\` IF EXISTS`);
      }
      // Digest-derived identifiers only, never caller/model-name interpolation.
      await this.core.run(`CREATE VECTOR INDEX vec_fact_${id} IF NOT EXISTS FOR (v:Embedding_${id}) ON (v.vector)
        OPTIONS {indexConfig: {\`vector.dimensions\`: ${profile.dimensions}, \`vector.similarity_function\`: 'cosine'}}`);
    }
    await this.core.run(`CALL db.awaitIndexes(60)`);
    await this.core.withWriteTx(async (tx) => {
      const state = await tx.run<{ revision: number | null; format: string | null; events: number; legacy: number; elements: number; schema_version: number | null }>(
        `MERGE (m:Meta {key:'meta'}) ON CREATE SET m.ingest_seq=0, m.conducting_arc_ready=false
         SET m.ingest_seq=m.ingest_seq
         WITH m OPTIONAL MATCH (p:PolicyAuthority {key:'installation'})
         CALL () { MATCH (e:PolicyEvent) RETURN count(e) AS events }
         CALL () { MATCH (e:Element {schema:'anamnesis.memory-policy/1'}) RETURN count(e) AS legacy }
         CALL () { MATCH (e:Element) RETURN count(e) AS elements }
         RETURN m.policy_revision AS revision,p.format AS format,events,legacy,elements,m.schema_version AS schema_version`);
      const row = state.records[0]!;
      const migration = planSchemaMigration(row.get("schema_version"), row.get("elements") > 0);
      for (const step of migration.steps) for (const statement of step.statements) await tx.run(statement);
      if (row.get("schema_version") !== migration.target) await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.schema_version=toInteger($version)`, { version: migration.target });
      // Only a genuinely policy-empty database may bootstrap revision zero.
      // Missing/incompatible authority never overwrites an existing revision.
      const preserveLegacy = row.get("elements") > 0 && row.get("revision") === null && row.get("format") === null && row.get("events") === 0 && row.get("legacy") === 0;
      if (!preserveLegacy && row.get("revision") === null && row.get("format") === null && row.get("events") === 0 && row.get("legacy") === 0) {
        await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.policy_revision=0
          CREATE (:PolicyAuthority {key:'installation', format:'episode-source-v1'})`);
      }
      if (!preserveLegacy) {
        await tx.run(`MERGE (s:Meta {key:'extraction_selector'})
          ON CREATE SET s.selector_version=0`);
        // Startup verifies a capped retained-graph snapshot; it never reconstructs
        // endpoint rows. Existing incomplete/oversized stores need explicit repair.
        const snapshot = await this.conducting.conductingSnapshotTx(tx, 10000);
        if (!snapshot.report.truncated && snapshot.dataIssues.length === 0) {
          await this.conducting.publishConductingTx(tx, snapshot.partitions);
        } else {
          await this.conducting.invalidateConductingTx(tx);
        }
      }
    });
  }

  /** Retained provenance reader for lifecycle semantic adapters. No caller role,
   * lineage, source text or time replacement crosses this custody boundary. */
  semanticEpisode(sourceId: string, context: InstallationContext): Promise<Omit<SemanticSourceContext["episode"], "content_language">> {
    return this.receipts.semanticEpisode(sourceId, context);
  }

  setPolicy(input: RpcPolicySetParams, context: InstallationContext): Promise<RpcPolicyResult> {
    return this.receipts.setPolicy(input, context);
  }

  revokePolicy(input: RpcPolicyRevokeParams, context: InstallationContext): Promise<RpcPolicyResult> {
    return this.receipts.revokePolicy(input, context);
  }

  reportOutcome(input: RpcReportOutcomeParams, context: InstallationContext) {
    return this.receipts.reportOutcome(input, context);
  }

  getHitCache(episodeId: string): Promise<HitCache | null> {
    return this.receipts.getHitCache(episodeId);
  }

  verifyHitCache(): Promise<HitCacheVerification> {
    return this.receipts.verifyHitCache();
  }

  rebuildHitCache(): Promise<HitCacheRebuild> {
    return this.receipts.rebuildHitCache();
  }

  importRoomAssignments(input: RpcSpacesImportParams) {
    return this.episodeDerived.importAssignments(input);
  }

  listSpaces(input: RpcSpacesListParams) {
    return this.episodeDerived.listSpaces(input);
  }

  rebuildEpisodeDerived(input: RpcEpisodeRebuildParams, kind: "labels" | "replies") {
    return this.episodeDerived.rebuild(input, kind);
  }

  putElement(input: MemoryElementInput, opts?: ElementWriteOptions): Promise<PutResult> {
    return this.elements.putElement(input, opts);
  }

  /** Engine ingestion uses this after its derived input schema has parsed once. */
  putParsedElement(el: MemoryElement, opts?: ElementWriteOptions): Promise<PutResult> {
    return this.elements.putParsedElement(el, opts);
  }

  /** Only cache links are replaced. Legacy explicit parents were not persisted,
   * so rebuilding unmarked rows would invent provenance; require journal recovery.
   */
  rebuildTopology(): Promise<void> {
    return this.elements.rebuildTopology();
  }

  putLink(input: MemoryLinkInput): Promise<MemoryLink> {
    return this.elements.putLink(input);
  }

  /** Read-only diagnostic scan. It never repairs data or certifies publication;
   * use checkConductingArcs to fence detection and persist an unavailable gate. */
  verifyConductingArcs(options?: { maxItems?: number }): Promise<ConductingArcVerification> {
    return this.conducting.verifyConductingArcs(options);
  }

  checkConductingArcs(options?: { maxItems?: number }): Promise<ConductingArcVerification> {
    return this.conducting.checkConductingArcs(options);
  }

  /** Atomic, collection-capped operational rebuild; total scan work is not bounded.
   * UNAVAILABLE commits first: interruption, overflow and invalid authority leave
   * PPR closed. A subsequent explicit call can resume by reconstructing afresh. */
  rebuildConductingArcs(options?: { maxItems?: number }): Promise<ConductingArcVerification> {
    return this.conducting.rebuildConductingArcs(options);
  }

  /** Authenticated, writer-fenced per-collection digests without a size cap. */
  authoritySnapshot(context?: InstallationContext): Promise<AuthoritySnapshot> {
    return this.conducting.authoritySnapshot(context);
  }

  /** Bounded physical ConductingArc probe. Raw rows are ordered and capped before
   * any serving predicate; unavailable coverage is never treated as degree zero. */
  probeConductingArcs(sourceId: string, options?: { limit?: number }, context?: InstallationContext): Promise<ConductingArcProbe> {
    return this.conducting.probeConductingArcs(sourceId, options, context);
  }

  graphEnvelope(seedIds: string[], options?: { T?: number; maxNodes?: number; maxArcs?: number }, context?: InstallationContext): Promise<GraphEnvelope> {
    return this.conducting.graphEnvelope(seedIds, options, context);
  }

  getElement(id: string): Promise<MemoryElement | null> {
    return this.elements.getElement(id);
  }

  getPayload(hash: string): Promise<Uint8Array | null> {
    return this.elements.getPayload(hash);
  }

  /** Explicit operator retry; an unfinished operation is rediscovered from its missing vector. */
  recoverEmbedding(input: RpcEmbeddingRecoverParams, context: InstallationContext): Promise<RpcEmbeddingAttempt> {
    return this.embeddings.recoverEmbedding(input, context);
  }

  /** Remove quarantines so eligible Episodes are rediscovered by the vector scan. */
  requeueQuarantinedEmbeddings(input: RpcEmbeddingRequeueParams, context: InstallationContext): Promise<RpcEmbeddingRequeueResult> {
    return this.embeddings.requeueQuarantinedEmbeddings(input, context);
  }

  embeddingStatus(operationId: string, context: InstallationContext): Promise<RpcEmbeddingAttempt | { state: "unknown"; operation_id: string }> {
    return this.embeddings.embeddingStatus(operationId, context);
  }

  recall(input: z.input<typeof RpcRecallParams>, context: InstallationContext): Promise<RpcRecallResult> {
    return this.recalls.recall(input, context);
  }
  projection(input: RpcProjectionParams, context: InstallationContext): Promise<RpcProjectionResult> {
    return this.projections.snapshot(input, context);
  }

  linksOf(id: string, role?: LinkRole): Promise<MemoryLink[]> {
    return this.elements.linksOf(id, role);
  }

  isValidAt(id: string, at: string): Promise<boolean> {
    return this.elements.isValidAt(id, at);
  }

  /** Drain missing vectors and due deferrals within a bounded batch. */
  drainEmbeddingOutbox(limit?: number, context?: InstallationContext): Promise<EmbeddingDrain | { drained: 0; reason: "embeddings_disabled" }> {
    return this.embeddings.drainEmbeddingOutbox(limit, context);
  }

  /** The outbox's next Facts without a provider call; null when embeddings are not configured. */
  dueEmbeddings(limit?: number): Promise<string[] | null> {
    return this.embeddings.dueEmbeddings(limit);
  }

  embedOutbox(ids: readonly string[], context?: InstallationContext): Promise<EmbeddingDrain> {
    return this.embeddings.embedOutbox(ids, context);
  }

  verify(): Promise<IntegrityIssue[]> {
    return this.integrity.verify();
  }

  counts(): Promise<{
    elements: number;
    links: number;
    pending: number;
  }> {
    return this.integrity.counts();
  }

  createExtractionGeneration(input: Generation, context: InstallationContext): Promise<Generation> {
    return this.extraction.createExtractionGeneration(input, context);
  }

  getExtractionGeneration(id: string, context: InstallationContext): Promise<Generation> {
    return this.extraction.getExtractionGeneration(id, context);
  }

  createModelTask(input: CreateModelTask, context: InstallationContext): Promise<ModelTask> {
    return this.extraction.createModelTask(input, context);
  }

  getExtractionTask(id: string, context: InstallationContext): Promise<ModelTask> {
    return this.extraction.getExtractionTask(id, context);
  }

  getExtractionTaskByWorkKey(workKey: string, context: InstallationContext): Promise<ModelTask | null> {
    return this.extraction.getExtractionTaskByWorkKey(workKey, context);
  }

  getExtractionAttempt(id: string, context: InstallationContext): Promise<ExtractionAttempt> {
    return this.extraction.getExtractionAttempt(id, context);
  }

  /** Idempotent child admission under the same fence as the immutable parent.
   * The caller supplies no claim body, generation, source or model identity. */
  createExtractionJudgeTask(input: {claim_task_id:string}, context: InstallationContext): Promise<ModelTask> {
    return this.extraction.createExtractionJudgeTask(input, context);
  }

  readExtractionDecisions(attemptId: string, context: InstallationContext): Promise<ExtractionDisposition[]> {
    return this.extraction.readExtractionDecisions(attemptId, context);
  }

  readExtractionPipeline(id: string, context: InstallationContext): Promise<ExtractionPipeline> {
    return this.activation.readExtractionPipeline(id, context);
  }

  /** Seals a validated-but-unjudged source as a terminal omission (D53): the relation judge has failed on every
   * premise at least `min_failures` times, so the source's custody operation is written content-free with the
   * omission recorded, no Fact is written, and coverage/activation see custody like any refused source. Idempotent;
   * refuses while a verdict is still owed within budget or when the pipeline is not at the relation stage. */
  sealFactRelationOmission(request: { pipeline_id: string; min_failures: number }, context: InstallationContext): Promise<{ sealed: boolean; failures: number }> {
    return this.materialization.sealFactRelationOmission(request, context);
  }

  /** Relation premises of one pipeline that still owe a verdict (candidates present, none recorded). */
  pendingFactRelationInputs(pipelineId: string, context: InstallationContext): Promise<{ key: string; context: FactRelationContext }[]> {
    return this.materialization.pendingFactRelationInputs(pipelineId, context);
  }

  /** Highest recorded provider-failure count among the premises of one pipeline that still owe a verdict;
   * the scheduler seals a source whose relation judge keeps failing as a terminal omission (D53). */
  factRelationFailures(pipelineId: string, context: InstallationContext): Promise<number> {
    return this.materialization.factRelationFailures(pipelineId, context);
  }

  /** Records the provider's answer for one premise: a verdict bound to the premise
   * digest (written once), or the failure reason that keeps the pipeline pending. */
  recordFactRelationVerdict(input: { key: string } & ({ judgements: FactRelationJudgement[]; model: string; model_incarnation: string; reported_model?: string } | { failure: string; detail?: ExtractionFailureDetail }), context: InstallationContext): Promise<void> {
    return this.materialization.recordFactRelationVerdict(input, context);
  }

  /** `provider` is the identity of the provider about to run the task; the leased task adopts it so a daemon whose
   * provider changed between boots finishes inherited work instead of refusing it (#218). Absent, the task keeps its own. */
  leaseModelTask(input: LeaseModelTask & { provider?: Pick<ModelTask, "model" | "model_incarnation"> }, context: InstallationContext): Promise<ModelTask> {
    return this.extraction.leaseModelTask(input, context);
  }

  /** Source text leaves the database only after lease and current policy checks. */
  extractionTaskInput(taskId: string, leaseEpoch: string, context: InstallationContext): Promise<{ task: ModelTask; text: string; claim_context?: ExtractionClaimContext }> {
    return this.extraction.extractionTaskInput(taskId, leaseEpoch, context);
  }

  extractionContextInput(ids: string[], context: InstallationContext): Promise<{ id: string; text: string }[]> {
    return this.extraction.extractionContextInput(ids, context);
  }

  /** Exact completion replay returns the stored immutable outcome. Neither the
   * submitted output nor caller-selected policy context is ever an authority. */
  recordExtractionAttempt(input: CompleteExtractionAttempt, context: InstallationContext): Promise<ExtractionAttempt> {
    return this.extraction.recordExtractionAttempt(input, context);
  }

  cancelModelTask(input: Parameters<ExtractionStore["cancelModelTask"]>[0], context: InstallationContext): Promise<ModelTask> {
    return this.extraction.cancelModelTask(input, context);
  }

  settleModelTask(input: SettleModelTask, context: InstallationContext): Promise<ModelTask> {
    return this.extraction.settleModelTask(input, context);
  }

  retryModelTask(input: ModelTaskCAS, context: InstallationContext): Promise<ModelTask> {
    return this.extraction.retryModelTask(input, context);
  }

  recordExtractionCoverage(input: AdvanceExtractionCoverage, context: InstallationContext): Promise<Coverage> {
    return this.activation.recordExtractionCoverage(input, context);
  }

  readExtractionSelection(context: InstallationContext): Promise<ExtractionSelection> {
    return this.extraction.readExtractionSelection(context);
  }

  /** Audit coverage is necessary, never sufficient for derived activation.
   * No caller-supplied readiness flags can stand in for missing serving proofs. */
  cutoverExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext): Promise<Generation> {
    return this.activation.cutoverExtractionGeneration(input, context);
  }

  /** Rollback reopening is not activation. Fence it with the same server epoch
   * and advance that epoch atomically, so stale rollback retries cannot reopen
   * a target during a later selection era. */
  rollbackExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext): Promise<Generation> {
    return this.activation.rollbackExtractionGeneration(input, context);
  }

  /** Acquire a new pin or revalidate an old epoch under current policy and the
   * writer barrier. An unversioned legacy selector is never a valid reader pin. */
  readExtractionCoverage(input: ReadExtractionCoverage | string, context: InstallationContext): Promise<ExtractionCoverageRead> {
    return this.activation.readExtractionCoverage(input, context);
  }

  close(): Promise<void> {
    return this.core.close();
  }
}
