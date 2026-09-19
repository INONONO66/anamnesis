import { createReadStream } from "node:fs";
import { mkdir, open, readFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { z } from "zod";
import { RememberInput } from "./engine.ts";
import { validateElementSemantics } from "@anamnesis/protocol";
import { HistoricalElement, historicalEligibility, type EligibilityReason } from "./legacy-format.ts";
import type { PutResult } from "./store.ts";

const PersistedElement = z
  .object({
    time: RememberInput.shape.time,
    content: RememberInput.shape.content,
    origin: RememberInput.shape.origin,
    schema: RememberInput.shape.schema,
    mass: RememberInput.shape.mass,
    properties: RememberInput.shape.properties,
    source_revision: RememberInput.shape.source_revision,
    expected_previous_revision_key: RememberInput.shape.expected_previous_revision_key,
    previous: RememberInput.shape.previous,
    payload_media_type: RememberInput.shape.payload_media_type,
    payload: z.array(z.number().int().min(0).max(255)).optional(),
  })
  .strict()
  .superRefine(validateElementSemantics);

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

const HistoricalEntry = z.object({
  recordedAt: z.iso.datetime(),
  element: HistoricalElement.extend({
    source_revision: z.string().min(1).optional(),
    previous: z.string().min(1).optional(),
    payload_media_type: z.string().min(1).optional(),
    payload: z.array(z.number().int().min(0).max(255)).optional(),
  }),
}).strict();
export interface HistoricalInspection {
  file: string;
  offset: number;
  raw: Buffer;
  sha256: string;
  entry: z.infer<typeof HistoricalEntry>;
  eligibility: EligibilityReason[];
}

/** Append-only origin journal; graph state is rebuilt from these records. */
export class EpisodeJournal {
  constructor(
    private readonly directory: string,
    private readonly clock: () => Date = () => new Date(),
  ) {}

  /** Inventory only. Provenance is explicit, never inferred from a parse failure
   * or recordedAt. Keep the original bytes, including terminators, untouched. */
  async inspect(format: string): Promise<HistoricalInspection[]> {
    if (format !== "post167-pre194") throw new Error(`unsupported-legacy-format: ${format}`);
    const results: HistoricalInspection[] = [];
    const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
    const names = (await readdir(this.directory)).filter(name => /^journal-\d{4}-\d{2}\.jsonl$/.test(name)).sort();
    for (const file of names) {
      const bytes = await readFile(join(this.directory, file));
      for (let offset = 0; offset < bytes.length;) {
        const end = bytes.indexOf(0x0a, offset);
        if (end < 0) throw new Error(`incomplete-legacy-line: ${file}:${offset}`);
        const raw = bytes.subarray(offset, end + 1);
        const entry = HistoricalEntry.parse(JSON.parse(decoder.decode(raw)));
        results.push({ file, offset, raw, sha256: createHash("sha256").update(raw).digest("hex"),
          entry, eligibility: historicalEligibility(entry.element) });
        offset = end + 1;
      }
    }
    return results;
  }

  async append(input: object): Promise<void> {
    const parsed = RememberInput.parse(input);
    const { payload, ...fields } = parsed;
    const recordedAt = this.clock().toISOString();
    const element: PersistedElement = {
      ...fields,
      ...(payload ? { payload: Array.from(payload) } : {}),
    };
    const line = Buffer.from(`${JSON.stringify({ recordedAt, element })}\n`);
    const month = recordedAt.slice(0, 7);

    await mkdir(this.directory, { recursive: true });
    const file = await open(join(this.directory, `journal-${month}.jsonl`), "a");
    try {
      for (let offset = 0; offset < line.length;) {
        const { bytesWritten } = await file.write(line, offset, line.length - offset);
        if (bytesWritten === 0) throw new Error("Journal write made no progress");
        offset += bytesWritten;
      }
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
      const input = createReadStream(join(this.directory, name), { encoding: "utf8" });
      const lines = createInterface({ input, crlfDelay: Infinity });
      try {
        for await (const line of lines) {
          opts.signal?.throwIfAborted();
          const entry = JournalEntry.parse(JSON.parse(line));
          const { payload, ...fields } = entry.element;
          const remember = engine.remember({
            ...fields,
            ...(payload ? { payload: Uint8Array.from(payload) } : {}),
          });
          if (!opts.signal) {
            await remember;
          } else {
            let abort!: () => void;
            const cancelled = new Promise<never>((_, reject) => {
              abort = () => reject(opts.signal!.reason ?? new Error("Replay aborted"));
              opts.signal!.addEventListener("abort", abort, { once: true });
              if (opts.signal!.aborted) abort();
            });
            try {
              await Promise.race([remember, cancelled]);
            } finally {
              opts.signal!.removeEventListener("abort", abort);
            }
          }
          replayed += 1;
        }
      } finally {
        // The async iterator normally closes readline, but an engine failure or
        // abort must also release the underlying file before replay settles.
        lines.close();
        input.destroy();
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
