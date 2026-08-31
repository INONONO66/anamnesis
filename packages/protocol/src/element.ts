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
 * - 한 번 생성된 원소는 수정되지 않는다. 틀렸으면 무효화 사건(그것도
 *   그냥 claim이다) + `INVALIDATES` 링크로 무효화한다 — 노드 종류가
 *   아니라 엣지가 역할을 말한다 (docs/01).
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
  /** 원본 대화 메시지 — 원본층 레코드의 투영 */
  "anamnesis.original-message/1",
  /** 원본 문서·파일 (리비전당 한 에피소드) */
  "anamnesis.original-document/1",
  /** 사람·사물·개념 앵커 — 무시간 돌덩이 */
  "anamnesis.entity/1",
  /** 추출된 주장 (자연어 사실). 불변. 무효화 사건도 그냥 claim —
      특별함은 INVALIDATES 엣지가 나간다는 것뿐 */
  "anamnesis.claim/1",
  /** actor ↔ 인물 매핑 주장 */
  "anamnesis.mapping/1",
  /** 여러 근거를 합친 상위 사실 (DERIVED_FROM 다수). 재계산 가능 */
  "anamnesis.synthesis/1",
  /** 주제 덩어리 요약 — HAS_MEMBER로 구성원 소유 (dreaming, v0.3) */
  "anamnesis.community/1",
] as const;
export type KnownSchema = (typeof KNOWN_SCHEMAS)[number];

/**
 * 천체 라벨 (docs/09) — 저장 시 공통 라벨 :Element에 더해 이중으로
 * 물질화된다. 전역 질의는 :Element 한 방, 천체별 질의·GDS projection은
 * 세부 라벨로 탄다 (docs/01 §schema 레지스트리).
 */
export const Celestial = z.enum(["Episode", "Entity", "Fact", "Community"]);
export type Celestial = z.infer<typeof Celestial>;

/** schema → 천체 라벨. 레지스트리 밖 schema는 라벨 없이 :Element만. */
export const SCHEMA_LABELS: Record<KnownSchema, Celestial> = {
  "anamnesis.original-message/1": "Episode",
  "anamnesis.original-document/1": "Episode",
  "anamnesis.entity/1": "Entity",
  "anamnesis.claim/1": "Fact",
  "anamnesis.mapping/1": "Fact",
  "anamnesis.synthesis/1": "Fact",
  "anamnesis.community/1": "Community",
};

/**
 * claim의 sub_kind 태그 7종 (omni MemoryKind 차용, docs/01).
 * 타입이 아니라 `properties.sub_kind` 태그 — 역학 입력(S_base 보정,
 * docs/10 카드 2)으로만 쓰이고 계약 구조는 늘리지 않는다.
 */
export const ClaimSubKind = z.enum([
  "fact",
  "state",
  "event",
  "preference",
  "procedure",
  "decision",
  "summary",
]);
export type ClaimSubKind = z.infer<typeof ClaimSubKind>;

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
     * 고유 질량 m₀ (base mass), [0, 1]. 생성 시 한 번 부여되고 불변 —
     * 이 기억이 태어날 때부터 갖는 중요도다 (추출 시 LLM이 평가).
     * 회상 시점의 유효 질량은 멱법칙으로 읽기 시점에 평가된다:
     *   m(T) = m₀ × (1 + FACTOR·t/S)^DECAY,  t = T − 마지막 히트
     * (정본: docs/10 카드 1·2). 감쇠·강화는 저장하지 않고 히트 원장에서
     * 유도한다 — tick 데몬 없음 (03-recall).
     */
    mass: z.number().min(0).max(1).default(0.5),
    /** schema별 부가 정보. 최소로 유지 — 여기에 구조를 쌓지 않는다. */
    properties: z.record(z.string(), z.unknown()).default({}),
  })
  .strict();
export type MemoryElement = z.infer<typeof MemoryElement>;
export type MemoryElementInput = z.input<typeof MemoryElement>;
