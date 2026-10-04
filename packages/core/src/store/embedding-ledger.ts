import { z } from "zod";
import { RpcEmbeddingAttempt } from "@anamnesis/protocol";
import { StateFile } from "./state-file.ts";

const Entry = z.strictObject({
  profile_id: z.string().regex(/^[0-9a-f]{64}$/),
  state: z.enum(["deferred", "quarantined"]),
  deferrals: z.number().int().nonnegative(),
  retry_after: z.number().int().nonnegative().nullable(),
  attempts: z.array(RpcEmbeddingAttempt).min(1).max(16).refine(
    attempts => attempts.every(attempt => attempt.state === "deferred" || attempt.state === "quarantined"),
  ),
});
export type EmbeddingLedgerEntry = z.infer<typeof Entry>;
const FileSchema = z.strictObject({
  version: z.literal(1),
  episodes: z.record(z.uuidv7(), Entry),
});
type LedgerFile = z.infer<typeof FileSchema>;

/** A serial, write-through ledger. Only completed failures live here; successful
 * vectors are represented by the graph and have no ledger entry. */
export class EmbeddingLedger {
  private readonly file: StateFile<EmbeddingLedgerEntry>;

  constructor(path?: string) {
    this.file = new StateFile(path, {
      parse: text => FileSchema.parse(JSON.parse(text)).episodes,
      serialize: episodes => JSON.stringify({ version: 1, episodes } satisfies LedgerFile),
    });
  }

  async get(id: string): Promise<EmbeddingLedgerEntry | undefined> {
    return (await this.file.entries())[id];
  }

  async list(): Promise<[string, EmbeddingLedgerEntry][]> {
    return Object.entries(await this.file.entries());
  }

  async due(profileId: string, now: number): Promise<[string, EmbeddingLedgerEntry][]> {
    return (await this.list()).filter(([, entry]) =>
      entry.profile_id === profileId && entry.state === "deferred" && entry.retry_after !== null && entry.retry_after <= now)
      .sort(([a, x], [b, y]) => x.deferrals - y.deferrals || a.localeCompare(b));
  }

  async quarantined(profileId: string, reasons?: readonly RpcEmbeddingAttempt["reason"][]): Promise<[string, EmbeddingLedgerEntry][]> {
    return (await this.list()).filter(([, entry]) => entry.profile_id === profileId && entry.state === "quarantined"
      && (!reasons || reasons.includes(entry.attempts.at(-1)?.reason ?? null)))
      .sort(([a], [b]) => a.localeCompare(b));
  }

  async set(id: string, entry: EmbeddingLedgerEntry): Promise<void> {
    await this.file.mutate(episodes => { episodes[id] = Entry.parse(entry); });
  }

  async delete(id: string): Promise<void> {
    await this.file.mutate(episodes => { delete episodes[id]; });
  }
}
