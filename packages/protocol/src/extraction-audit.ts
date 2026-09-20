import { z } from 'zod';
import { ExtractionSpan, ExtractionClaim, ExtractionAttempt, ModelTask, LeaseModelTask } from './extraction.ts';

const id = z.uuidv7();
const hash = z.string().regex(/^[0-9a-f]{64}$/);
const counter = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
/** This ABI judges the existing extraction audit claims, not semantic Facts.
 * Dispositions are retained observations, never authority to suppress/correct. */
export const ExtractionClaimContext = z.strictObject({
  task_id: id, attempt_id: id, body_digest: hash,
  claims: z.array(ExtractionClaim).max(64),
});
export type ExtractionClaimContext = z.infer<typeof ExtractionClaimContext>;
export const ExtractionJudgeInput = z.strictObject({
  task_id: id, attempt_id: id, pipeline_id: id, source_head_revision: hash, policy_revision: counter,
  claim_context: ExtractionClaimContext,
});
export type ExtractionJudgeInput = z.infer<typeof ExtractionJudgeInput>;
export const CreateExtractionPipeline = z.strictObject({ id, generation_id: id, source_id: id });
export type CreateExtractionPipeline = z.infer<typeof CreateExtractionPipeline>;
export const RunExtractionPipeline = LeaseModelTask;
export type RunExtractionPipeline = z.infer<typeof RunExtractionPipeline>;
export const ExtractionPipelineStatus = z.strictObject({ pipeline_id: id });
export const ExtractionDisposition = z.strictObject({
  judge_attempt_id: id, claim_attempt_id: id, claim_body_digest: hash,
  claim_index: counter.max(63), disposition: z.enum(['retain','suppress','correct','unknown']),
  evidence: ExtractionSpan, confidence: z.number().min(0).max(1).optional(),
});
export type ExtractionDisposition = z.infer<typeof ExtractionDisposition>;
export const ExtractionPipeline = z.discriminatedUnion('state', [
  z.strictObject({ state: z.literal('unknown'), pipeline_id: id }),
  z.strictObject({ state: z.literal('known'), pipeline_id: id, mode: z.literal('claim-judge-audit-v1'),
    semantic_writes: z.boolean(), claim: ModelTask, claim_attempt: ExtractionAttempt.nullable(),
    judge: ModelTask.nullable(), judge_attempt: ExtractionAttempt.nullable(),
    decisions: z.array(ExtractionDisposition).max(64),
  }),
]);
export type ExtractionPipeline = z.infer<typeof ExtractionPipeline>;
export class ExtractionAuditError extends Error {
  readonly code: 'extraction_not_configured' | 'extraction_audit_conflict' | 'extraction_audit_incomplete' | 'extraction_audit_stale';
  constructor(code: ExtractionAuditError['code']) {
    super(code); this.name = 'ExtractionAuditError'; this.code = code;
  }
}
