import { readFile, rename, writeFile } from "node:fs/promises";

export type StateFileCodec<Entry> = {
  parse(text: string): Record<string, Entry>;
  serialize(entries: Record<string, Entry>): string;
};

/** Serial, write-through state keyed by id. Every change rewrites the whole file through a tmp+rename so a crash
 * leaves either the previous or the next complete file; without a path the state lives in memory only. */
export class StateFile<Entry> {
  private loaded: Promise<void> | undefined;
  private data: Record<string, Entry> = {};
  private writes: Promise<void> = Promise.resolve();

  constructor(private readonly path: string | undefined, private readonly codec: StateFileCodec<Entry>) {}

  /** The entries once every earlier write has settled. Callers must treat the record as read-only. */
  async entries(): Promise<Record<string, Entry>> {
    await this.load(); await this.writes;
    return this.data;
  }

  async mutate(change: (entries: Record<string, Entry>) => void): Promise<void> {
    await this.load();
    const next = this.writes.then(async () => {
      const entries = { ...this.data };
      change(entries);
      if (this.path) {
        await writeFile(`${this.path}.tmp`, this.codec.serialize(entries));
        await rename(`${this.path}.tmp`, this.path);
      }
      this.data = entries;
    });
    // A failed write rejects its caller; later reads and writes see the state before it.
    this.writes = next.catch(() => {});
    await next;
  }

  private async load(): Promise<void> {
    if (!this.loaded) this.loaded = (async () => {
      if (!this.path) return;
      try { this.data = this.codec.parse(await readFile(this.path, "utf8")); }
      catch (error) {
        if (error instanceof Error && "code" in error && error.code === "ENOENT") return;
        throw error;
      }
    })();
    await this.loaded;
  }
}
