/**
 * Engine — 두 DB(금고 vault.db / 기억 memory.db)를 하나의 수명으로 관리한다.
 *
 * 디렉토리 배치 (docs/02):
 *   <root>/vault/vault.db + objects/     불변 금고
 *   <root>/memory/memory.db              파생 (통째로 재구축 가능)
 *
 * 데이터 흐름:
 *   remember() ── hot path. vault append만 하고 즉시 리턴 (LLM 없음)
 *   digest()   ── cold path. outbox를 소화해 원본을 memory에 투영
 *                 (v0.2에서 LLM 추출이 이 자리에 끼어든다)
 *   recall()   ── FTS 후보 검색 (v0.2에서 벡터·PPR·가중합성으로 확장)
 *   rebuild()  ── memory 전량 소거 → outbox 리셋 → 재소화. 금고만 진실이다.
 */
import { homedir } from "node:os";
import { join } from "node:path";
import { KNOWN_SCHEMAS, type MemoryElement } from "@anamnesis/protocol";
import { Vault, type AppendResult, type VaultRecordInput } from "./vault.ts";
import { MemoryStore, type SearchHit } from "./store.ts";

export interface EngineOptions {
  /** 기본 ~/.anamnesis (ANAMNESIS_HOME으로 오버라이드) */
  root?: string;
}

export function defaultRoot(): string {
  return process.env["ANAMNESIS_HOME"] ?? join(homedir(), ".anamnesis");
}

const ORIGINAL_MESSAGE = KNOWN_SCHEMAS[0]; // anamnesis.original-message/1

export class Engine {
  readonly root: string;
  readonly vault: Vault;
  readonly store: MemoryStore;

  constructor(opts: EngineOptions = {}) {
    this.root = opts.root ?? defaultRoot();
    this.vault = new Vault(join(this.root, "vault"));
    this.store = new MemoryStore(join(this.root, "memory"));
  }

  /** hot path — 금고 append 후 즉시 리턴. ms 단위. */
  remember(input: VaultRecordInput): AppendResult {
    return this.vault.append(input);
  }

  /**
   * cold path — outbox 소화. 원본 레코드를 원소로 기계 투영한다.
   * 원소 id = 금고 레코드 id (결정론적 → 재구축해도 id 동일).
   * @returns 처리한 레코드 수
   */
  digest(batchSize = 200): number {
    let total = 0;
    for (;;) {
      const batch = this.vault.pending(batchSize);
      if (batch.length === 0) break;
      for (const entry of batch) {
        const rec = this.vault.get(entry.recordId);
        if (rec) {
          this.store.putElement({
            id: rec.id,
            schema: ORIGINAL_MESSAGE,
            time: rec.time,
            content: rec.content,
            origin: rec.origin,
            mass: 0.5,
            properties: rec.payloadHash ? { payloadHash: rec.payloadHash } : {},
          } satisfies Partial<MemoryElement> & Record<string, unknown>);
        }
        total += 1;
      }
      this.vault.markProcessed(batch.map((e) => e.seq));
    }
    return total;
  }

  /**
   * 회상 (v0.1: FTS 후보 + 시점 절단 + 유효성 필터).
   * at을 주면 snapshot(T)에서 답한다.
   */
  recall(
    query: string,
    opts: { limit?: number; at?: string } = {},
  ): SearchHit[] {
    const at = opts.at ?? new Date().toISOString();
    const hits = this.store.searchText(query, {
      limit: (opts.limit ?? 10) * 2, // 무효화 필터 여유분
      until: at,
    });
    return hits
      .filter((h) => this.store.isValidAt(h.element.id, at))
      .slice(0, opts.limit ?? 10);
  }

  /** memory는 소모품 — 전량 소거 후 금고에서 재구축 */
  rebuild(): number {
    this.store.wipe();
    this.vault.resetOutbox();
    return this.digest();
  }

  status(): {
    root: string;
    vaultRecords: number;
    pendingOutbox: number;
    elements: number;
    links: number;
  } {
    const counts = this.store.counts();
    return {
      root: this.root,
      vaultRecords: this.vault.count(),
      pendingOutbox: this.vault.pending(Number.MAX_SAFE_INTEGER).length,
      elements: counts.elements,
      links: counts.links,
    };
  }

  close(): void {
    this.vault.close();
    this.store.close();
  }
}
