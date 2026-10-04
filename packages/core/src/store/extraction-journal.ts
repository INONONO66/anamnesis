import { ExtractionAttempt, ExtractionDisposition, ExtractionJudgeInput, ModelTask } from "@anamnesis/protocol";
import { z } from "zod";
import { StateFile } from "./state-file.ts";

const Entry = z.strictObject({
  work_key: z.string(),
  generation_id: z.uuidv7(),
  source_id: z.uuidv7(),
  claim: ModelTask,
  claim_attempt: ExtractionAttempt.nullable(),
  judge: ModelTask.nullable(),
  judge_input: ExtractionJudgeInput.nullable(),
  judge_attempt: ExtractionAttempt.nullable(),
  decisions: z.array(ExtractionDisposition).max(64),
  attempts: z.array(ExtractionAttempt).max(32),
  // Original requests cannot be reconstructed after provider adoption or a
  // server-rewritten outcome (policy denial, changed premises, judge mismatch).
  creation_digest: z.string().regex(/^[0-9a-f]{64}$/).optional(),
  // One digest per retained attempt: the 32 historical outcomes plus the pinned claim and judge attempts.
  request_digests: z.record(z.uuidv7(), z.string().regex(/^[0-9a-f]{64}$/))
    .refine(digests => Object.keys(digests).length <= 34, "too many request digests").optional(),
  sealed_ingest_seq: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER).nullable(),
}).refine(entry => entry.work_key === `${entry.generation_id}:${entry.source_id}`, "invalid work key");
export type ExtractionJournalEntry = z.infer<typeof Entry>;

/** The attempt an entry still holds under `id`: the pinned claim and judge attempts outlive the bounded history. */
export function journalAttempt(entry: ExtractionJournalEntry, id: string): ExtractionAttempt | undefined {
  for (const attempt of [entry.claim_attempt, entry.judge_attempt, ...entry.attempts]) if (attempt?.id === id) return attempt;
  return undefined;
}

const FileSchema = z.strictObject({
  version: z.literal(1),
  pipelines: z.record(z.uuidv7(), Entry),
});
type JournalFile = z.infer<typeof FileSchema>;

/** Serial write-through operational state for extraction pipelines. */
export class ExtractionJournal {
  private readonly file: StateFile<ExtractionJournalEntry>;

  constructor(path?: string) {
    this.file = new StateFile(path, {
      parse: text => FileSchema.parse(JSON.parse(text)).pipelines,
      serialize: pipelines => JSON.stringify({ version: 1, pipelines } satisfies JournalFile),
    });
  }

  async get(pipelineId: string): Promise<ExtractionJournalEntry | undefined> {
    const entry = (await this.file.entries())[pipelineId];
    return entry && structuredClone(entry);
  }

  async list(): Promise<[string, ExtractionJournalEntry][]> {
    return Object.entries(await this.file.entries()).map(([id, entry]) => [id, structuredClone(entry)]);
  }

  async byWorkKey(workKey: string): Promise<ExtractionJournalEntry | undefined> {
    return (await this.list()).find(([, entry]) => entry.work_key === workKey)?.[1];
  }

  async byAttempt(attemptId: string): Promise<ExtractionJournalEntry | undefined> {
    return (await this.list()).find(([, entry]) => entry.claim.attempt_id === attemptId ||
      entry.judge?.attempt_id === attemptId || entry.attempts.some(attempt => attempt.id === attemptId))?.[1];
  }

  async byTask(taskId: string): Promise<ExtractionJournalEntry | undefined> {
    return (await this.list()).find(([, entry]) => entry.claim.id === taskId || entry.judge?.id === taskId)?.[1];
  }

  async pending(generationId: string): Promise<[string, ExtractionJournalEntry][]> {
    return (await this.list()).filter(([, entry]) => entry.generation_id === generationId &&
      (entry.claim.state === "queued" || entry.claim.state === "leased" ||
       entry.judge?.state === "queued" || entry.judge?.state === "leased"));
  }

  async set(pipelineId: string, entry: ExtractionJournalEntry): Promise<void> {
    z.uuidv7().parse(pipelineId);
    await this.file.mutate(pipelines => { pipelines[pipelineId] = Entry.parse(entry); });
  }

  async delete(pipelineId: string): Promise<void> {
    await this.file.mutate(pipelines => { delete pipelines[pipelineId]; });
  }
}
