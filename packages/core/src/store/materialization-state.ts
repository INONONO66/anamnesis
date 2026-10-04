import { FactRelationDecision, MaterializationResult } from "@anamnesis/protocol";
import { z } from "zod";
import { StateFile } from "./state-file.ts";

const hash = z.string().regex(/^[0-9a-f]{64}$/);
export const SourceMaterializationResult = z.strictObject({
  created: z.boolean(), facts: z.number().int().min(0).max(64),
  refused: z.array(z.string()).max(64), duplicates: z.array(z.uuidv7()).max(64).optional(),
  omitted: z.literal("relation_judge_exhausted").optional(),
  failures: z.number().int().nonnegative().optional(), occurrences: z.array(hash).max(64).optional(),
});
const Entry = z.strictObject({
  request_digest: hash, occurrence_key: z.string().min(1),
  generation_id: z.uuidv7(), source_episode_id: z.uuidv7(), source_ingest_seq: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
  // Written before the graph commit. Positive results are replayable only once
  // these Facts and their source links exist; rollback leaves a retryable intent.
  fact_ids: z.array(z.uuidv7()).max(64),
  result: z.union([
    MaterializationResult, SourceMaterializationResult,
    z.strictObject({ created: z.literal(false), refused: z.string() }),
    z.strictObject({ created: z.literal(false), duplicate_of: z.uuidv7(), relations: z.array(FactRelationDecision).max(16) }),
  ]),
});
export type MaterializationStateEntry = z.infer<typeof Entry>;
const File = z.strictObject({ version: z.literal(1), operations: z.record(z.uuidv7(), Entry) });

/** Bounded by unsealed source work, like ExtractionJournal. Source records are
 * removed together only after both coverage partitions and materialization settle. */
export class MaterializationState {
  private readonly file: StateFile<MaterializationStateEntry>;

  constructor(path?: string) {
    this.file = new StateFile(path, {
      parse: text => File.parse(JSON.parse(text)).operations,
      serialize: operations => JSON.stringify({ version: 1, operations }),
    });
  }

  async get(id: string): Promise<MaterializationStateEntry | undefined> {
    const entry = (await this.file.entries())[id];
    return entry && structuredClone(entry);
  }

  async list(): Promise<[string, MaterializationStateEntry][]> {
    return Object.entries(await this.file.entries()).map(([id, entry]) => [id, structuredClone(entry)]);
  }

  async byOccurrence(key: string): Promise<MaterializationStateEntry | undefined> {
    return (await this.list()).find(([, entry]) => entry.occurrence_key === key)?.[1];
  }

  async set(id: string, value: MaterializationStateEntry): Promise<void> {
    z.uuidv7().parse(id);
    const entry = Entry.parse(value);
    await this.file.mutate(operations => {
      for (const [priorId, prior] of Object.entries(operations))
        if (priorId !== id && prior.occurrence_key === entry.occurrence_key) delete operations[priorId];
      operations[id] = entry;
    });
  }

  async deleteSource(generation: string, source: string): Promise<void> {
    await this.file.mutate(operations => {
      for (const [id, entry] of Object.entries(operations))
        if (entry.generation_id === generation && entry.source_episode_id === source) delete operations[id];
    });
  }
}
