import { createReadStream } from "node:fs";
import { mkdir, open, readdir } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { z } from "zod";
import { RememberInput } from "./engine.ts";
import type { PutResult } from "./store.ts";

const PersistedElement = RememberInput.extend({
  payload: z.array(z.number().int().min(0).max(255)).optional(),
});

const JournalEntry = z
  .object({
    recordedAt: z.iso.datetime(),
    element: PersistedElement,
  })
  .strict();

type PersistedElement = z.infer<typeof PersistedElement>;

export interface ReplayOptions {
  signal?: AbortSignal;
}

interface Rememberer {
  remember(input: RememberInput): Promise<PutResult>;
}

/** Append-only origin journal; graph state is rebuilt from these records. */
export class EpisodeJournal {
  constructor(
    private readonly directory: string,
    private readonly clock: () => Date = () => new Date(),
  ) {}

  async append(input: object): Promise<void> {
    const parsed = RememberInput.parse(input);
    const { payload, ...fields } = parsed;
    const recordedAt = this.clock().toISOString();
    const element: PersistedElement = {
      ...fields,
      ...(payload ? { payload: Array.from(payload) } : {}),
    };
    const line = `${JSON.stringify({ recordedAt, element })}\n`;
    const month = recordedAt.slice(0, 7);

    await mkdir(this.directory, { recursive: true });
    const file = await open(join(this.directory, `journal-${month}.jsonl`), "a");
    try {
      await file.write(line);
      await file.sync();
    } finally {
      await file.close();
    }
  }

  async replay(engine: Rememberer, opts: ReplayOptions = {}): Promise<number> {
    const names = (await readdir(this.directory))
      .filter((name) => /^journal-\d{4}-\d{2}\.jsonl$/.test(name))
      .sort();
    let replayed = 0;

    for (const name of names) {
      const lines = createInterface({
        input: createReadStream(join(this.directory, name), { encoding: "utf8" }),
        crlfDelay: Infinity,
      });
      for await (const line of lines) {
        opts.signal?.throwIfAborted();
        const entry = JournalEntry.parse(JSON.parse(line));
        const { payload, ...fields } = entry.element;
        await engine.remember({
          ...fields,
          ...(payload ? { payload: Uint8Array.from(payload) } : {}),
        });
        replayed += 1;
      }
    }

    return replayed;
  }
}

/** Persist the origin before deriving graph state so failed ingestion is replayable. */
export async function journaledRemember(
  journal: EpisodeJournal,
  engine: Rememberer,
  input: RememberInput,
): Promise<PutResult> {
  await journal.append(input);
  return engine.remember(input);
}
