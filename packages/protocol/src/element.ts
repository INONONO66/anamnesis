/**
 * MemoryElement — 기억 공간의 유일한 노드 타입.
 *
 * 원본 기억, 추출된 주장, 매핑, 통합 사실, 무효화 사건까지 전부 이 하나의
 * 형태다. 타입 분화는 `schema` 문자열 하나로만 표현하고, 구조는 여기서
 * 더 늘리지 않는다 (자연어 최소주의).
 *
 * 불변식:
 * - 객체당 시간은 정확히 하나이며, 그것은 사건 시간이다.
 *   (원본 = 실제 발생 시각, 주장 = 내용이 가리키는 시각,
 *    파생물 = 성립 시각. createdAt/ingestedAt 같은 운영 시간은 계약 밖.)
 * - 한 번 생성된 원소는 수정되지 않는다. 틀렸으면 invalidation 사건 +
 *   `invalidates` 링크로 무효화한다.
 * - `origin.actor`는 플랫폼 원본 ID 그대로다. 인물 해석은 별도의
 *   mapping 원소로 존재하며 원본을 덮어쓰지 않는다.
 */
import { z } from "zod";

/** 시간 정밀도. value는 항상 완전한 ISO 8601이되, 해석 단위를 한정한다. */
export const TimePrecision = z.enum([
  "second",
  "minute",
  "day",
  "month",
  "year",
]);
export type TimePrecision = z.infer<typeof TimePrecision>;

/** 사건 시간. 원소당 정확히 하나. */
export const TimePoint = z
  .object({
    /** ISO 8601, 타임존 오프셋 필수. 예: "2026-08-21T14:03:22+09:00" */
    value: z.iso.datetime({ offset: true }),
    precision: TimePrecision,
  })
  .strict();
export type TimePoint = z.infer<typeof TimePoint>;

/**
 * 출처. (source, session, record) 조합이 전역 유일 — 멱등 재유입의 키.
 * 파생물(주장·통합 등)도 자기를 만든 계보의 origin을 갖는다.
 */
export const Origin = z
  .object({
    /** 어댑터 식별자. 예: "slack", "kakao-export", "agent:claude-code" */
    source: z.string().min(1),
    /** 대화·문서 단위. 예: "C0123/2026-08-21" */
    session: z.string().min(1),
    /** 플랫폼 원본 행위자 ID. 해석하지 않는다. 예: "U098765" */
    actor: z.string().min(1),
    /** 소스 내 레코드 ID. 파생물은 파생 규칙이 부여한 안정적 ID. */
    record: z.string().min(1),
  })
  .strict();
export type Origin = z.infer<typeof Origin>;

/**
 * 원소 종류 식별자: `anamnesis.<kind>/<version>`.
 * 레지스트리는 열려 있되(패턴 검증), 코어가 아는 종류는 KNOWN_SCHEMAS.
 */
export const ElementSchemaId = z
  .string()
  .regex(
    /^anamnesis\.[a-z][a-z0-9-]*\/[1-9][0-9]*$/,
    "expected `anamnesis.<kind>/<version>` (e.g. anamnesis.claim/1)",
  );
export type ElementSchemaId = z.infer<typeof ElementSchemaId>;

export const KNOWN_SCHEMAS = [
  /** 원본 대화 메시지 — 금고 레코드의 투영 */
  "anamnesis.original-message/1",
  /** 원본 문서·파일 */
  "anamnesis.original-document/1",
  /** 추출된 주장 (자연어 사실). 불변 — 틀렸으면 invalidate */
  "anamnesis.claim/1",
  /** actor ↔ 인물 매핑 기억 */
  "anamnesis.mapping/1",
  /** dreaming이 만든 통합 사실·요약. 재계산 가능 */
  "anamnesis.synthesis/1",
  /** 무효화 사건 — 자기 시간을 가진 1급 사건 */
  "anamnesis.invalidation/1",
] as const;
export type KnownSchema = (typeof KNOWN_SCHEMAS)[number];

/** 기억의 최소 단위. */
export const MemoryElement = z
  .object({
    /** UUIDv7 — 생성 순 정렬 가능 */
    id: z.uuid(),
    schema: ElementSchemaId,
    time: TimePoint,
    /** 정규화된 자연어 문장. 의미의 본체. */
    content: z.string().min(1),
    origin: Origin,
    /**
     * 고유 질량 (base mass), [0, 1]. 생성 시 한 번 부여되고 불변 —
     * 이 기억이 태어날 때부터 갖는 중요도다 (추출 시 LLM이 평가).
     * 회상 시점의 유효 질량은 이 값을 입력으로 계산된다:
     *   mass(T) = mass × decay(T − 마지막 강화) + Σ 강화(≤ T)
     * 감쇠·강화는 저장하지 않고 읽기 시점에 평가한다 (03-recall).
     */
    mass: z.number().min(0).max(1).default(0.5),
    /** schema별 부가 정보. 최소로 유지 — 여기에 구조를 쌓지 않는다. */
    properties: z.record(z.string(), z.unknown()).default({}),
  })
  .strict();
export type MemoryElement = z.infer<typeof MemoryElement>;
export type MemoryElementInput = z.input<typeof MemoryElement>;
