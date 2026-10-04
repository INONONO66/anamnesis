import { SCHEMA_ID } from "@anamnesis/protocol";
import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { MemoryElement, MemoryLink, Generation, type ExtractionAttempt, type ModelTask } from "@anamnesis/protocol";
import { SemanticClaimValidationError, validateSemanticClaim, type SemanticSourceContext, type ValidatedSemanticClaim } from "@anamnesis/protocol";
import { canonicalExtractionBody, extractionBodyDigest, FactRelationJudgement, type ExtractionFailureDetail } from "@anamnesis/protocol";
import { ExtractionPipeline, ExtractionDisposition, ExtractionAuditError, FactRelationContext } from "@anamnesis/protocol";
import { ProposeRetainedClaim, MaterializeRetainedClaim, ReviewRetainedClaim, MaterializationResult, FactRelationDecision, SemanticReviewPremises, SemanticResolution, SemanticReviewOutput, RetainedSemanticProposal, semanticReviewClaimBody } from "@anamnesis/protocol";
import { ExtractionModelOutput } from "@anamnesis/protocol";
import { z } from "zod";
import { semanticClaimTime, validatedFactTime, sha256 } from "./digest.ts";
import { type InstallationContext, type PolicyState } from "./policy.ts";
import { type ElementProperties } from "./records.ts";
import type { StoreCore } from "./core.ts";
import type { ReceiptStore } from "./receipt-store.ts";
import type { ExtractionStore } from "./extraction-store.ts";
import type { ElementStore } from "./element-store.ts";
import type { ExtractionJournalEntry } from "./extraction-journal.ts";
import { SourceMaterializationResult, type MaterializationStateEntry } from "./materialization-state.ts";

/** Only model-reported confidence admits a claim to semantic writes. Audit-only
 * output (no confidence) stays an auditable decision and never becomes a Fact. */
function admittedClaims(decisions: ExtractionDisposition[], claimOutput: Extract<ExtractionModelOutput, { task: "claim" }>) {
  return decisions.flatMap(decision => {
    if (decision.disposition !== "retain" && decision.disposition !== "correct") return [];
    const extracted = claimOutput.claims[decision.claim_index];
    if (!extracted) throw new ExtractionAuditError("extraction_audit_conflict");
    const confidence = decision.confidence ?? extracted.confidence;
    return confidence === undefined ? [] : [{ decision, extracted, confidence }];
  });
}
type AdmittedClaim = ReturnType<typeof admittedClaims>[number];

function pendingRelations(entry: ExtractionJournalEntry | undefined) {
  return (entry?.relations?.verdicts ?? []).filter(verdict => verdict.context.candidates.length > 0 && verdict.judgements === null);
}

/** What one materialization pass over a source shares between its claims. Entities first seen in the pass keep one
 * allocated id per entity_key, so two claims of one source share a single new Entity whichever is written first. */
interface MaterializationPass {
  judge: ExtractionAttempt; pipelineId: string; source: Omit<SemanticSourceContext["episode"], "content_language">;
  language: string; profile: string; allocatedEntities: Map<string, string>;
}

/** Per-source custody is written exactly once; readiness requires every covered source to carry custody even when the
 * judge admitted nothing or every claim was refused. Returns whether any Fact was created. */
async function custodyRecord(core: StoreCore, custody: string, pass: MaterializationPass, written: { factIds: string[]; refused: string[]; duplicates: string[] }): Promise<boolean> {
  const { judge, source } = pass, { factIds, refused, duplicates } = written;
  const created = factIds.length > 0;
  await core.materializationState.set(uuidv7(), {
    request_digest: extractionBodyDigest({ generation: judge.generation_id, source: source.id, judge: judge.id, facts: factIds, refused, duplicates }),
    occurrence_key: custody, source_episode_id: source.id, generation_id: judge.generation_id, source_ingest_seq: source.ingest_seq,
    fact_ids: factIds, result: { created, facts: factIds.length, refused, ...(duplicates.length ? { duplicates } : {}) },
  });
  return created;
}

export class MaterializationStore {
  constructor(private readonly core: StoreCore, private readonly receipts: ReceiptStore, private readonly extraction: ExtractionStore, private readonly elements: ElementStore) {}

  /** File writes precede graph commit. An interrupted transaction leaves an
   * intent, not proof that Facts or proposal consumption committed. */
  private async committedOperationTx(tx: ManagedTransaction, entry: MaterializationStateEntry | undefined): Promise<MaterializationStateEntry | undefined> {
    if (!entry || entry.fact_ids.length === 0) return entry;
    const rows = await tx.run<{ count: number }>(`UNWIND $ids AS id
      MATCH (f:Fact {id:id,generation:$generation})-[:DERIVED_FROM]->(:Episode {id:$source})
      RETURN count(DISTINCT f.id) AS count`, { ids: entry.fact_ids, generation: entry.generation_id, source: entry.source_episode_id });
    return rows.records[0]?.get("count") === entry.fact_ids.length ? entry : undefined;
  }
  private async recordMaterialization(input: {
    operationId: string; digest: string; occurrence: string; generation: string; source: { id: string; ingest_seq: number };
  }, result: MaterializationStateEntry["result"], factIds: string[] = []): Promise<void> {
    await this.core.materializationState.set(input.operationId, {
      request_digest: input.digest, occurrence_key: input.occurrence, generation_id: input.generation,
      source_episode_id: input.source.id, source_ingest_seq: input.source.ingest_seq, fact_ids: factIds, result,
    });
  }
  async sourceCustodyTx(tx: ManagedTransaction, generation: string, source: string): Promise<MaterializationStateEntry | undefined> {
    return this.committedOperationTx(tx, await this.core.materializationState.byOccurrence(extractionBodyDigest([generation, source])));
  }
  /** Bound the entire new-occurrence candidate partition before handing any
   * content to the semantic judge. Overflow is unknown, not an empty candidate set. */
  private async semanticCandidatesTx(tx: ManagedTransaction, generation: string, policy: PolicyState) {
    const rows = await tx.run<{ props: ElementProperties }>(`MATCH (f:Fact {generation:$generation})
      USING INDEX f:Fact(generation,id) WHERE f.id IS NOT NULL
      RETURN properties(f) AS props ORDER BY f.id LIMIT 129`, { generation });
    if (rows.records.length > 128) throw new Error("semantic_candidates_unavailable");
    const candidates = [];
    for (const row of rows.records) {
      const p = row.get("props");
      const sources = z.array(z.uuidv7()).min(1).max(16).parse(p["source_episode_ids"]);
      await this.core.authorizeEpisodesTx(tx, sources, policy);
      candidates.push({ id: z.uuidv7().parse(p["id"]), content: z.string().parse(p["content"]), digest: extractionBodyDigest(p) });
    }
    return candidates;
  }
  private async semanticPremisesTx(tx: ManagedTransaction, request: ProposeRetainedClaim, profile: string, policy: PolicyState): Promise<SemanticReviewPremises> {
    const source = await this.receipts.semanticEpisodeTx(tx, request.source_episode_id, policy);
    const generation = await this.core.writableExtractionGenerationTx(tx, request.generation_id);
    const selection = await this.extraction.extractionSelectionTx(tx);
    if (generation.state === "active" && selection.generation_id !== generation.id) throw new Error("semantic_generation_unselected");
    const head = await this.extraction.extractionHeadTx(tx, source.id);
    if (head !== source.revision_key) throw new Error("stale_materialization");
    const judge = await this.core.extractionAttempt(request.judge_attempt_id);
    if (judge.state !== "succeeded" || judge.generation_id !== generation.id || judge.source_id !== source.id
      || judge.source_revision !== source.revision_key || judge.body_digest !== source.content_digest
      || judge.source_ingest_seq !== source.ingest_seq || judge.policy_context.revision !== policy.policy_revision) throw new Error("stale_materialization");
    const decisions = await this.extraction.extractionDecisionsTx(tx, judge);
    const decision = decisions[request.claim_index];
    if (!decision || !["retain", "correct"].includes(decision.disposition)) throw new Error("audit_decision_refused");
    const premise = await this.core.extractionJudgeInput(judge.id);
    if (premise.source_head_revision !== head || premise.policy_revision !== policy.policy_revision) throw new Error("stale_materialization");
    const candidates = await this.semanticCandidatesTx(tx, generation.id, policy);
    return SemanticReviewPremises.parse({ request, request_digest: extractionBodyDigest(request), judge_profile_id: profile,
      source, audit_evidence: decision.evidence, source_head_revision_key: head, policy_revision: policy.policy_revision,
      generation_digest: extractionBodyDigest(generation), selection, candidates, candidate_digest: extractionBodyDigest(candidates) });
  }
  async prepareSemanticReview(input: ProposeRetainedClaim, profile: string, context: InstallationContext) {
    const request = ProposeRetainedClaim.parse(input), digest = extractionBodyDigest(request);
    return this.core.extractionTx(context, async (tx, policy) => {
      await this.core.authorizeEpisodesTx(tx, [request.source_episode_id], policy);
      const old = await tx.run(`MATCH (a:AdjudicationInput {id:$id})
        OPTIONAL MATCH (p:AdjudicationProposal {id:a.id}) RETURN a.body AS body,p.body AS proposal`, { id: request.proposal_id });
      if (old.records[0]) {
        const premises = SemanticReviewPremises.parse(JSON.parse(old.records[0].get("body")));
        if (premises.request_digest !== digest || premises.judge_profile_id !== profile) throw new Error("semantic_proposal_conflict");
        if (!old.records[0].get("proposal")) throw new Error("semantic_review_incomplete");
        return { created: false, premises, proposal: RetainedSemanticProposal.parse(JSON.parse(old.records[0].get("proposal"))) };
      }
      const premises = await this.semanticPremisesTx(tx, request, profile, policy);
      await tx.run(`CREATE (:AdjudicationInput {id:$id,body:$body,started_at:$now})`, { id: request.proposal_id, body: canonicalExtractionBody(premises), now: this.core.clock() });
      return { created: true, premises, proposal: null };
    });
  }
  private async validateSemanticResolutionTx(tx: ManagedTransaction, premises: SemanticReviewPremises, resolution: SemanticResolution) {
    for (const entity of resolution.entity_resolutions) {
      if (entity.status === "unresolved") continue;
      const rows = await tx.run(`MATCH (e:Element {id:$id}) RETURN e.schema AS schema,e.generation AS generation`, { id: entity.entity_id });
      if (entity.status === "existing") {
        if (rows.records[0]?.get("schema") !== SCHEMA_ID.ENTITY || rows.records[0]?.get("generation") !== premises.request.generation_id) throw new Error("semantic_entity_stale");
      } else {
        const keys = await tx.run(`MATCH (e:Entity {generation:$generation,entity_key:$key}) RETURN e.id LIMIT 1`, { generation: premises.request.generation_id, key: entity.entity_key });
        if (rows.records.length || keys.records.length) throw new Error("semantic_entity_stale");
      }
    }
  }
  private semanticValidation(premises: SemanticReviewPremises, resolution: SemanticResolution, claim: ProposeRetainedClaim["semantic_claim"]) {
    const validated = validateSemanticClaim(claim, { generation: premises.request.generation_id, fact_language_policy: "source",
      entity_resolutions: resolution.entity_resolutions, attribution_speakers: resolution.attribution_speakers,
      allow_no_single_locus: resolution.allow_no_single_locus,
      episode: { ...premises.source, content_language: resolution.content_language } });
    if (canonicalExtractionBody(validated.evidence) !== canonicalExtractionBody(premises.audit_evidence)) throw new Error("semantic_evidence_mismatch");
    return validated;
  }
  /** Completion has retained pre-call premises. It does not accept booleans
   * asserting independence, lineage, policy, or semantic permission. */
  async completeSemanticReview(proposalId: string, resolved: SemanticResolution, judged: SemanticReviewOutput, context: InstallationContext) {
    const id = z.uuidv7().parse(proposalId), resolution = SemanticResolution.parse(resolved), output = SemanticReviewOutput.parse(judged);
    return this.core.extractionTx(context, async (tx, policy) => {
      const rows = await tx.run(`MATCH (a:AdjudicationInput {id:$id}) RETURN a.body AS body`, { id });
      if (!rows.records[0]) throw new Error("semantic_review_unavailable");
      const premises = SemanticReviewPremises.parse(JSON.parse(rows.records[0].get("body")));
      const terminal = await tx.run(`MATCH (a:AdjudicationAttempt {id:$id}) RETURN a.outcome AS outcome`, { id });
      if (terminal.records[0] && terminal.records[0].get("outcome") !== "succeeded") throw new Error("semantic_review_terminal");
      const current = await this.semanticPremisesTx(tx, premises.request, premises.judge_profile_id, policy);
      if (canonicalExtractionBody(current) !== canonicalExtractionBody(premises)) throw new Error("semantic_review_stale");
      await this.validateSemanticResolutionTx(tx, premises, resolution);
      const validated = this.semanticValidation(premises, resolution, premises.request.semantic_claim);
      if (output.disposition === "retain" && canonicalExtractionBody(output.semantic_claim) !== canonicalExtractionBody(premises.request.semantic_claim)) throw new Error("semantic_output_mismatch");
      if (output.semantic_claim) this.semanticValidation(premises, resolution, output.semantic_claim);
      const proposal = RetainedSemanticProposal.parse({ premises, resolution, output,
        proposed_claim_digest: extractionBodyDigest(semanticReviewClaimBody(validated.claim, premises.source, resolution)),
        output_claim_digest: extractionBodyDigest(output.semantic_claim) });
      const old = await tx.run(`MATCH (p:AdjudicationProposal {id:$id}) RETURN p.body AS body`, { id });
      const body = canonicalExtractionBody(proposal);
      if (old.records[0]) {
        if (old.records[0].get("body") !== body) throw new Error("semantic_proposal_conflict");
        return proposal;
      }
      await tx.run(`CREATE (:AdjudicationAttempt {id:$id,outcome:'succeeded',input_body:$input,output_body:$output,finished_at:$now})
        CREATE (:AdjudicationProposal {id:$id,body:$body,digest:$digest})`,
        { id, input: canonicalExtractionBody(premises), output: canonicalExtractionBody({ resolution, output }), body, digest: extractionBodyDigest(proposal), now: this.core.clock() });
      return proposal;
    });
  }
  async failSemanticReview(proposalId: string, error: string, context: InstallationContext): Promise<void> {
    const id = z.uuidv7().parse(proposalId);
    await this.core.extractionTx(context, async tx => {
      await tx.run(`MATCH (a:AdjudicationInput {id:$id})
        MERGE (t:AdjudicationAttempt {id:$id}) ON CREATE SET t.outcome='validation_error',t.input_body=a.body,t.error_digest=$error,t.finished_at=$now`,
        { id, error: sha256(error), now: this.core.clock() });
    });
  }
  async reviewRetainedClaim(input: ReviewRetainedClaim, context: InstallationContext) {
    const request = ReviewRetainedClaim.parse(input);
    return this.core.extractionTx(context, async (tx, policy) => {
      if (!context.client_binding) throw new Error("semantic_operator_binding_required");
      const proposal = await this.retainedSemanticProposalTx(tx, request.proposal_id);
      await this.core.authorizeEpisodesTx(tx, [proposal.premises.source.id], policy);
      const body = canonicalExtractionBody({ ...request, operator_binding: context.client_binding });
      const old = await tx.run(`MATCH (r:AdjudicationReview) WHERE r.proposal_id=$proposal OR r.review_id=$id RETURN r.body AS body`, { proposal: request.proposal_id, id: request.review_id });
      if (old.records.length) {
        if (old.records.length !== 1 || old.records[0]!.get("body") !== body) throw new Error("semantic_review_conflict");
        return request;
      }
      await tx.run(`CREATE (:AdjudicationReview {review_id:$id,proposal_id:$proposal,body:$body,action:$action})`, { id: request.review_id, proposal: request.proposal_id, body, action: request.action });
      return request;
    });
  }
  private async retainedSemanticProposalTx(tx: ManagedTransaction, id: string): Promise<RetainedSemanticProposal> {
    const rows = await tx.run(`MATCH (p:AdjudicationProposal {id:$id}) MATCH (a:AdjudicationAttempt {id:p.id})
      RETURN p.body AS body,p.digest AS digest,a.outcome AS outcome,a.input_body AS input,a.output_body AS output`, { id });
    const row = rows.records[0];
    if (!row) throw new Error("retained_review_unavailable");
    const proposal = RetainedSemanticProposal.parse(JSON.parse(row.get("body")));
    if (proposal.premises.request.proposal_id !== id || row.get("digest") !== extractionBodyDigest(proposal) || row.get("outcome") !== "succeeded"
      || row.get("input") !== canonicalExtractionBody(proposal.premises) || row.get("output") !== canonicalExtractionBody({ resolution: proposal.resolution, output: proposal.output })) throw new Error("semantic_proposal_corrupt");
    return proposal;
  }
  private async materializationReplayTx(tx: ManagedTransaction, request: MaterializeRetainedClaim, digest: string, policy: PolicyState) {
    const intent = await this.core.materializationState.get(request.operation_id);
    if (intent && intent.request_digest !== digest) throw new Error("materialization_conflict");
    const prior = await this.committedOperationTx(tx, intent);
    const consumption = await tx.run<{ proposal: string; fact: string; link: string }>(`MATCH (c:AdjudicationConsumption {operation_id:$id})
      MATCH (:Fact {id:c.fact_id})-[l:DERIVED_FROM {id:c.link_id}]->(:Episode)
      RETURN c.proposal_id AS proposal,c.fact_id AS fact,l.id AS link`, { id: request.operation_id });
    const consumed = consumption.records[0];
    if (consumed) {
      // Covered operations leave the bounded file. The retained review and its
      // consumption still prove the original request and immutable Fact/link IDs.
      if (!prior) {
        const proposal = await this.retainedSemanticProposalTx(tx, consumed.get("proposal"));
        const original = { ...proposal.premises.request, operation_id: request.operation_id, semantic_claim: proposal.output.semantic_claim };
        if (extractionBodyDigest(original) !== digest) throw new Error("materialization_conflict");
      }
      await this.receipts.semanticEpisodeTx(tx, request.source_episode_id, policy);
      return { intent, result: MaterializationResult.parse({
        ...(prior?.result ?? { fact_id: consumed.get("fact"), link_id: consumed.get("link") }), created: false,
      }) };
    }
    return { intent, result: null };
  }
  /** One accepted new occurrence. This path does not select a serving generation,
   * infer approval from audit decisions, or merge into a previous generation. */
  async materializeRetainedClaim(input: MaterializeRetainedClaim, context: InstallationContext): Promise<MaterializationResult> {
    canonicalExtractionBody(input);
    const request = MaterializeRetainedClaim.parse(input), digest = extractionBodyDigest(request);
    let allocated: { fact_id: string; link_id: string; mention_ids: string[] } | undefined;
    return this.core.extractionTx(context, async (tx, policy) => {
      const { intent, result: replay } = await this.materializationReplayTx(tx, request, digest, policy);
      if (replay) return replay;
      if (!request.proposal_id) throw new Error("retained_review_unavailable");
      const proposal = await this.retainedSemanticProposalTx(tx, request.proposal_id);
      const { premises, resolution, output } = proposal;
      const original = premises.request;
      if (request.generation_id !== original.generation_id || request.source_episode_id !== original.source_episode_id
        || request.judge_attempt_id !== original.judge_attempt_id || request.claim_index !== original.claim_index
        || extractionBodyDigest(request.semantic_claim) !== proposal.output_claim_digest) throw new Error("semantic_candidate_mismatch");
      const review = await tx.run(`MATCH (r:AdjudicationReview {proposal_id:$id}) RETURN r.body AS body`, { id: request.proposal_id });
      if (!review.records[0]) throw new Error("retained_review_unavailable");
      const accepted = ReviewRetainedClaim.extend({ operator_binding: z.uuidv7() }).parse(JSON.parse(review.records[0].get("body")));
      if (accepted.action !== "accept" || accepted.proposal_id !== request.proposal_id || output.disposition === "suppress" || !output.semantic_claim) throw new Error("semantic_review_refused");
      const consumed = await tx.run(`MATCH (c:AdjudicationConsumption {proposal_id:$id}) RETURN c.operation_id`, { id: request.proposal_id });
      if (consumed.records.length) throw new Error("semantic_proposal_consumed");
      const current = await this.semanticPremisesTx(tx, original, premises.judge_profile_id, policy);
      if (canonicalExtractionBody(current) !== canonicalExtractionBody(premises)) throw new Error("semantic_review_stale");
      await this.validateSemanticResolutionTx(tx, premises, resolution);
      const validated = this.semanticValidation(premises, resolution, output.semantic_claim);
      if (proposal.proposed_claim_digest !== extractionBodyDigest(semanticReviewClaimBody(original.semantic_claim, premises.source, resolution))) throw new Error("semantic_candidate_mismatch");
      const occurrence = extractionBodyDigest([original.generation_id, original.source_episode_id, original.judge_attempt_id, original.claim_index]);
      const existing = await this.committedOperationTx(tx, await this.core.materializationState.byOccurrence(occurrence));
      if (existing && intent?.occurrence_key !== occurrence) throw new Error("claim_already_materialized");
      allocated ??= { fact_id: uuidv7(), link_id: uuidv7(), mention_ids: validated.identity.entity_ids.map(() => uuidv7()) };
      const result = await this.writeValidatedFactTx(tx, { validated, resolution, source: premises.source,
        generation: request.generation_id, profile: premises.judge_profile_id, policy: policy.policy_revision,
        operationId: request.operation_id, digest, occurrence, proposalId: request.proposal_id, allocated });
      await tx.run(`CREATE (:AdjudicationConsumption {proposal_id:$proposal,operation_id:$id,fact_id:$fact,link_id:$link})`,
        { proposal: request.proposal_id, id: request.operation_id, fact: result.fact_id, link: result.link_id });
      return result;
    });
  }
  /** Both admission paths write only a mechanically validated new occurrence.
   * mergeLinkTx creates physical links and their real conducting rows atomically.
   * Relation adjudication is separate; this body never invalidates another Fact. */
  private async writeValidatedFactTx(tx: ManagedTransaction, input: {
    validated: ValidatedSemanticClaim; resolution: SemanticResolution; source: SemanticReviewPremises["source"];
    generation: string; profile: string; policy: number; operationId: string; digest: string; occurrence: string;
    proposalId: string | null; allocated: { fact_id: string; link_id: string; mention_ids: string[] };
    /** Relation verdicts already applied for this Fact (D53); absent when no relation judge ran. */
    relations?: FactRelationDecision[];
  }): Promise<MaterializationResult> {
    const { validated, resolution, source, generation, profile, policy, operationId, digest, occurrence, proposalId, allocated, relations } = input;
    const claim = validated.claim, { fact_id, link_id } = allocated;
    const merged = await tx.run<{ id: string }>(`MERGE (f:Fact {generation:$generation,meaning_digest:$meaning,primary_episode_id:$source})
      ON CREATE SET f.id=$id RETURN f.id AS id`, { generation, meaning: validated.meaning_digest, source: source.id, id: fact_id });
    const retainedId = z.uuidv7().parse(merged.records[0]?.get("id"));
    if (retainedId !== fact_id) {
      const evidence = await tx.run<{ id: string }>(`MATCH (:Fact {id:$fact})-[l:DERIVED_FROM]->(:Episode {id:$source}) RETURN l.id AS id`, { fact: retainedId, source: source.id });
      const result = MaterializationResult.parse({ created: false, fact_id: retainedId, link_id: evidence.records[0]?.get("id") });
      await this.recordMaterialization(input, result, [retainedId]);
      return result;
    }
    const inherited = claim.time.resolution === "inherited";
    const element = MemoryElement.parse({ id: fact_id, schema: SCHEMA_ID.CLAIM, content: claim.content, time: validatedFactTime(validated, source),
      origin: { source: "semantic-extraction", session: generation, actor: profile, record: occurrence },
      mass: claim.confidence, properties: { ...validated.identity.properties, sub_kind: claim.sub_kind, modality: claim.modality,
        confidence: claim.confidence, content_language: claim.content_language, semantic_time: claim.time,
        time_basis: inherited ? "episode_fallback" : "claim" } });
    await this.elements.createElementTx(tx, element, null, { existingFact: true });
    await tx.run(`MATCH (f:Fact {id:$id}) SET f.generation=$generation,f.content_language=$language,f.sub_kind=$subkind,f.modality=$modality,
      f.confidence=$confidence,f.meaning_digest=$meaning,f.primary_episode_id=$source,f.source_episode_ids=$sources,f.max_source_ingest_seq=$seq,
      f.echo_state=$echo,f.echo_depth=$depth,f.echo_lineage_truncated=false,f.parent_recall_ids=$parents,f.corroboration_root_episode_ids=$roots,
      f.entity_ids=$entities,f.support_fact_ids=[],f.proposal_id=$proposal,f.policy_revision=$policy,f.semantic_profile_id=$profile`,
      { id: fact_id, generation, language: claim.content_language, subkind: claim.sub_kind, modality: claim.modality,
        confidence: claim.confidence, meaning: validated.meaning_digest, source: source.id, sources: [source.id], seq: source.ingest_seq,
        echo: validated.identity.echo_state, depth: validated.identity.echo_depth, parents: validated.identity.parent_recall_ids,
        roots: validated.identity.corroboration_root_episode_ids, entities: validated.identity.entity_ids, proposal: proposalId, policy, profile });
    await this.elements.mergeLinkTx(tx, MemoryLink.parse({ id: link_id, from: fact_id, to: source.id, role: "DERIVED_FROM", content: "semantic evidence", weight: 1 }));
    await tx.run(`MATCH ()-[l:DERIVED_FROM]->() WHERE l.id=$id SET l.span=$span,l.evidence_text=$text,l.proposal_id=$proposal`,
      { id: link_id, span: validated.evidence ? [validated.evidence.start, validated.evidence.end] : null, text: validated.evidence?.text ?? null, proposal: proposalId });
    const createdEntities = new Set<string>();
    for (const entity of resolution.entity_resolutions) {
      if (entity.status !== "new" || !validated.identity.entity_ids.includes(entity.entity_id) || createdEntities.has(entity.entity_id)) continue;
      await this.elements.createElementTx(tx, MemoryElement.parse({ id: entity.entity_id, schema: SCHEMA_ID.ENTITY, content: entity.normalized_name,
        origin: { source: "semantic-extraction", session: generation, actor: profile, record: entity.entity_key },
        properties: { normalized_name: entity.normalized_name, entity_kind: entity.entity_kind, entity_key: entity.entity_key } }), null, {});
      await tx.run(`MATCH (e:Entity {id:$id}) SET e.generation=$generation,e.entity_key=$key`, { id: entity.entity_id, generation, key: entity.entity_key });
      createdEntities.add(entity.entity_id);
    }
    for (const [i, entity] of validated.identity.entity_ids.entries()) {
      await this.elements.mergeLinkTx(tx, MemoryLink.parse({ id: allocated.mention_ids[i], from: fact_id, to: entity, role: "MENTIONS", content: "semantic entity mention", weight: 1 }));
      await tx.run(`MERGE (w:EntityWitness {entity_id:$entity,generation:$generation,policy_revision:$policy}) SET w.state='COMPLETE'`, { entity, generation, policy: neo4j.int(policy) });
    }
    const result = MaterializationResult.parse({ created: true, fact_id, link_id, ...(relations ? { relations } : {}) });
    await this.recordMaterialization({ operationId, digest, occurrence, source, generation }, result, [fact_id]);
    await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.structure_revision=coalesce(m.structure_revision,0)+1`);
    return result;
  }
  /** Relation adjudication seam (D52, D53). A judge-approved claim becomes a Fact
   * only through the validated path. When the extraction provider judges relations,
   * every validated claim of the source first gets verdicts against its ACTIVE
   * same-entity Facts: the premise is persisted once per occurrence so the verdict
   * binds to a fixed digest, and no Fact of the source is written while a verdict
   * is still owed. Verdicts are applied mechanically: CONTRASTS and INVALIDATES
   * new->candidate (never onto an invalidator), a duplicate suppresses the write. */
  private premiseTx(tx: ManagedTransaction, occurrence: string, validated: ValidatedSemanticClaim, pass: MaterializationPass) {
    return this.factRelationVerdictTx(tx, { occurrence, pipeline_id: pass.pipelineId, source: pass.source, generation: pass.judge.generation_id, validated });
  }
  /** One admitted claim resolved against the source: entity mentions (existing, new with an id allocated once per key, or
   * unresolved), the claim's time, and the semantic-claim validation verdict. */
  private async prepareClaimTx(tx: ManagedTransaction, { decision, extracted, confidence }: AdmittedClaim, pass: MaterializationPass) {
    const { judge, source, language, allocatedEntities } = pass;
    const occurrence = extractionBodyDigest([judge.generation_id, judge.source_id, judge.id, decision.claim_index]);
    const resolutions: SemanticResolution["entity_resolutions"] = [], references: { mention: string; entity_id: string | null }[] = [];
    for (const entity of extracted.entities ?? []) {
      if (references.some(reference => reference.mention === entity.mention)) continue;
      const key = extractionBodyDigest({ generation: judge.generation_id, normalized_name: entity.normalized_name, entity_kind: entity.entity_kind });
      const existing = await tx.run(`MATCH (e:Entity {generation:$generation,entity_key:$key}) RETURN e.id AS id LIMIT 1`, { generation: judge.generation_id, key });
      const known = existing.records[0]?.get("id");
      if (typeof known === "string") { resolutions.push({ status: "existing", mention: entity.mention, entity_id: known }); references.push({ mention: entity.mention, entity_id: known }); }
      else if (!source.content.includes(entity.normalized_name)) { resolutions.push({ status: "unresolved", mention: entity.mention }); references.push({ mention: entity.mention, entity_id: null }); }
      else {
        const entity_id = allocatedEntities.get(key) ?? uuidv7();
        allocatedEntities.set(key, entity_id);
        resolutions.push({ status: "new", mention: entity.mention, entity_id, normalized_name: entity.normalized_name, entity_kind: entity.entity_kind, entity_key: key }); references.push({ mention: entity.mention, entity_id });
      }
    }
    const time = extracted.time ? semanticClaimTime(extracted.time) : { time_value: source.time.time_value, time_utc: source.time.time_utc, time_precision: "inherited" as const, resolution: "inherited" as const, anchor_time_utc: source.time.time_utc };
    const semantic = { content: extracted.text, content_language: language, sub_kind: extracted.sub_kind ?? "fact", modality: extracted.speech_act ?? "asserted",
      confidence, time, entities: references, subject_keys: null, predicate_text: [...extracted.text.normalize("NFC")].slice(0, 256).join(""),
      scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] }, scope_complete: false,
      evidence: { kind: "source_locus" as const, span: { start: decision.evidence.start, end: decision.evidence.end } } };
    const digest = extractionBodyDigest(semantic);
    const resolution = SemanticResolution.parse({ entity_resolutions: resolutions, attribution_speakers: [], allow_no_single_locus: false, content_language: language });
    try {
      const validated = validateSemanticClaim(semantic, { generation: judge.generation_id, fact_language_policy: "source", allow_no_single_locus: false,
        episode: { ...source, content_language: language }, entity_resolutions: resolutions, attribution_speakers: [] });
      return { occurrence, digest, resolution, validated, refused: null };
    } catch (error) {
      if (!(error instanceof SemanticClaimValidationError)) throw error;
      return { occurrence, digest, resolution, validated: null, refused: error.code };
    }
  }
  /** Phase 1 (relation judge on): bind every validated claim's premise; true while any verdict is still owed. */
  private async bindVerdictsTx(tx: ManagedTransaction, admitted: AdmittedClaim[], pass: MaterializationPass): Promise<boolean> {
    let pending = false;
    for (const candidate of admitted) {
      const claim = await this.prepareClaimTx(tx, candidate, pass);
      if (claim.validated && await this.premiseTx(tx, claim.occurrence, claim.validated, pass) === "pending") pending = true;
    }
    return pending;
  }
  /** Phase 2: every operation of the source in this transaction, so a retry either sees custody or repeats all of it.
   * Claims are re-resolved in order, so an Entity created by an earlier claim of this source is reused rather than duplicated. */
  private async writeAdmittedClaimsTx(tx: ManagedTransaction, admitted: AdmittedClaim[], pass: MaterializationPass, policy: PolicyState) {
    const { judge, source, profile } = pass;
    const factIds: string[] = [], refused: string[] = [], duplicates: string[] = [];
    const contentFree = (occurrence: string, digest: string, result: MaterializationStateEntry["result"]) =>
      this.recordMaterialization({ operationId: uuidv7(), digest, occurrence, source, generation: judge.generation_id }, result);
    for (const candidate of admitted) {
      const { occurrence, digest, resolution, validated, refused: refusal } = await this.prepareClaimTx(tx, candidate, pass);
      if (!validated) {
        // A refused claim is retained as a content-free operation so the pipeline stays idempotent and auditable.
        await contentFree(`refused:${occurrence}`, digest, { created: false, refused: refusal });
        refused.push(refusal);
        continue;
      }
      const judgements = this.core.relationJudge ? await this.premiseTx(tx, occurrence, validated, pass) : null;
      if (judgements === "pending") throw new ExtractionAuditError("extraction_audit_conflict");
      const judged = judgements ? await this.decideFactRelationsTx(tx, judge.generation_id, judgements) : null;
      if (judged?.duplicate_of) {
        await contentFree(`duplicate:${occurrence}`, digest, { created: false, duplicate_of: judged.duplicate_of, relations: judged.decisions });
        duplicates.push(judged.duplicate_of);
        continue;
      }
      const allocated = { fact_id: uuidv7(), link_id: uuidv7(), mention_ids: validated.identity.entity_ids.map(() => uuidv7()) };
      const result = await this.writeValidatedFactTx(tx, { validated, resolution, source, generation: judge.generation_id, profile, policy: policy.policy_revision,
        operationId: uuidv7(), digest, occurrence, proposalId: null, allocated, ...(judged ? { relations: judged.decisions } : {}) });
      if (result.created) for (const link of judged?.links ?? [])
        await this.elements.mergeLinkTx(tx, MemoryLink.parse({ id: link.id, from: result.fact_id, to: link.to, role: link.role, content: link.content, weight: link.weight }));
      if (!factIds.includes(result.fact_id)) factIds.push(result.fact_id);
    }
    return { factIds, refused, duplicates };
  }
  private async materializeExtractionPipelineTx(tx: ManagedTransaction, pipeline: {
    claim: ModelTask; claim_attempt: ExtractionAttempt | null; judge: ModelTask | null; judge_attempt: ExtractionAttempt | null; decisions: ExtractionDisposition[];
  }, policy: PolicyState): Promise<{ semantic_writes: boolean; relation_judge?: "disabled" | "pending" | "complete" | "omitted" }> {
    const judge = pipeline.judge_attempt, claim = pipeline.claim_attempt;
    if (!judge || judge.state !== "succeeded" || !judge.output || !claim || claim.state !== "succeeded" || !claim.output || !pipeline.judge) return { semantic_writes: false };
    const relationJudge = this.core.relationJudge ? "complete" as const : "disabled" as const;
    const claimOutput = ExtractionModelOutput.parse(JSON.parse(claim.output.canonical_body));
    if (claimOutput.task !== "claim") throw new ExtractionAuditError("extraction_audit_conflict");
    const judgeOutput = ExtractionModelOutput.parse(JSON.parse(judge.output.canonical_body));
    if (judgeOutput.task !== "judge_claims") throw new ExtractionAuditError("extraction_audit_conflict");
    const admitted = admittedClaims(pipeline.decisions, claimOutput);
    // Per-source custody is written exactly once; readiness requires every covered
    // source to carry custody even when the judge admitted nothing or every claim was refused.
    const custody = extractionBodyDigest([judge.generation_id, judge.source_id]);
    const priorCustody = await this.committedOperationTx(tx, await this.core.materializationState.byOccurrence(custody));
    if (priorCustody) {
      const result = SourceMaterializationResult.parse(priorCustody.result);
      return { semantic_writes: result.created === true, relation_judge: result.omitted === "relation_judge_exhausted" ? "omitted" : relationJudge };
    }
    const source = await this.receipts.semanticEpisodeTx(tx, judge.source_id, policy);
    if (judge.source_revision !== source.revision_key || judge.body_digest !== source.content_digest || judge.source_ingest_seq !== source.ingest_seq)
      throw new ExtractionAuditError("extraction_audit_stale");
    const reported = claimOutput.language.toLowerCase();
    const language = /^[a-z]{2,8}(?:-[a-z0-9]{1,8})*$/.test(reported) ? reported : "und";
    const profile = pipeline.judge.model;
    const pass: MaterializationPass = { judge, pipelineId: pipeline.claim.id, source, language, profile, allocatedEntities: new Map() };
    if (this.core.relationJudge && await this.bindVerdictsTx(tx, admitted, pass)) return { semantic_writes: false, relation_judge: "pending" };
    const { factIds, refused, duplicates } = await this.writeAdmittedClaimsTx(tx, admitted, pass, policy);
    const created = await custodyRecord(this.core, custody, pass, { factIds, refused, duplicates });
    this.core.audit("extraction.pipeline.materialized", { pipeline_id: pipeline.claim.id, generation_id: judge.generation_id,
      source_id: judge.source_id, task_id: judge.task_id, kind: "judge", semantic_writes: created,
      relation_judge: relationJudge, facts: factIds.length, refused: refused.length, duplicates: duplicates.length });
    return { semantic_writes: created, relation_judge: relationJudge };
  }
  /** One validated claim's relation premise: its ACTIVE same-entity Facts of the
   * generation (cap 16), persisted once per occurrence in the extraction journal so
   * the provider's verdict binds to a fixed digest. Returns the recorded judgements,
   * `[]` when there is nobody to compare against, or "pending" while the verdict is owed. */
  private async factRelationVerdictTx(tx: ManagedTransaction, input: {
    occurrence: string; pipeline_id: string; source: SemanticReviewPremises["source"]; generation: string; validated: ValidatedSemanticClaim;
  }): Promise<FactRelationJudgement[] | "pending"> {
    const { occurrence, generation, validated } = input;
    const entry = await this.core.extractionJournal.get(input.pipeline_id);
    if (!entry) throw new ExtractionAuditError("extraction_audit_conflict");
    const known = entry.relations?.verdicts.find(verdict => verdict.key === occurrence);
    if (known) {
      if (known.context.candidates.length === 0) return [];
      return known.judgements ?? "pending";
    }
    // Vector neighbours are not consulted: no Fact vector index exists in this schema.
    const rows = await tx.run(`MATCH (f:Fact {generation:$generation})-[:MENTIONS]->(e:Entity) WHERE e.id IN $entities
      AND NOT EXISTS { MATCH ()-[inv:INVALIDATES]->(f) WHERE inv.id IS NOT NULL }
      RETURN DISTINCT f.id AS id, f.content AS text, f.time_value AS value, f.time_precision AS precision ORDER BY id LIMIT 16`,
      { generation, entities: validated.identity.entity_ids });
    const candidates = rows.records.map(record => ({ id: record.get("id"), text: record.get("text"), time: { value: record.get("value"), precision: record.get("precision") } }));
    const body = { fact: { text: validated.claim.content, time: validatedFactTime(validated, input.source) }, candidates };
    const context = FactRelationContext.parse({ body_digest: extractionBodyDigest(body), ...body });
    const verdicts = [...(entry.relations?.verdicts ?? []), {
      key: occurrence, context, judgements: null, failures: 0, last_failure: null, last_failure_detail: null,
      model: null, model_incarnation: null, reported_model: null,
    }];
    await this.core.extractionJournal.set(input.pipeline_id, { ...entry, relations: {
      context_digest: extractionBodyDigest(verdicts.map(({ key, context }) => ({ key, context_digest: context.body_digest }))), verdicts,
    } });
    return candidates.length ? "pending" : [];
  }
  /** Mechanical application of relation verdicts, read-only: the confidence floor,
   * candidate staleness and the non-recursive INVALIDATES rule decide each outcome;
   * link ids are allocated here and written only once the new Fact exists. */
  private async decideFactRelationsTx(tx: ManagedTransaction, generation: string, judgements: FactRelationJudgement[]): Promise<{
    decisions: FactRelationDecision[]; duplicate_of: string | null; links: { id: string; to: string; role: "CONTRASTS" | "INVALIDATES"; content: string; weight: number }[];
  }> {
    const decisions: FactRelationDecision[] = [], links: { id: string; to: string; role: "CONTRASTS" | "INVALIDATES"; content: string; weight: number }[] = [];
    let duplicate_of: string | null = null;
    for (const judgement of judgements) {
      const base = { candidate_id: judgement.candidate_id, relation: judgement.relation, confidence: judgement.confidence, reason: judgement.reason };
      if (judgement.relation === "unrelated") { decisions.push({ ...base, outcome: "unrelated" }); continue; }
      if (judgement.confidence < 0.6) { decisions.push({ ...base, outcome: "low_confidence" }); continue; }
      const state = await tx.run(`MATCH (c:Fact {id:$id,generation:$generation})
        RETURN EXISTS { MATCH ()-[inv:INVALIDATES]->(c) WHERE inv.id IS NOT NULL } AS invalidated, EXISTS { MATCH (c)-[inv:INVALIDATES]->() WHERE inv.generation=$generation } AS invalidator`,
        { id: judgement.candidate_id, generation });
      const candidate = state.records[0];
      if (!candidate || candidate.get("invalidated") === true) { decisions.push({ ...base, outcome: "stale_candidate" }); continue; }
      if (judgement.relation === "duplicate") { duplicate_of ??= judgement.candidate_id; decisions.push({ ...base, outcome: "duplicate" }); continue; }
      if (judgement.relation === "invalidates" && candidate.get("invalidator") === true) { decisions.push({ ...base, outcome: "chain_refused" }); continue; }
      const id = uuidv7();
      links.push({ id, to: judgement.candidate_id, role: judgement.relation === "invalidates" ? "INVALIDATES" : "CONTRASTS", content: judgement.reason, weight: judgement.confidence });
      decisions.push({ ...base, outcome: "linked", link_id: id });
    }
    // A duplicate is never written, so no verdict of it can become a link.
    if (duplicate_of) return { duplicate_of, links: [], decisions: decisions.map(decision => decision.outcome === "linked" || decision.outcome === "chain_refused"
      ? { candidate_id: decision.candidate_id, relation: decision.relation, confidence: decision.confidence, reason: decision.reason, outcome: "duplicate" } : decision) };
    return { duplicate_of, links, decisions };
  }
  async readExtractionPipelineTx(tx: ManagedTransaction,id: string): Promise<ExtractionPipeline> {
    const entry = await this.core.extractionJournal.get(id);
    if (!entry?.claim.pipeline) return {state:'unknown',pipeline_id:id};
    const { claim, judge, claim_attempt: claimAttempt, judge_attempt: judgeAttempt } = entry;
    const decisions = judgeAttempt ? await this.extraction.extractionDecisionsTx(tx,judgeAttempt) : [];
    const materialized = judgeAttempt && judgeAttempt.state === "succeeded"
      ? await this.materializeExtractionPipelineTx(tx, { claim, claim_attempt: claimAttempt, judge, judge_attempt: judgeAttempt, decisions }, await this.core.receiptLockTx(tx))
      : { semantic_writes: false };
    return ExtractionPipeline.parse({state:'known',pipeline_id:id,mode:'claim-judge-audit-v1',semantic_writes:materialized.semantic_writes,claim,claim_attempt:claimAttempt,judge,judge_attempt:judgeAttempt,decisions,
      ...(materialized.relation_judge ? { relation_judge: materialized.relation_judge } : {})});
  }
  /** Retention invariant: both coverage partitions committed the terminal source;
   * custody (including content-free custody) or a sealed omission makes replay unnecessary.
   * No custody means relation verdicts may still be pending, so that entry stays. */
  async pruneExtractionJournal(context: InstallationContext, filter: { generation_id?: string; pipeline_id?: string }): Promise<void> {
    await this.core.extractionTx(context, async tx => {
      for (const [id, entry] of await this.core.extractionJournal.list()) {
        if ((filter.generation_id && entry.generation_id !== filter.generation_id) || (filter.pipeline_id && id !== filter.pipeline_id)) continue;
        const generation = await this.core.extractionRecordTx(tx, "ExtractionGeneration", entry.generation_id, Generation);
        if (entry.claim.source_ingest_seq > generation.covered_ingest_seq) continue;
        const sealed = { ...entry, sealed_ingest_seq: generation.covered_ingest_seq };
        const omission = extractionEntryOmission(entry);
        const standalone = !entry.claim.pipeline && entry.claim_attempt !== null && !["queued", "leased"].includes(entry.claim.state);
        const custody = await this.committedOperationTx(tx, await this.core.materializationState.byOccurrence(extractionBodyDigest([entry.generation_id, entry.source_id])));
        if (!omission && !standalone && !custody) {
          if (entry.sealed_ingest_seq !== sealed.sealed_ingest_seq) await this.core.extractionJournal.set(id, sealed);
          continue;
        }
        await this.core.extractionJournal.delete(id);
        await this.core.materializationState.deleteSource(entry.generation_id, entry.source_id);
        const task = entry.judge ?? entry.claim;
        this.core.audit("extraction.pipeline.pruned", { pipeline_id: id, generation_id: entry.generation_id,
          source_id: entry.source_id, task_id: task.id, kind: task.kind === "claim" ? "claim" : "judge", sealed_ingest_seq: sealed.sealed_ingest_seq });
      }
      await this.pruneOrphanedMaterializationsTx(tx, filter.generation_id);
    });
  }
  /** Reconcile a crash between the journal deletion and its operation deletion;
   * both coverage cursors were committed before either file was pruned. */
  private async pruneOrphanedMaterializationsTx(tx: ManagedTransaction, generationId: string | undefined): Promise<void> {
    for (const [, entry] of await this.core.materializationState.list()) {
      if (generationId && entry.generation_id !== generationId) continue;
      if (await this.core.extractionJournal.byWorkKey(`${entry.generation_id}:${entry.source_episode_id}`)) continue;
      const generation = await this.core.extractionRecordTx(tx, "ExtractionGeneration", entry.generation_id, Generation);
      if (entry.source_ingest_seq <= generation.covered_ingest_seq)
        await this.core.materializationState.deleteSource(entry.generation_id, entry.source_episode_id);
    }
  }
  /** Seals a validated-but-unjudged source as a terminal omission (D53): the relation judge has failed on every
   * premise at least `min_failures` times, so the source's custody operation is written content-free with the
   * omission recorded, no Fact is written, and coverage/activation see custody like any refused source. Idempotent;
   * refuses while a verdict is still owed within budget or when the pipeline is not at the relation stage. */
  async sealFactRelationOmission(request: { pipeline_id: string; min_failures: number }, context: InstallationContext): Promise<{ sealed: boolean; failures: number }> {
    const pipelineId = z.uuidv7().parse(request.pipeline_id), minFailures = z.number().int().positive().parse(request.min_failures);
    const result = await this.core.extractionTx(context, async (tx, policy) => {
      const pipeline = await this.readExtractionPipelineTx(tx, pipelineId);
      if (pipeline.state !== "known" || !pipeline.judge || pipeline.judge.state !== "succeeded") throw new Error("invalid_transition");
      await this.core.authorizeEpisodesTx(tx, [pipeline.claim.source_id], policy);
      await this.core.writableExtractionGenerationTx(tx, pipeline.claim.generation_id);
      const custody = extractionBodyDigest([pipeline.claim.generation_id, pipeline.claim.source_id]);
      const pending = pendingRelations(await this.core.extractionJournal.get(pipelineId));
      const failures = pending.reduce((max, verdict) => Math.max(max, verdict.failures), 0);
      if (pipeline.relation_judge === "omitted") return { sealed: false, failures };
      if (pipeline.relation_judge !== "pending") throw new Error("invalid_transition");
      if (failures < minFailures) throw new Error("relation_omission_premature");
      const occurrences = pending.map(verdict => verdict.key).sort();
      await this.recordMaterialization({ operationId: uuidv7(),
        digest: extractionBodyDigest({ generation: pipeline.claim.generation_id, source: pipeline.claim.source_id, judge: pipeline.judge.id, omitted: "relation_judge_exhausted", occurrences }),
        occurrence: custody, source: { id: pipeline.claim.source_id, ingest_seq: pipeline.claim.source_ingest_seq }, generation: pipeline.claim.generation_id },
        { created: false, facts: 0, refused: [], omitted: "relation_judge_exhausted", failures, occurrences });
      this.core.audit("extraction.pipeline.materialized", { pipeline_id: pipelineId, generation_id: pipeline.claim.generation_id,
        source_id: pipeline.claim.source_id, task_id: pipeline.judge.id, kind: "judge", semantic_writes: false,
        relation_judge: "omitted", facts: 0, refused: 0, duplicates: 0 });
      return { sealed: true, failures };
    });
    await this.pruneExtractionJournal(context, { pipeline_id: pipelineId });
    return result;
  }
  /** Relation premises of one pipeline that still owe a verdict (candidates present, none recorded). */
  async pendingFactRelationInputs(pipelineId: string, context: InstallationContext): Promise<{ key: string; context: FactRelationContext }[]> {
    return this.core.extractionTx(context, async (tx, policy) => {
      const entry = await this.core.extractionJournal.get(z.uuidv7().parse(pipelineId));
      const pending = pendingRelations(entry);
      if (entry && pending.length) await this.core.authorizeEpisodesTx(tx, [entry.source_id], policy);
      return pending.map(verdict => ({ key: verdict.key, context: verdict.context })).sort((a, b) => a.key.localeCompare(b.key));
    });
  }
  /** Highest recorded provider-failure count among the premises of one pipeline that still owe a verdict;
   * the scheduler seals a source whose relation judge keeps failing as a terminal omission (D53). */
  async factRelationFailures(pipelineId: string, context: InstallationContext): Promise<number> {
    return this.core.extractionTx(context, async () => {
      const entry = await this.core.extractionJournal.get(z.uuidv7().parse(pipelineId));
      return pendingRelations(entry).reduce((max, verdict) => Math.max(max, verdict.failures), 0);
    });
  }
  /** Records the provider's answer for one premise: a verdict bound to the premise
   * digest (written once), or the failure reason that keeps the pipeline pending. */
  async recordFactRelationVerdict(input: { key: string } & ({ judgements: FactRelationJudgement[]; model: string; model_incarnation: string; reported_model?: string } | { failure: string; detail?: ExtractionFailureDetail }), context: InstallationContext): Promise<void> {
    await this.core.extractionTx(context, async (tx, policy) => {
      const entry = await this.core.extractionJournal.byRelation(input.key);
      const relations = entry?.relations;
      const premise = relations?.verdicts.find(verdict => verdict.key === input.key);
      if (!entry || !relations || !premise) throw new ExtractionAuditError("extraction_audit_conflict");
      await this.core.authorizeEpisodesTx(tx, [entry.source_id], policy);
      const updated = "failure" in input
        ? { ...premise, last_failure: input.failure, last_failure_detail: input.detail ?? null, failures: premise.failures + 1 }
        : { ...premise, last_failure: null, last_failure_detail: null,
          ...(premise.judgements === null ? { judgements: z.array(FactRelationJudgement).max(16).parse(input.judgements),
            model: input.model, model_incarnation: input.model_incarnation, reported_model: input.reported_model ?? null } : {}) };
      await this.core.extractionJournal.set(entry.claim.id, { ...entry,
        relations: { ...relations, verdicts: relations.verdicts.map(verdict => verdict.key === input.key ? updated : verdict) } });
    });
  }
}

/** Only a matching immutable failed/cancelled attempt is a coverable omission. */
export function extractionEntryOmission(entry: ExtractionJournalEntry) {
  for (const [stage, task, attempt] of [
    ["claim", entry.claim, entry.claim_attempt],
    ["judge", entry.judge, entry.judge_attempt],
  ] as const) {
    if (stage === "judge" && entry.claim.state !== "succeeded") continue;
    if (task && attempt && (task.state === "failed" || task.state === "cancelled")
      && attempt.state === task.state && attempt.id === task.attempt_id && attempt.task_id === task.id) {
      return { pipeline_id: entry.claim.id, stage, attempt_id: attempt.id, state: attempt.state, reason: attempt.reason };
    }
  }
  return null;
}
