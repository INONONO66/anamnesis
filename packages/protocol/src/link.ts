/**
 * MemoryLink — 원소 사이의 관계. 노드에 관계를 내장하지 않는다.
 *
 * role 어휘는 graphiti의 엣지 체계를 차용한다 (docs/08·09). 저장 시
 * Neo4j 관계 실타입(UPPER_SNAKE)으로 물질화되며, 이 7종에서 늘리지
 * 않는다. 의미의 다양성은 `content`(자연어)가 담당하고, role은 엔진이
 * 기계적으로 소비하는 최소 구조만 남긴다.
 *
 * 세션 시계열은 NEXT_EPISODE로 물질화한다 — 정렬 질의로 유도 가능하지만
 * 그래프 확산(PPR)이 대화 흐름을 연결성으로 밟게 하려면 엣지여야 한다.
 */
import { z } from "zod";
import type { Celestial } from "./element.ts";

export const LinkRole = z.enum([
  /** 에피소드/사실 → 엔티티. 무엇을 언급/관여하는가. PPR 시딩 연료. (graphiti MENTIONS) */
  "MENTIONS",
  /** 원소 ↔ 원소 자유 자연어 관계. content가 관계 문장. 양방향 취급. (graphiti RELATES_TO) */
  "RELATES_TO",
  /** 에피소드 → 같은 세션의 직후 에피소드. 시계열 사슬. (graphiti NEXT_EPISODE) */
  "NEXT_EPISODE",
  /** 커뮤니티(은하) → 구성원. dreaming이 생성. (graphiti HAS_MEMBER) */
  "HAS_MEMBER",
  /** 파생물 → 근거. 주장 → 원본, 통합 → 주장들. (graphiti에 없음 — 우리 출처 사슬) */
  "DERIVED_FROM",
  /** 무효화 사건 → 대상 사실. 사건 시각부터 대상이 유효하지 않음. (우리 시그니처) */
  "INVALIDATES",
  /** 미해소 모순의 양쪽 보존. content에 긴장의 사유. (우리 시그니처) */
  "CONTRASTS",
]);
export type LinkRole = z.infer<typeof LinkRole>;

/**
 * 격자 — 허용되는 (출발 라벨, 엣지, 도착 라벨) 쌍 (docs/01).
 * graphiti처럼 엣지마다 출발/도착 천체를 고정한다. 이 격자 밖은 계약
 * 위반 — 저장 계층이 거부한다. INVALIDATES에 Episode가 들어가는 것은
 * 수정·분기(divergence) 감지용이다.
 */
export const LINK_LATTICE: Record<
  LinkRole,
  { from: readonly Celestial[]; to: readonly Celestial[] }
> = {
  NEXT_EPISODE: { from: ["Episode"], to: ["Episode"] },
  MENTIONS: { from: ["Episode", "Fact"], to: ["Entity"] },
  RELATES_TO: { from: ["Fact", "Entity"], to: ["Fact", "Entity"] },
  HAS_MEMBER: { from: ["Community"], to: ["Entity", "Fact"] },
  DERIVED_FROM: { from: ["Fact"], to: ["Episode", "Fact"] },
  INVALIDATES: { from: ["Fact", "Episode"], to: ["Fact", "Episode"] },
  CONTRASTS: { from: ["Fact"], to: ["Fact"] },
};

export const MemoryLink = z
  .object({
    /** UUIDv7 */
    id: z.uuid(),
    from: z.uuid(),
    to: z.uuid(),
    role: LinkRole,
    /** 관계의 자연어 서술 */
    content: z.string().min(1),
    /** 회상 가중치. 강화로 증가할 수 있다. */
    weight: z.number().positive().finite().default(1),
  })
  .strict()
  .refine((l) => l.from !== l.to, { message: "self-link is not allowed" });
export type MemoryLink = z.infer<typeof MemoryLink>;
export type MemoryLinkInput = z.input<typeof MemoryLink>;
