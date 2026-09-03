import { homedir } from "node:os";
import { join } from "node:path";
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import {
  MemoryElement,
  type MemoryElementInput,
  type MemoryLink,
  type MemoryLinkInput,
} from "@anamnesis/protocol";
import {
  Store,
  type IntegrityIssue,
  type PutResult,
  type SearchHit,
  type StoreOptions,
} from "./store.ts";

export const RememberInput = z
  .object(MemoryElement.shape)
  .omit({ id: true })
  .extend({
    schema: MemoryElement.shape.schema.default(
      "anamnesis.original-message/1",
    ),
    payload: z.instanceof(Uint8Array).optional(),
    payload_media_type: z.string().min(1).optional(),
    source_revision: z.string().min(1).optional(),
    /** The parent record takes precedence over inferred chronological order. */
    previous: z.string().min(1).optional(),
  })
  .strict();
export type RememberInput = z.input<typeof RememberInput>;

export interface EngineOptions extends Partial<StoreOptions> {}

export function envConfig(): StoreOptions {
  return {
    uri: process.env["ANAMNESIS_NEO4J_URI"] ?? "bolt://127.0.0.1:7687",
    user: process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j",
    password: process.env["ANAMNESIS_NEO4J_PASSWORD"] ?? "anamnesis",
    database: process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j",
    objectsRoot:
      process.env["ANAMNESIS_OBJECTS_ROOT"] ??
      join(homedir(), ".anamnesis", "objects"),
  };
}

export class Engine {
  readonly store: Store;

  constructor(opts: EngineOptions = {}) {
    this.store = new Store({ ...envConfig(), ...opts });
  }

  async init(): Promise<void> {
    await this.store.init();
  }

  async remember(input: RememberInput): Promise<PutResult> {
    const rec = RememberInput.parse(input);
    const {
      payload,
      payload_media_type: payloadMediaType,
      source_revision: sourceRevision,
      previous,
      ...fields
    } = rec;
    const element: MemoryElement = { ...fields, id: uuidv7() };
    return this.store.putParsedElement(element, {
      ...(payload ? { payload } : {}),
      ...(payloadMediaType ? { payloadMediaType } : {}),
      sourceRevision: sourceRevision ?? element.origin.record,
      ...(previous ? { previous } : {}),
      enqueue: true,
    });
  }

  /** Failed handlers leave their whole fetched batch pending for retry. */
  async digest(
    handler: (episode: MemoryElement, store: Store) => Promise<void> | void,
    batchSize = 200,
  ): Promise<number> {
    const batch = await this.store.pending(batchSize);
    if (batch.length === 0) return 0;
    for (const id of batch) {
      const episode = await this.store.getElement(id);
      if (episode) await handler(episode, this.store);
    }
    await this.store.markProcessed(batch);
    return batch.length + (await this.digest(handler, batchSize));
  }

  async recall(
    query: string,
    opts: { limit?: number; at?: string } = {},
  ): Promise<SearchHit[]> {
    return this.store.searchText(query, {
      limit: opts.limit ?? 10,
      until: opts.at ?? new Date().toISOString(),
      validOnly: true,
    });
  }

  async put(element: MemoryElementInput): Promise<PutResult> {
    return this.store.putElement(element);
  }

  async link(link: MemoryLinkInput): Promise<MemoryLink> {
    return this.store.putLink(link);
  }

  /** Re-extraction rewinds only the mutable cursor, never memory data. */
  async requeueEpisodes(
    schema = "anamnesis.original-message/1",
  ): Promise<number> {
    return this.store.requeue(schema);
  }

  async verify(): Promise<IntegrityIssue[]> {
    return this.store.verify();
  }

  async status(): Promise<{
    elements: number;
    links: number;
    pendingOutbox: number;
  }> {
    const c = await this.store.counts();
    return { elements: c.elements, links: c.links, pendingOutbox: c.pending };
  }

  async close(): Promise<void> {
    await this.store.close();
  }
}
