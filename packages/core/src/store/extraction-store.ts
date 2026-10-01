import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { ExtractionAttempt, Generation, ModelTask } from "@anamnesis/protocol";
import { CreateModelTask, ModelTaskCAS, LeaseModelTask, SettleModelTask, CompleteExtractionAttempt, ExtractionSelection, canonicalExtractionBody, extractionBodyDigest, type ExtractionFailureDetail } from "@anamnesis/protocol";
import { validateModelOutput, validateSourceSpans } from "../extraction.ts";
import { ExtractionClaimContext, ExtractionJudgeInput, ExtractionDisposition, ExtractionAuditError } from "@anamnesis/protocol";
import { ExtractionModelOutput } from "@anamnesis/protocol";
import { z } from "zod";
import { sha256 } from "./digest.ts";
import { receiptTime, receiptHash, ReceiptError } from "./receipts.ts";
import { type InstallationContext, type PolicyState, requireInstallation } from "./policy.ts";
import type { StoreCore } from "./core.ts";
import type { ConductingStore } from "./conducting-store.ts";

/** Internal lease request: the engine names the provider that will run the task (never an RPC caller). */
const LeaseModelTaskWithProvider = LeaseModelTask.extend({ provider: z.strictObject({ model: ModelTask.shape.model, model_incarnation: ModelTask.shape.model_incarnation }).optional() });

export class GenerationReadinessError extends Error {
  constructor(readonly code: "coverage_unavailable" | "coverage_incomplete" | "generation_work_in_flight" | "generation_watermark_unavailable"
    | "selector_unavailable" | "selector_conflict" | "selector_version_conflict" | "selector_version_exhausted"
    | "generation_not_active" | "activation_prerequisite_unavailable", readonly prerequisites: readonly string[] = []) {
    super(`${code}${prerequisites.length ? `: ${prerequisites.join(",")}` : ""}`);
    this.name = "GenerationReadinessError";
  }
}

/** Submitted output must be what the model-output validator accepts, with the same disposition and spans, and its spans must lie in the source. */
function checkAttemptOutput(request: CompleteExtractionAttempt, task: ModelTask, source: { content: string }): void {
  if (!request.output) return;
  const validated = validateModelOutput(JSON.parse(request.output.canonical_body), task.kind);
  if (canonicalExtractionBody(validated.output) !== canonicalExtractionBody(request.output) || validated.disposition !== request.disposition
    || canonicalExtractionBody(request.spans) !== canonicalExtractionBody(validated.spans)) throw new Error("output_mismatch");
  validateSourceSpans(source.content, request.spans);
}

export class ExtractionStore {
  constructor(private readonly core: StoreCore, private readonly conducting: ConductingStore) {}

  private async extractionSourceTx(tx: ManagedTransaction, sourceId: string) {
    const rows = await tx.run<{ content: string; revision: string; seq: number }>(
      `MATCH (e:Element:Episode {id:$id}) RETURN e.content AS content,e.revision_key AS revision,e.ingest_seq AS seq`, { id: sourceId });
    const row = rows.records[0];
    if (!row) throw new Error("unknown_source");
    const content = z.string().parse(row.get("content"));
    return { content, source_revision: receiptHash.parse(row.get("revision")), source_ingest_seq: receiptTime.positive().parse(row.get("seq")), body_digest: sha256(Buffer.from(content, "utf8")) };
  }
  private async validateExtractionSourceTx(tx: ManagedTransaction, task: ModelTask) {
    const source = await this.extractionSourceTx(tx, task.source_id);
    if (source.source_revision !== task.source_revision || source.body_digest !== task.body_digest || source.source_ingest_seq !== task.source_ingest_seq) throw new Error("stale_input");
    return source;
  }
  async createExtractionGeneration(input: Generation, context: InstallationContext): Promise<Generation> {
    requireInstallation(context);
    const value = Generation.parse(input), body = canonicalExtractionBody(value);
    return this.core.extractionTx(context, async tx => {
      const rows = await tx.run<{ body: string; creation: string }>(`MATCH (g:ExtractionGeneration {id:$id}) RETURN g.body AS body,g.creation_body AS creation`, { id: value.id });
      const old = rows.records[0];
      if (old) {
        if (old.get("creation") !== body) throw new Error("generation_conflict");
        return Generation.parse(JSON.parse(old.get("body")));
      }
      if (value.covered_ingest_seq !== 0 || !["active", "catching_up"].includes(value.state)) throw new Error("invalid_generation_initial_state");
      await tx.run(`MATCH (m:Meta {key:'meta'})
        CREATE (:ExtractionGeneration {id:$id,body:$body,creation_body:$body,state:$state,
          covered_ingest_seq:0,source_high_watermark:m.ingest_seq})`, { id: value.id, body, state: value.state });
      const retained = await tx.run(`MATCH ()-[l:MENTIONS|RELATES_TO|DERIVED_FROM]->() WHERE l.generation=$id RETURN l.id LIMIT 1`, {id:value.id});
      await tx.run(`MERGE (c:ConductingArcCoverage {stream:'extraction',generation:$id})
        SET c.state=$state`, {id:value.id,state:retained.records.length ? "UNAVAILABLE" : "COMPLETE"});
      if (retained.records.length) await this.conducting.invalidateConductingTx(tx);
      await this.conducting.conductingRevisionTx(tx);
      return value;
    });
  }
  async getExtractionGeneration(id: string, context: InstallationContext): Promise<Generation> {
    return this.core.extractionTx(context, tx => this.core.extractionRecordTx(tx, "ExtractionGeneration", z.uuidv7().parse(id), Generation));
  }
  async createModelTask(input: CreateModelTask, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = CreateModelTask.parse(input), digest = extractionBodyDigest(request);
    return this.core.extractionTx(context, async (tx, policy) => {
      await this.core.writableExtractionGenerationTx(tx, request.generation_id);
      await this.core.authorizeEpisodesTx(tx, [request.source_id], policy);
      const source = await this.extractionSourceTx(tx, request.source_id);
      const oldRows = await tx.run<{ body: string; digest: string }>(`MATCH (t:ModelTask {id:$id}) RETURN t.body AS body,t.creation_digest AS digest`, { id: request.id });
      const old = oldRows.records[0];
      if (old) {
        if (old.get("digest") !== digest) throw new Error("task_conflict");
        const task = ModelTask.parse(JSON.parse(old.get("body")));
        await this.validateExtractionSourceTx(tx, task);
        return task;
      }
      const workKey = `${request.generation_id}:${request.source_id}`;
      const work = await tx.run(`MATCH (t:ModelTask {work_key:$key}) RETURN t.id AS id`, { key: workKey });
      if (work.records.length) throw new Error("task_conflict");
      const now = receiptTime.parse(this.core.clock());
      const task = ModelTask.parse({ ...request, source_revision: source.source_revision, source_ingest_seq: source.source_ingest_seq, body_digest: source.body_digest,
        attempt_id: null, state: "queued", lease: null, policy_context: null, attempts: 0, version: 0, created_at: now, updated_at: now });
      await this.extractionNotCoveredTx(tx, task);
      await tx.run(`CREATE (:ModelTask {id:$id,work_key:$key,generation_id:$generation,source_id:$source,source_ingest_seq:$seq,creation_digest:$digest,body:$body,state:'queued'})`,
        { id: task.id, key: workKey, generation: task.generation_id, source: task.source_id, seq: task.source_ingest_seq, digest, body: canonicalExtractionBody(task) });
      if (task.pipeline) await tx.run(`CREATE (:ExtractionPipeline {id:$id,generation_id:$generation,source_id:$source,source_ingest_seq:$seq})`,
        {id:task.id,generation:task.generation_id,source:task.source_id,seq:task.source_ingest_seq});
      return task;
    });
  }
  async getExtractionTask(id: string, context: InstallationContext): Promise<ModelTask> {
    return this.core.extractionTx(context, async (tx, policy) => {
      const task = await this.core.extractionRecordTx(tx, "ModelTask", z.uuidv7().parse(id), ModelTask);
      await this.core.authorizeEpisodesTx(tx, [task.source_id], policy);
      return task;
    });
  }
  async getExtractionAttempt(id: string, context: InstallationContext): Promise<ExtractionAttempt> {
    return this.core.extractionTx(context, async (tx, policy) => {
      const attempt = await this.core.extractionRecordTx(tx, "ExtractionAttempt", z.uuidv7().parse(id), ExtractionAttempt);
      await this.core.authorizeEpisodesTx(tx, [attempt.source_id], policy);
      return attempt;
    });
  }
  async extractionHeadTx(tx: ManagedTransaction, sourceId: string): Promise<string> {
    const rows = await tx.run(`MATCH (e:Element:Episode {id:$id}) MATCH (h:OriginHead {origin_key:e.origin_key}) RETURN h.revision_key AS head`, {id:sourceId});
    return receiptHash.parse(rows.records[0]?.get('head'));
  }
  private async extractionClaimContextTx(tx: ManagedTransaction, id: string): Promise<ExtractionClaimContext> {
    const task = await this.core.extractionRecordTx(tx,'ModelTask',id,ModelTask);
    if (task.pipeline !== 'claim-judge-audit-v1' || task.kind !== 'claim' || task.state !== 'succeeded' || !task.attempt_id) throw new ExtractionAuditError('extraction_audit_incomplete');
    const attempt = await this.core.extractionRecordTx(tx,'ExtractionAttempt',task.attempt_id,ExtractionAttempt);
    if (attempt.state !== 'succeeded' || !attempt.output || attempt.task_id !== task.id || attempt.generation_id !== task.generation_id
      || attempt.source_id !== task.source_id || attempt.source_revision !== task.source_revision || attempt.body_digest !== task.body_digest) throw new ExtractionAuditError('extraction_audit_conflict');
    const body = ExtractionModelOutput.parse(JSON.parse(attempt.output.canonical_body));
    if (body.task !== 'claim') throw new ExtractionAuditError('extraction_audit_conflict');
    await this.validateExtractionSourceTx(tx,task);
    return ExtractionClaimContext.parse({task_id:task.id,attempt_id:attempt.id,body_digest:attempt.output.body_digest,claims:body.claims});
  }
  /** Idempotent child admission under the same fence as the immutable parent.
   * The caller supplies no claim body, generation, source or model identity. */
  async createExtractionJudgeTask(input: {claim_task_id:string}, context: InstallationContext): Promise<ModelTask> {
    const request = z.strictObject({claim_task_id:z.uuidv7()}).parse(input);
    return this.core.extractionTx(context,async(tx,policy)=>{
      const task = await this.core.extractionRecordTx(tx,'ModelTask',request.claim_task_id,ModelTask);
      await this.core.authorizeEpisodesTx(tx,[task.source_id],policy);
      await this.extractionClaimContextTx(tx,task.id);
      const rows = await tx.run(`MATCH (p:ExtractionPipeline {id:$id}) RETURN p.judge_task_id AS judge`,{id:task.id});
      if (!rows.records[0]) throw new ExtractionAuditError('extraction_audit_conflict');
      const existing = rows.records[0].get('judge');
      if (existing) return this.core.extractionRecordTx(tx,'ModelTask',z.uuidv7().parse(existing),ModelTask);
      await this.core.writableExtractionGenerationTx(tx,task.generation_id);
      await this.extractionNotCoveredTx(tx,task);
      const now = Math.max(task.updated_at,this.core.clock());
      const judge = ModelTask.parse({...task,id:uuidv7(),kind:'judge_claims',attempt_id:null,state:'queued',lease:null,policy_context:null,version:0,attempts:0,created_at:now,updated_at:now});
      await tx.run(`CREATE (:ModelTask {id:$id,work_key:$key,generation_id:$generation,source_id:$source,source_ingest_seq:$seq,body:$body,state:'queued'})
        WITH 1 AS ignored MATCH (p:ExtractionPipeline {id:$parent}) SET p.judge_task_id=$id`,
        {id:judge.id,key:`${task.id}:judge`,generation:task.generation_id,source:task.source_id,seq:task.source_ingest_seq,body:canonicalExtractionBody(judge),parent:task.id});
      return judge;
    });
  }
  async extractionDecisionsTx(tx: ManagedTransaction, attempt: ExtractionAttempt): Promise<ExtractionDisposition[]> {
    // Exactly 65 composite point seeks, including one overflow sentinel. There
    // is no suffix-order Top over an unbounded attempt partition.
    const rows = await tx.run(`UNWIND range(0,64) AS index MATCH (d:ExtractionDisposition {judge_attempt_id:$id,claim_index:index})
      USING INDEX SEEK d:ExtractionDisposition(judge_attempt_id,claim_index)
      RETURN d.body AS body ORDER BY index`,{id:attempt.id});
    const values = z.array(ExtractionDisposition).max(64).parse(rows.records.map(row=>JSON.parse(row.get('body'))));
    if (!attempt.output) { if (values.length) throw new ExtractionAuditError('extraction_audit_conflict'); return values; }
    const output = ExtractionModelOutput.parse(JSON.parse(attempt.output.canonical_body));
    if (output.task !== 'judge_claims') throw new ExtractionAuditError('extraction_audit_conflict');
    const premise = await this.core.extractionRecordTx(tx,'ExtractionJudgeInput',attempt.id,ExtractionJudgeInput);
    const expected = output.decisions.map(d=>({...d,judge_attempt_id:attempt.id,claim_attempt_id:premise.claim_context.attempt_id,claim_body_digest:premise.claim_context.body_digest}));
    if (canonicalExtractionBody(values) !== canonicalExtractionBody(expected)) throw new ExtractionAuditError('extraction_audit_conflict');
    return values;
  }
  async readExtractionDecisions(attemptId: string, context: InstallationContext): Promise<ExtractionDisposition[]> {
    return this.core.extractionTx(context,async(tx,policy)=>{
      const attempt = await this.core.extractionRecordTx(tx,'ExtractionAttempt',z.uuidv7().parse(attemptId),ExtractionAttempt);
      await this.core.authorizeEpisodesTx(tx,[attempt.source_id],policy);
      return this.extractionDecisionsTx(tx,attempt);
    });
  }
  private checkExtractionCAS(task: ModelTask, version: number): void {
    if (task.version !== version) throw new Error("task_conflict");
  }
  private checkExtractionLease(task: ModelTask, epoch: string): void {
    if (task.state !== "leased" || task.lease?.epoch !== epoch) throw new Error("lease_conflict");
    if (task.lease.writer_epoch !== this.core.writerEpoch) throw new Error("ownership_lost");
    if (this.core.clock() >= task.lease.expires_at) throw new Error("lease_expired");
  }
  private async saveExtractionTaskTx(tx: ManagedTransaction, task: ModelTask): Promise<ModelTask> {
    const parsed = ModelTask.parse(task);
    await tx.run(`MATCH (t:ModelTask {id:$id}) SET t.body=$body,t.state=$state,t.attempt_id=$attempt`,
      { id: task.id, body: canonicalExtractionBody(parsed), state: task.state, attempt: task.attempt_id });
    return parsed;
  }
  /** `provider` is the identity of the provider about to run the task; the leased task adopts it so a daemon whose
   * provider changed between boots finishes inherited work instead of refusing it (#218). Absent, the task keeps its own. */
  async leaseModelTask(input: LeaseModelTask & { provider?: Pick<ModelTask, "model" | "model_incarnation"> }, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = LeaseModelTaskWithProvider.parse(input);
    return this.core.extractionTx(context, async (tx, policy) => {
      const task = await this.core.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      if (task.state !== "queued" || task.attempts >= 1000 || (task.lost_leases ?? 0) >= 1000) throw new Error("invalid_transition");
      await this.core.writableExtractionGenerationTx(tx, task.generation_id);
      await this.core.authorizeEpisodesTx(tx, [task.source_id], policy);
      await this.validateExtractionSourceTx(tx, task);
      await this.extractionNotCoveredTx(tx, task);
      const now = Math.max(task.updated_at, receiptTime.parse(this.core.clock()));
      const leased = await this.saveExtractionTaskTx(tx, { ...task, ...request.provider, state: "leased", version: task.version + 1, attempts: task.attempts + 1, attempt_id: uuidv7(), updated_at: now,
        lease: { worker_id: request.worker_id, epoch: uuidv7(), writer_epoch: this.core.writerEpoch!, expires_at: now + request.lease_ms },
        policy_context: { revision: policy.policy_revision, authority: "installation" } });
      if (task.kind === 'judge_claims') {
        const parent = await tx.run(`MATCH (p:ExtractionPipeline {judge_task_id:$id}) RETURN p.id AS id`, {id:task.id});
        if (parent.records.length !== 1) throw new ExtractionAuditError('extraction_audit_conflict');
        const pipelineId = z.uuidv7().parse(parent.records[0]!.get('id'));
        const claim = await this.extractionClaimContextTx(tx, pipelineId);
        const premise = ExtractionJudgeInput.parse({task_id:task.id,attempt_id:leased.attempt_id,pipeline_id:pipelineId,
          source_head_revision:await this.extractionHeadTx(tx,task.source_id),policy_revision:policy.policy_revision,claim_context:claim});
        await tx.run(`CREATE (:ExtractionJudgeInput {id:$id,body:$body})`, {id:leased.attempt_id,body:canonicalExtractionBody(premise)});
      }
      return leased;
    });
  }
  /** Source text leaves the database only after lease and current policy checks. */
  async extractionTaskInput(taskId: string, leaseEpoch: string, context: InstallationContext): Promise<{ task: ModelTask; text: string; claim_context?: ExtractionClaimContext }> {
    return this.core.extractionTx(context, async (tx, policy) => {
      const task = await this.core.extractionRecordTx(tx, "ModelTask", z.uuidv7().parse(taskId), ModelTask);
      this.checkExtractionLease(task, z.uuidv7().parse(leaseEpoch));
      await this.core.authorizeEpisodesTx(tx, [task.source_id], policy);
      const source = await this.validateExtractionSourceTx(tx, task);
      if (task.kind === 'judge_claims') {
        const premise = await this.core.extractionRecordTx(tx,'ExtractionJudgeInput',task.attempt_id!,ExtractionJudgeInput);
        if (premise.policy_revision !== policy.policy_revision || premise.source_head_revision !== await this.extractionHeadTx(tx,task.source_id)) throw new ExtractionAuditError('extraction_audit_stale');
        return {task,text:source.content,claim_context:premise.claim_context};
      }
      return { task, text: source.content };
    });
  }
  private async finishExtractionTx(tx: ManagedTransaction, task: ModelTask, policy: PolicyState,
    outcome: Pick<ExtractionAttempt, "state" | "reason" | "disposition" | "output" | "spans" | "detail" | "reported_model">, requestDigest: string): Promise<ExtractionAttempt> {
    const now = Math.max(task.updated_at, receiptTime.parse(this.core.clock()));
    const attempt = ExtractionAttempt.parse({ state: outcome.state, reason: outcome.reason, disposition: outcome.disposition, output: outcome.output, spans: outcome.spans,
      ...(outcome.detail === undefined ? {} : { detail: outcome.detail }),
      ...(outcome.reported_model === undefined ? {} : { reported_model: outcome.reported_model }),
      id: task.attempt_id ?? uuidv7(), task_id: task.id,
      generation_id: task.generation_id, source_id: task.source_id, source_revision: task.source_revision, source_ingest_seq: task.source_ingest_seq, body_digest: task.body_digest,
      created_at: task.updated_at, updated_at: now, lease: task.lease, policy_context: { revision: policy.policy_revision, authority: "installation" } });
    await tx.run(`CREATE (:ExtractionAttempt {id:$id,task_id:$task,generation_id:$generation,source_id:$source,source_ingest_seq:$seq,state:$state,request_digest:$digest,body:$body})`,
      { id: attempt.id, task: task.id, generation: task.generation_id, source: task.source_id, seq: task.source_ingest_seq, state: attempt.state, digest: requestDigest, body: canonicalExtractionBody(attempt) });
    await this.saveExtractionTaskTx(tx, { ...task, state: attempt.state, attempt_id: attempt.id, lease: null, version: task.version + 1, updated_at: now,
      attempts: task.attempts + (task.attempt_id === null ? 1 : 0), policy_context: attempt.policy_context });
    return attempt;
  }
  /** A replayed completion: the stored attempt for an identical request, re-authorized when it carries output. */
  private async replayedAttemptTx(tx: ManagedTransaction, request: CompleteExtractionAttempt, digest: string, policy: PolicyState): Promise<ExtractionAttempt | null> {
    const oldRows = await tx.run<{ body: string; digest: string }>(`MATCH (a:ExtractionAttempt {id:$id}) RETURN a.body AS body,a.request_digest AS digest`, { id: request.id });
    const old = oldRows.records[0];
    if (!old) return null;
    if (old.get("digest") !== digest) throw new Error("attempt_conflict");
    const attempt = ExtractionAttempt.parse(JSON.parse(old.get("body")));
    if (attempt.output) await this.core.authorizeEpisodesTx(tx, [attempt.source_id], policy);
    return attempt;
  }
  /** A judge attempt is checked against its recorded premises and the claim context it judged: stale premises or a
   * decision set that does not match finish the attempt as failed, a matching one records each disposition. Null when
   * the attempt may finish as submitted. */
  private async judgeAttemptTx(tx: ManagedTransaction, task: ModelTask, policy: PolicyState, request: CompleteExtractionAttempt, digest: string): Promise<ExtractionAttempt | null> {
    const premise = await this.core.extractionRecordTx(tx,'ExtractionJudgeInput',task.attempt_id!,ExtractionJudgeInput);
    if (premise.policy_revision !== policy.policy_revision || premise.source_head_revision !== await this.extractionHeadTx(tx,task.source_id)) {
      return this.finishExtractionTx(tx,task,policy,{state:'failed',reason:'premises_changed',disposition:null,output:null,spans:[]},digest);
    }
    const parent = await this.extractionClaimContextTx(tx,premise.pipeline_id);
    if (canonicalExtractionBody(parent) !== canonicalExtractionBody(premise.claim_context) || premise.task_id !== task.id || premise.attempt_id !== task.attempt_id) throw new ExtractionAuditError('extraction_audit_conflict');
    if (!request.output) return null;
    const body = ExtractionModelOutput.parse(JSON.parse(request.output.canonical_body));
    // The attempt names the check the judge failed (#218): ABI task, parent digest, or the decision set's shape.
    const refuse = (detail: ExtractionFailureDetail) => this.finishExtractionTx(tx,task,policy,{state:'failed',reason:'provider_mismatch',detail,disposition:null,output:null,spans:[]},digest);
    if (body.task !== 'judge_claims') return refuse('normalize');
    if (body.claim_body_digest !== parent.body_digest) return refuse('digest');
    if (body.decisions.length !== parent.claims.length || body.decisions.some((d,i)=>d.claim_index !== i || canonicalExtractionBody(d.evidence) !== canonicalExtractionBody(parent.claims[i]!.evidence))) return refuse('judge_shape');
    for (const d of body.decisions) {
      const decision = ExtractionDisposition.parse({...d,judge_attempt_id:task.attempt_id,claim_attempt_id:parent.attempt_id,claim_body_digest:parent.body_digest});
      await tx.run(`CREATE (:ExtractionDisposition {judge_attempt_id:$id,claim_index:$index,body:$body})`,
        {id:task.attempt_id,index:neo4j.int(d.claim_index),body:canonicalExtractionBody(decision)});
    }
    return null;
  }
  /** Exact completion replay returns the stored immutable outcome. Neither the
   * submitted output nor caller-selected policy context is ever an authority. */
  async recordExtractionAttempt(input: CompleteExtractionAttempt, context: InstallationContext): Promise<ExtractionAttempt> {
    requireInstallation(context);
    const request = CompleteExtractionAttempt.parse(input), digest = extractionBodyDigest(request);
    return this.core.extractionTx(context, async (tx, policy) => {
      const replayed = await this.replayedAttemptTx(tx, request, digest, policy);
      if (replayed) return replayed;
      const task = await this.core.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      this.checkExtractionLease(task, request.lease_epoch);
      if (task.attempt_id !== request.id) throw new Error("attempt_conflict");
      await this.core.writableExtractionGenerationTx(tx, task.generation_id);
      const source = await this.validateExtractionSourceTx(tx, task);
      try { await this.core.authorizeEpisodesTx(tx, [task.source_id], policy); }
      catch (error) {
        if (!(error instanceof ReceiptError) || error.code !== "policy_denied") throw error;
        return this.finishExtractionTx(tx, task, policy, { state: "cancelled", reason: "policy_denied", output: null, disposition: null, spans: [] }, digest);
      }
      checkAttemptOutput(request, task, source);
      if (task.kind === 'judge_claims') {
        const judged = await this.judgeAttemptTx(tx, task, policy, request, digest);
        if (judged) return judged;
      }
      return this.finishExtractionTx(tx, task, policy, request, digest);
    });
  }
  /** A CAS-guarded ModelTask transition: the task is loaded inside an extraction transaction, its version checked, and its state gated. */
  private modelTaskTransition(input: ModelTaskCAS, context: InstallationContext, from: readonly ModelTask["state"][],
    body: (tx: ManagedTransaction, policy: PolicyState, task: ModelTask, request: ModelTaskCAS) => Promise<ModelTask>): Promise<ModelTask> {
    requireInstallation(context);
    const request = ModelTaskCAS.parse(input);
    return this.core.extractionTx(context, async (tx, policy) => {
      const task = await this.core.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      if (!from.includes(task.state)) throw new Error("invalid_transition");
      return body(tx, policy, task, request);
    });
  }
  async cancelModelTask(input: ModelTaskCAS, context: InstallationContext): Promise<ModelTask> {
    return this.modelTaskTransition(input, context, ["queued", "leased", "expired", "worker_lost"], async (tx, policy, task, request) => {
      // Open work is cancelled in place. An expired or lost lease is unresolved work, not an outcome (extractionPipelineOmission);
      // cancelling it is how a caller whose retry budget is spent turns it into a durable, coverable omission. The cancel is a
      // fresh content-free attempt record, so the settled lease attempt stays immutable and the attempt count is unchanged.
      const open = task.state === "queued" || task.state === "leased" ? task : { ...task, attempt_id: null, lease: null, attempts: task.attempts - 1 };
      await this.finishExtractionTx(tx, open, policy, { state: "cancelled", reason: "cancelled", output: null, disposition: null, spans: [] }, extractionBodyDigest({ action: "cancel", ...request }));
      return this.core.extractionRecordTx(tx, "ModelTask", task.id, ModelTask);
    });
  }
  async settleModelTask(input: SettleModelTask, context: InstallationContext): Promise<ModelTask> {
    requireInstallation(context);
    const request = SettleModelTask.parse(input);
    return this.core.extractionTx(context, async (tx, policy) => {
      const task = await this.core.extractionRecordTx(tx, "ModelTask", request.task_id, ModelTask);
      this.checkExtractionCAS(task, request.expected_version);
      if (task.state !== "leased" || task.lease?.epoch !== request.lease_epoch) throw new Error("lease_conflict");
      if (request.reason === "expired" && this.core.clock() < task.lease.expires_at) throw new Error("lease_not_expired");
      if (request.reason === "worker_lost" && task.lease.writer_epoch === this.core.writerEpoch) throw new Error("worker_still_owned");
      // No provider outcome was received: the lease returns its attempt to the provider-failure budget and counts as a
      // lost lease instead, so a restart or an overrun on the last budgeted lease never becomes a terminal omission.
      const lost = { ...task, attempts: task.attempts - 1, lost_leases: (task.lost_leases ?? 0) + 1 };
      await this.finishExtractionTx(tx, lost, policy, { state: request.reason, reason: request.reason, output: null, disposition: null, spans: [] }, extractionBodyDigest(request));
      return this.core.extractionRecordTx(tx, "ModelTask", task.id, ModelTask);
    });
  }
  private async extractionNotCoveredTx(tx: ManagedTransaction, task: ModelTask): Promise<void> {
    const rows = await tx.run(`MATCH (c:ExtractionCoverage {generation_id:$generation}) WHERE c.covered_ingest_seq >= $seq RETURN c.key AS key LIMIT 1`, { generation: task.generation_id, seq: task.source_ingest_seq });
    if (rows.records.length) throw new Error("coverage_frozen");
  }
  async retryModelTask(input: ModelTaskCAS, context: InstallationContext): Promise<ModelTask> {
    return this.modelTaskTransition(input, context, ["failed", "expired", "worker_lost"], async (tx, policy, task) => {
      await this.core.writableExtractionGenerationTx(tx, task.generation_id);
      await this.core.authorizeEpisodesTx(tx, [task.source_id], policy);
      await this.validateExtractionSourceTx(tx, task);
      await this.extractionNotCoveredTx(tx, task);
      return this.saveExtractionTaskTx(tx, { ...task, state: "queued", attempt_id: null, lease: null, policy_context: null, version: task.version + 1, updated_at: Math.max(task.updated_at, this.core.clock()) });
    });
  }
  async extractionSelectionTx(tx: ManagedTransaction): Promise<ExtractionSelection> {
    const rows = await tx.run(`MATCH (s:Meta {key:'extraction_selector'})
      RETURN s.generation_id AS generation_id,s.selector_version AS selector_version`);
    const parsed = ExtractionSelection.safeParse(rows.records[0]?.toObject());
    if (!parsed.success) throw new GenerationReadinessError("selector_unavailable");
    if (parsed.data.generation_id) {
      const active = await this.core.extractionRecordTx(tx, "ExtractionGeneration", parsed.data.generation_id, Generation);
      if (active.state !== "active") throw new GenerationReadinessError("generation_not_active");
    }
    return parsed.data;
  }
  async readExtractionSelection(context: InstallationContext): Promise<ExtractionSelection> {
    return this.core.extractionTx(context, tx => this.extractionSelectionTx(tx));
  }
}
