import neo4j, { type ManagedTransaction, type Record as Neo4jRecord } from "neo4j-driver";
import { ExtractionAttempt, Generation, ModelTask, Coverage } from "@anamnesis/protocol";
import { AdvanceExtractionCoverage, SelectExtractionGeneration, ExtractionSelection, ReadExtractionCoverage, ExtractionCoverageRead, canonicalExtractionBody, extractionBodyDigest } from "@anamnesis/protocol";
import { ExtractionPipeline, ExtractionAuditError } from "@anamnesis/protocol";
import { z } from "zod";
import { embeddingProfileId } from "../embedding.ts";
import { GraphAccessError } from "./conducting.ts";
import { receiptTime } from "./receipts.ts";
import { type InstallationContext, type PolicyState, requireInstallation } from "./policy.ts";
import type { StoreCore } from "./core.ts";
import { extractionEntryOmission, type MaterializationStore } from "./materialization-store.ts";
import type { ExtractionJournalEntry } from "./extraction-journal.ts";
import type { ExtractionStore } from "./extraction-store.ts";
import type { ConductingStore } from "./conducting-store.ts";
import { GenerationReadinessError } from "./extraction-store.ts";

/** An advance must start at the cursor the caller saw and move forward by at most 256. */
function checkCoverageAdvance(request: AdvanceExtractionCoverage, covered: number): void {
  if (covered !== request.expected_covered_ingest_seq) throw new Error("coverage_conflict");
  if (request.covered_ingest_seq < covered) throw new Error("coverage_regression");
  if (request.covered_ingest_seq - covered > 256) throw new Error("coverage_batch_too_large");
}

type CoveredRow = Neo4jRecord<{ source: string; seq: number }>;

/** The task and attempt a covered Episode row must carry: the expected sequence, a terminal task, and an attempt that is the task's own. */
function coveredRow(row: CoveredRow, seq: number, generationId: string, entry: ExtractionJournalEntry | undefined): { task: ModelTask; attempt: ExtractionAttempt } {
  if (row.get("seq") !== seq || !entry) throw new Error("coverage_hole");
  const { claim: task, claim_attempt: attempt } = entry;
  if (task.pipeline && (task.state === "queued" || task.state === "leased" || !attempt)) throw new ExtractionAuditError("extraction_audit_incomplete");
  if (!attempt) throw new Error("coverage_hole");
  if (task.state === "queued" || task.state === "leased" || attempt.state !== task.state || attempt.id !== task.attempt_id
    || attempt.task_id !== task.id || attempt.source_id !== row.get("source") || attempt.source_ingest_seq !== row.get("seq") || attempt.generation_id !== generationId) throw new Error("coverage_hole");
  return { task, attempt };
}

type CoveredSource = Neo4jRecord<{ id: string; seq: number; operations: number }>;

/** The existing access-index prerequisites must be online; this does not certify
 * nonexistent generation-scoped derived indexes or an ordered query plan. */
async function checkAccessIndexesTx(tx: ManagedTransaction): Promise<void> {
  const indexes = await tx.run(`SHOW INDEXES YIELD name,state WHERE name IN $names RETURN name,state`, {
    names: ["conducting_arc_source_link", "conducting_arc_coverage", "extraction_coverage_key", "extraction_generation_id", "meta_key", "fact_generation_id", "entity_generation_key"],
  });
  if (indexes.records.length !== 7 || indexes.records.some(index => index.get("state") !== "ONLINE")) throw new GraphAccessError("ordered_probe_unavailable");
}

/** Every DERIVED_FROM link of the generation names its Fact, source and link id, within the bounded view. */
async function derivedLinksBadTx(tx: ManagedTransaction, generation: string, maxDerived: number): Promise<boolean> {
  const links = await tx.run(`MATCH (f:Element:Fact)-[l:DERIVED_FROM]->(e:Element:Episode)
    WHERE l.generation=$generation RETURN f.id AS fact,e.id AS source,l.id AS link,l.generation AS generation LIMIT $limit`, { generation, limit: neo4j.int(maxDerived + 1) });
  return links.records.length > maxDerived || links.records.some(row => row.get("generation") !== generation || !row.get("fact") || !row.get("source") || !row.get("link"));
}

/** Several Facts may mention one Entity; the witness requirement is per distinct entity at the current policy revision. */
async function entityWitnessesBadTx(tx: ManagedTransaction, generation: string, policyRevision: number, maxDerived: number): Promise<boolean> {
  const entities = await tx.run(`MATCH (f:Element:Fact {generation:$generation}) UNWIND coalesce(f.entity_ids,[]) AS entity
    WITH DISTINCT entity OPTIONAL MATCH (w:EntityWitness {entity_id:entity,generation:$generation,policy_revision:$policy})
    RETURN entity,count(w) AS witnesses LIMIT $limit`, { generation, policy: neo4j.int(policyRevision), limit: neo4j.int(maxDerived + 1) });
  return entities.records.length > maxDerived || entities.records.some(row => row.get("witnesses") !== 1);
}

/** The selected embedding profile must be the only configured one and must carry a vector for every covered source. */
async function embeddingCoverageBadTx(tx: ManagedTransaction, profileId: string, sources: CoveredSource[]): Promise<boolean> {
  const configured = await tx.run(`MATCH (p:EmbeddingProfile) RETURN p.id AS id`);
  const vectors = await tx.run(`MATCH (v:EmbeddingVector {profile_id:$profile}) RETURN v.episode_id AS episode`, { profile: profileId });
  const vectorEpisodes = new Set(vectors.records.map(record => record.get("episode")));
  return configured.records.length !== 1 || configured.records[0]?.get("id") !== profileId || sources.some(source => !vectorEpisodes.has(source.get("id")));
}

export class ActivationStore {
  constructor(private readonly core: StoreCore, private readonly materialization: MaterializationStore, private readonly extraction: ExtractionStore, private readonly conducting: ConductingStore) {}

  async readExtractionPipeline(id: string, context: InstallationContext): Promise<ExtractionPipeline> {
    const result = await this.core.extractionTx(context,async(tx,policy)=>{
      const value = await this.materialization.readExtractionPipelineTx(tx,z.uuidv7().parse(id));
      if (value.state === 'known') await this.core.authorizeEpisodesTx(tx,[value.claim.source_id],policy);
      return value;
    });
    if (result.state === "known") await this.materialization.pruneExtractionJournal(context, { pipeline_id: id });
    return result;
  }
  /** Only immutable failed/cancelled attempts can justify a content-free omission.
   * A lost or expired lease is unresolved work, not an extraction outcome. */
  private extractionPipelineOmission(pipeline: ExtractionPipeline) {
    if (pipeline.state !== "known") return null;
    for (const [stage, task, attempt] of [
      ["claim", pipeline.claim, pipeline.claim_attempt],
      ["judge", pipeline.judge, pipeline.judge_attempt],
    ] as const) {
      if (stage === "judge" && pipeline.claim.state !== "succeeded") continue;
      if (task && attempt && (task.state === "failed" || task.state === "cancelled")
        && attempt.state === task.state && attempt.id === task.attempt_id && attempt.task_id === task.id) {
        return { pipeline_id: pipeline.pipeline_id, stage, attempt_id: attempt.id, state: attempt.state, reason: attempt.reason };
      }
    }
    return null;
  }
  /** Each explicit advance seals at most 256 terminal outcomes, including
   * content-free omissions. Retry is then forbidden for that sealed work.
   * The shared generation cursor is the minimum of the two partition cursors;
   * these are audit coverage partitions, not embedding/recall readiness gates. */
  /** A covered source's contribution to the omission digest: a pipeline's omission or its sealed decisions, a plain
   * attempt's terminal failure or refusal; a retained successful attempt leaves the digest as it was. */
  private async coverageOmissionTx(tx: ManagedTransaction, task: ModelTask, attempt: ExtractionAttempt, prior: string): Promise<string> {
    if (task.pipeline) {
      const pipeline = await this.materialization.readExtractionPipelineTx(tx,task.id);
      const omission = this.extractionPipelineOmission(pipeline);
      if (omission) return extractionBodyDigest({ prior, ...omission });
      if (pipeline.state !== 'known' || pipeline.claim.state !== 'succeeded' || pipeline.judge?.state !== 'succeeded') throw new ExtractionAuditError('extraction_audit_incomplete');
      // These rows seal audit work only, never materialization/embedding readiness.
      return extractionBodyDigest({prior,pipeline_id:task.id,judge_attempt_id:pipeline.judge_attempt!.id,decisions:pipeline.decisions.map(d=>d.disposition)});
    }
    if (attempt.state !== "succeeded" || !["retain", "correct"].includes(attempt.disposition ?? "")) return extractionBodyDigest({ prior, id: attempt.id, seq: attempt.source_ingest_seq, state: attempt.state, reason: attempt.reason, disposition: attempt.disposition });
    return prior;
  }
  async recordExtractionCoverage(input: AdvanceExtractionCoverage, context: InstallationContext): Promise<Coverage> {
    requireInstallation(context);
    const request = AdvanceExtractionCoverage.parse(input);
    const result = await this.core.extractionTx(context, async tx => {
      const generation = await this.core.writableExtractionGenerationTx(tx, request.generation_id);
      const key = `${generation.id}:${request.partition}`;
      const rows = await tx.run<{ body: string }>(`MATCH (c:ExtractionCoverage {key:$key}) RETURN c.body AS body`, { key });
      const prior = rows.records[0] ? Coverage.parse(JSON.parse(rows.records[0].get("body"))) : null;
      const covered = prior?.covered_ingest_seq ?? 0;
      checkCoverageAdvance(request, covered);
      const meta = await tx.run<{ seq: number }>(`MATCH (m:Meta {key:'meta'}) RETURN m.ingest_seq AS seq`);
      const required = receiptTime.parse(meta.records[0]?.get("seq"));
      if (request.covered_ingest_seq > required) throw new Error("coverage_exceeds_required");
      const prefix = await tx.run<{ source: string; seq: number }>(
        `MATCH (e:Element:Episode) WHERE e.ingest_seq > $from AND e.ingest_seq <= $to
         RETURN e.id AS source,e.ingest_seq AS seq ORDER BY seq LIMIT $limit`,
        { generation: generation.id, from: covered, to: request.covered_ingest_seq, limit: neo4j.int(256) });
      if (prefix.records.length !== request.covered_ingest_seq - covered) throw new Error("coverage_hole");
      let omissionDigest = prior?.omission_digest ?? extractionBodyDigest([]);
      for (const [index, row] of prefix.records.entries()) {
        const { task, attempt } = coveredRow(row, covered + index + 1, generation.id,
          await this.core.extractionJournal.byWorkKey(`${generation.id}:${row.get("source")}`));
        omissionDigest = await this.coverageOmissionTx(tx, task, attempt, omissionDigest);
      }
      const now = Math.max(generation.updated_at, prior?.updated_at ?? 0, receiptTime.parse(this.core.clock()));
      const value = Coverage.parse({ generation_id: generation.id, partition: request.partition, required_ingest_seq: required, covered_ingest_seq: request.covered_ingest_seq, omission_digest: omissionDigest, updated_at: now });
      await tx.run(`MERGE (c:ExtractionCoverage {key:$key}) SET c.generation_id=$generation,c.partition=$partition,c.covered_ingest_seq=$covered,c.body=$body`,
        { key, generation: generation.id, partition: request.partition, covered: value.covered_ingest_seq, body: canonicalExtractionBody(value) });
      const other = await tx.run<{ covered: number }>(`MATCH (c:ExtractionCoverage {key:$key}) RETURN c.covered_ingest_seq AS covered`, { key: `${generation.id}:${request.partition === "episodes" ? "active_extraction" : "episodes"}` });
      const cursor = Math.min(value.covered_ingest_seq, other.records[0]?.get("covered") ?? 0);
      if (cursor < generation.covered_ingest_seq) throw new Error("coverage_regression");
      const next = Generation.parse({ ...generation, covered_ingest_seq: cursor, updated_at: now });
      await tx.run(`MATCH (g:ExtractionGeneration {id:$id}) SET g.covered_ingest_seq=$covered,g.body=$body`, { id: generation.id, covered: cursor, body: canonicalExtractionBody(next) });
      return value;
    });
    // Prune only after commit: the minimum cursor seals BOTH audit partitions.
    await this.materialization.pruneExtractionJournal(context, { generation_id: request.generation_id });
    return result;
  }
  private async checkExtractionSelectionTx(tx: ManagedTransaction, request: SelectExtractionGeneration): Promise<ExtractionSelection> {
    const selection = await this.extraction.extractionSelectionTx(tx);
    if (selection.selector_version !== request.expected_selector_version) throw new GenerationReadinessError("selector_version_conflict");
    if (selection.generation_id !== request.expected_generation_id) throw new GenerationReadinessError("selector_conflict");
    return selection;
  }
  private async extractionCoverageTx(tx: ManagedTransaction, id: string): Promise<Coverage[]> {
    const rows = await tx.run(`UNWIND ['episodes','active_extraction'] AS partition
      MATCH (c:ExtractionCoverage {key:$id+':'+partition})
      RETURN partition,c.generation_id AS generation,c.covered_ingest_seq AS covered,c.body AS body ORDER BY partition`, { id });
    if (rows.records.length !== 2) throw new GenerationReadinessError("coverage_unavailable");
    return rows.records.map(row => {
      const value = Coverage.parse(JSON.parse(row.get("body")));
      if (value.partition !== row.get("partition") || value.generation_id !== id || row.get("generation") !== id
        || value.covered_ingest_seq !== row.get("covered")) throw new GenerationReadinessError("coverage_unavailable");
      return value;
    });
  }
  /** The target's coverage must be complete at the live ingest sequence: both partitions, the generation cursor, the
   * persisted cursor and the source watermark, with no model task still queued or leased. Returns the live sequence. */
  private async coverageCompleteTx(tx: ManagedTransaction, target: Generation): Promise<number> {
    const values = await this.extractionCoverageTx(tx, target.id);
    const meta = await tx.run(`MATCH (m:Meta {key:'meta'}) MATCH (g:ExtractionGeneration {id:$id})
      RETURN m.ingest_seq AS seq,g.source_high_watermark AS watermark,g.covered_ingest_seq AS covered`, { id: target.id });
    const row = meta.records[0]!, live = receiptTime.parse(row.get("seq"));
    const watermark = receiptTime.safeParse(row.get("watermark"));
    if (!watermark.success) throw new GenerationReadinessError("generation_watermark_unavailable");
    if ((await this.core.extractionJournal.pending(target.id)).length) throw new GenerationReadinessError("generation_work_in_flight");
    if (target.covered_ingest_seq !== live || row.get("covered") !== live || watermark.data > live
      || values.some(value => value.covered_ingest_seq !== live || value.required_ingest_seq !== live)) throw new GenerationReadinessError("coverage_incomplete");
    return live;
  }
  /** Conducting-arc coverage of the extraction stream must be complete and the retained snapshot clean. */
  private async conductingReadyTx(tx: ManagedTransaction, generation: string): Promise<void> {
    const conducting = await tx.run(`MATCH (m:Meta {key:'meta'})
      MATCH (c:ConductingArcCoverage {stream:'extraction',generation:$id})
      RETURN m.conducting_arc_ready AS ready,c.state AS state`, { id: generation });
    if (conducting.records[0]?.get("ready") !== true || conducting.records[0]?.get("state") !== "COMPLETE") throw new GraphAccessError("degree_probe_unavailable");
    const retained = await this.conducting.conductingSnapshotTx(tx, 10000);
    if (retained.report.truncated || retained.report.issues.length) throw new GraphAccessError("degree_probe_unavailable");
  }
  /** Every covered source carries materialization custody or an explicitly sealed omission, within the bounded custody view. */
  private async derivedCustodyBadTx(tx: ManagedTransaction, target: Generation, sources: CoveredSource[], maxDerived: number): Promise<boolean> {
    const operationRows = await tx.run(`MATCH (o:MaterializationOperation {generation:$generation})
      RETURN o.source_episode_id AS source,o.semantic_profile_id AS profile,o.fact_id AS fact,o.link_id AS link LIMIT $limit`, { generation: target.id, limit: neo4j.int(maxDerived + 1) });
    let missing = false;
    for (const source of sources) if (source.get("operations") === 0) {
      const entry = await this.core.extractionJournal.byWorkKey(`${target.id}:${source.get("id")}`);
      // Readiness invariant: sealing admits only terminal pipelines and success
      // writes custody in that transaction. A pruned covered source without
      // custody therefore denotes a sealed omission, not missing work.
      if (entry ? !extractionEntryOmission(entry) : source.get("seq") > target.covered_ingest_seq) missing = true;
    }
    const overflow = sources.length > 256 || operationRows.records.length > maxDerived;
    const malformed = operationRows.records.some(row => typeof row.get("source") !== "string" || typeof row.get("fact") !== "string" || typeof row.get("link") !== "string");
    return missing || overflow || malformed;
  }
  /** Serving readiness is derived only from persisted, generation-scoped
   * materialization custody. Audit success without an accepted proposal and
   * consumption remains insufficient. Every source in the covered prefix
   * must have materialization custody or an explicitly sealed omission, every
   * derived link and entity witness must be in order, and the selected
   * embedding profile must cover every source. Returns the unmet prerequisites. */
  private async activationCausesTx(tx: ManagedTransaction, target: Generation, policy: PolicyState, live: number): Promise<string[]> {
    const sources = await tx.run<{ id: string; seq: number; operations: number }>(`MATCH (e:Element:Episode) WHERE e.ingest_seq > 0 AND e.ingest_seq <= $seq
      OPTIONAL MATCH (o:MaterializationOperation {generation:$generation,source_episode_id:e.id})
      RETURN e.id AS id,e.ingest_seq AS seq,count(o) AS operations LIMIT 257`, { seq: live, generation: target.id });
    // The source partition is bounded at 256, but each source can retain up to
    // 64 claims. Derived custody rows use their own bounded overflow sentinel.
    const maxDerived = 256 * 64;
    const custodyBad = await this.derivedCustodyBadTx(tx, target, sources.records, maxDerived);
    const linkBad = await derivedLinksBadTx(tx, target.id, maxDerived);
    const witnessBad = await entityWitnessesBadTx(tx, target.id, policy.policy_revision, maxDerived);
    const embeddingCoverageBad = this.core.embeddingProvider ? await embeddingCoverageBadTx(tx, embeddingProfileId(this.core.embeddingProvider.profile), sources.records) : false;
    return [
      ...(embeddingCoverageBad ? ["selected_model_embedding_coverage"] : []),
      ...(custodyBad ? ["derived_authority_and_links"] : []),
      ...(witnessBad ? ["entity_witness_policy_coverage"] : []),
      ...(linkBad ? ["invalidation_mapping_equivalence"] : []),
    ];
  }
  /** Audit coverage is necessary, never sufficient for derived activation.
   * No caller-supplied readiness flags can stand in for missing serving proofs. */
  async cutoverExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext): Promise<Generation> {
    requireInstallation(context);
    const request = SelectExtractionGeneration.parse(input);
    return this.core.extractionTx(context, async (tx, policy) => {
      await this.checkExtractionSelectionTx(tx, request);
      const target = await this.core.extractionRecordTx(tx, "ExtractionGeneration", request.generation_id, Generation);
      if (target.state !== "catching_up" && target.state !== "active") throw new Error("generation_not_caught_up");
      const live = await this.coverageCompleteTx(tx, target);
      await this.conductingReadyTx(tx, target.id);
      await checkAccessIndexesTx(tx);
      const causes = await this.activationCausesTx(tx, target, policy, live);
      if (causes.length) throw new GenerationReadinessError("activation_prerequisite_unavailable", causes);
      const activated = Generation.parse({ ...target, state: "active", updated_at: Math.max(target.updated_at, this.core.clock()) });
      await tx.run(`MATCH (g:ExtractionGeneration {id:$id}), (s:Meta {key:'extraction_selector'})
        SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=s.selector_version+1`, { id: target.id, body: canonicalExtractionBody(activated) });
      await tx.run(`MERGE (r:DerivedServingReadiness {generation:$generation})
        SET r.state='COMPLETE',r.profile_id=$profile,r.policy_revision=$policy,r.covered_ingest_seq=$seq,r.link_revision=$revision`, { generation: target.id,
          profile: this.core.embeddingProvider ? embeddingProfileId(this.core.embeddingProvider.profile) : "embeddings-disabled-v1", policy: policy.policy_revision, seq: live, revision: live });
      return activated;
    });
  }
  /** Rollback reopening is not activation. Fence it with the same server epoch
   * and advance that epoch atomically, so stale rollback retries cannot reopen
   * a target during a later selection era. */
  async rollbackExtractionGeneration(input: SelectExtractionGeneration, context: InstallationContext): Promise<Generation> {
    requireInstallation(context);
    const request = SelectExtractionGeneration.parse(input);
    return this.core.extractionTx(context, async tx => {
      const selection = await this.checkExtractionSelectionTx(tx, request);
      const target = await this.core.extractionRecordTx(tx, "ExtractionGeneration", request.generation_id, Generation);
      if (target.state !== "retired") throw new Error("rollback_requires_retired");
      if (selection.selector_version === Number.MAX_SAFE_INTEGER) throw new GenerationReadinessError("selector_version_exhausted");
      const reopened = Generation.parse({ ...target, state: "catching_up", updated_at: Math.max(target.updated_at, this.core.clock()) });
      await tx.run(`MATCH (g:ExtractionGeneration {id:$id}), (m:Meta {key:'meta'}), (s:Meta {key:'extraction_selector'})
        SET g.body=$body,g.state='catching_up',g.source_high_watermark=m.ingest_seq,
          s.selector_version=s.selector_version+1`, { id: target.id, body: canonicalExtractionBody(reopened) });
      return reopened;
    });
  }
  /** Acquire a new pin or revalidate an old epoch under current policy and the
   * writer barrier. An unversioned legacy selector is never a valid reader pin. */
  async readExtractionCoverage(input: ReadExtractionCoverage | string, context: InstallationContext): Promise<ExtractionCoverageRead> {
    requireInstallation(context);
    const request = ReadExtractionCoverage.parse(typeof input === "string" ? { generation_id: input } : input);
    return this.core.extractionTx(context, async (tx, policy) => {
      const selection = await this.extraction.extractionSelectionTx(tx);
      if (request.expected_selector_version !== undefined && request.expected_selector_version !== selection.selector_version) throw new GenerationReadinessError("selector_version_conflict");
      if (selection.generation_id !== request.generation_id) throw new Error("generation_not_selected");
      const generation = await this.core.extractionRecordTx(tx, "ExtractionGeneration", request.generation_id, Generation);
      const values = await this.extractionCoverageTx(tx, generation.id);
      const required = Math.min(...values.map(value => value.required_ingest_seq));
      const covered = Math.min(...values.map(value => value.covered_ingest_seq));
      if (covered !== generation.covered_ingest_seq) throw new Error("coverage_stale");
      return ExtractionCoverageRead.parse({ generation_id: generation.id, selector_version: selection.selector_version,
        required_ingest_seq: required, covered_ingest_seq: covered,
        omission_digest: extractionBodyDigest(values.map(value => [value.partition, value.omission_digest])),
        policy_revision: policy.policy_revision, read_at: this.core.clock() });
    });
  }
}
