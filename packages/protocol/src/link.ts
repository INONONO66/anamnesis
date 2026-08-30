/**
 * MemoryLink — 원소 사이의 관계. 노드에 관계를 내장하지 않는다.
 *
 * role은 이 4종에서 늘리지 않는다. 의미의 다양성은 `content`(자연어)가
 * 담당하고, role은 엔진이 기계적으로 소비하는 최소 구조만 남긴다.
 *
 * `timeline`은 링크로 저장하지 않는다 — origin.session + time 정렬로
 * 질의 시점에 계산한다.
 */
import { z } from "zod";

export const LinkRole = z.enum([
  /** 파생물 → 근거. 주장 → 원본, 통합 → 주장들. */
  "provenance",
  /** 기억 → 대상. 매핑·주장이 무엇에 관한 것인지. */
  "about",
  /** 무효화 사건 → 대상 사실. 사건 시각부터 대상이 유효하지 않음. */
  "invalidates",
  /** 자유 자연어 관계. content가 관계를 서술. 양방향 취급. */
  "semantic",
]);
export type LinkRole = z.infer<typeof LinkRole>;

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
