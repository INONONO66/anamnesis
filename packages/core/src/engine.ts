/**
 * Engine — Neo4j 단일 스토어 위의 기억 수명 관리 (docs/02).
 *
 * 흐름:
 *   remember() ── hot path. 에피소드 CREATE + outbox 등록. LLM 없음.
 *   digest()   ── cold path. outbox의 에피소드를 핸들러(추출기)에 넘긴다.
 *                 v0.2에서 LLM 추출이 이 자리에 들어온다.
 *   recall()   ── 전문검색 후보 + snapshot(T) 절단 + invalidates 필터
 *                 (v0.2에서 벡터·PPR·가중합성으로 확장)
 */
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import {
  ElementSchemaId,
  Origin,
  TimePoint,
  type MemoryElement,
  type MemoryLink,
} from "@anamnesis/protocol";
import {
  Store,
  type IntegrityIssue,
  type PutResult,
  type SearchHit,
  type StoreOptions,
} from "./store.ts";

/** remember() 입력 — 어댑터가 소스를 정규화한 형태 */
export const RememberInput = z
  .object({
    time: TimePoint,
    /** 정규화된 자연어 content */
    content: z.string().min(1),
    origin: Origin,
    /** 에피소드 종류. 기본 original-message */
    schema: ElementSchemaId.default("anamnesis.original-message/1"),
    mass: z.number().min(0).max(1).default(0.5),
    properties: z.record(z.string(), z.unknown()).default({}),
    /** 소스 원본 바이트 (선택) — Payload 노드에 불변 보존 */
    payload: z.instanceof(Uint8Array).optional(),
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
  };
}

export class Engine {
  readonly store: Store;

  constructor(opts: EngineOptions = {}) {
    this.store = new Store({ ...envConfig(), ...opts });
  }

  /** 제약·인덱스 보장. 기동 시 1회. */
  async init(): Promise<void> {
    await this.store.init();
  }

  /** hot path — 에피소드 저장 + 콜드패스 등록. LLM 없음. */
  async remember(input: RememberInput): Promise<PutResult> {
    const rec = RememberInput.parse(input);
    const { payload, ...element } = rec;
    return this.store.putElement(
      { ...element, id: uuidv7() },
      { ...(payload ? { payload } : {}), enqueue: true },
    );
  }

  /**
   * cold path — outbox의 에피소드를 핸들러에 넘긴다.
   * 핸들러(v0.2: LLM 추출기)는 store.putElement/putLink로 파생을 쓴다.
   * 핸들러가 던지면 해당 배치는 마킹되지 않아 다음 digest가 재시도한다.
   */
  async digest(
    handler?: (episode: MemoryElement, store: Store) => Promise<void> | void,
    batchSize = 200,
  ): Promise<number> {
    let total = 0;
    for (;;) {
      const batch = await this.store.pending(batchSize);
      if (batch.length === 0) break;
      for (const id of batch) {
        const episode = await this.store.getElement(id);
        if (episode && handler) await handler(episode, this.store);
        total += 1;
      }
      await this.store.markProcessed(batch);
    }
    return total;
  }

  /** 회상. at을 주면 snapshot(T)에서 답한다 — 그 이후의 기억·무효화는 없던 것. */
  async recall(
    query: string,
    opts: { limit?: number; at?: string } = {},
  ): Promise<SearchHit[]> {
    const at = opts.at ?? new Date().toISOString();
    const limit = opts.limit ?? 10;
    const hits = await this.store.searchText(query, {
      limit: limit * 2, // 무효화 필터 여유분
      until: at,
    });
    const out: SearchHit[] = [];
    for (const h of hits) {
      if (await this.store.isValidAt(h.element.id, at)) out.push(h);
      if (out.length >= limit) break;
    }
    return out;
  }

  /** 파생 쓰기 (추출기·dreaming용 통로) */
  async put(element: unknown): Promise<PutResult> {
    return this.store.putElement(element);
  }

  async link(link: unknown): Promise<MemoryLink> {
    return this.store.putLink(link);
  }

  /** 추출 파이프라인 개선 시 재추출 — 데이터는 불변, 커서만 되돌린다 */
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
