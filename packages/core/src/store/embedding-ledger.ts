import { readFile, rename, writeFile } from "node:fs/promises";
import { z } from "zod";
import { RpcEmbeddingAttempt } from "@anamnesis/protocol";

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
  private loaded: Promise<void> | undefined;
  private data: LedgerFile = { version: 1, episodes: {} };
  private writes: Promise<void> = Promise.resolve();

  constructor(private readonly path?: string) {}

  private async load(): Promise<void> {
    if (!this.loaded) this.loaded = (async () => {
      if (!this.path) return;
      try { this.data = FileSchema.parse(JSON.parse(await readFile(this.path, "utf8"))); }
      catch (error) {
        if (error instanceof Error && "code" in error && error.code === "ENOENT") return;
        throw error;
      }
    })();
    await this.loaded;
  }

  async get(id: string): Promise<EmbeddingLedgerEntry | undefined> {
    await this.load(); await this.writes;
    return this.data.episodes[id];
  }

  async list(): Promise<[string, EmbeddingLedgerEntry][]> {
    await this.load(); await this.writes;
    return Object.entries(this.data.episodes);
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
    await this.mutate(episodes => { episodes[id] = Entry.parse(entry); });
  }

  async delete(id: string): Promise<void> {
    await this.mutate(episodes => { delete episodes[id]; });
  }

  private async mutate(change: (episodes: LedgerFile["episodes"]) => void): Promise<void> {
    await this.load();
    const next = this.writes.catch(() => {}).then(async () => {
      const episodes = { ...this.data.episodes };
      change(episodes);
      const updated: LedgerFile = { version: 1, episodes };
      if (this.path) {
        await writeFile(`${this.path}.tmp`, JSON.stringify(updated));
        await rename(`${this.path}.tmp`, this.path);
      }
      this.data = updated;
    });
    this.writes = next;
    await next;
  }
}
