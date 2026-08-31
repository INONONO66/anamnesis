# 01 — Data Model

계약의 원천은 `@anamnesis/protocol`의 zod 스키마이며(런타임 검증 + 타입
추론), JSON Schema로 export되어 언어 중립 계약을 이룬다. 아래는 그 계약의
규범적 서술이다.

## MemoryElement

기억 공간의 유일한 원소 계약. 원본·추출 주장·통합 사실 모두 이 형태다.
저장 시에는 공통 라벨 `:Element`에 더해 **천체 라벨**(docs/09)이 이중으로
물질화된다 — 엣지를 관계 실타입으로 물질화한 것과 같은 원리.

```text
(:Element:Episode)    먼지 — 불변 원본 (메시지·문서)
(:Element:Entity)     돌덩이 — 무시간 앵커 (사람·사물·개념)
(:Element:Fact)       행성 — 파생 서술. 사건 시각 = 참이 된 때
(:Element:Community)  은하 — dreaming이 만드는 주제 덩어리 요약 (v0.3)
```

전역 질의(전문검색·시간축 절단·감사)는 `:Element`로 한 방, 천체별
질의·GDS projection은 세부 라벨로 탄다.

```jsonc
{
  "id": "0192f3a1-…",                    // UUIDv7 (시간 정렬 가능)
  "schema": "anamnesis.original-message/1", // 종류와 버전. 유일한 "타입" 개념
  "time": {
    "value": "2026-08-21T14:03:22+09:00", // ISO 8601
    "precision": "second"                 // second | minute | day | month | year
  },
  "content": "이노가 다크 모드를 선호한다고 말했다.", // 정규화된 자연어
  "origin": {
    "source": "slack",                   // 어댑터 식별자
    "session": "C0123/2026-08-21",       // 대화·문서 단위
    "actor": "U098765",                  // 플랫폼 원본 ID. 해석하지 않는다
    "record": "1724221402.000300"        // 소스 내 레코드 ID
  },
  "mass": 0.7,                           // 고유 질량 [0,1]. 생성 시 부여, 불변
  "properties": {}                        // schema별 부가 정보. 최소 유지
}
```

### schema 레지스트리 (천체 라벨 매핑)

| schema | 라벨 | 내용 |
|---|---|---|
| `anamnesis.original-message/1` | Episode | 원본 대화 메시지 |
| `anamnesis.original-document/1` | Episode | 원본 문서·파일 (리비전당 한 에피소드) |
| `anamnesis.entity/1` | Entity | 사람·사물·개념 앵커 |
| `anamnesis.claim/1` | Fact | 추출된 주장. **무효화 사건도 그냥 claim이다** — 특별함은 INVALIDATES 엣지가 나간다는 것뿐 (invalidation schema 폐기) |
| `anamnesis.mapping/1` | Fact | actor ↔ 인물 매핑 주장 |
| `anamnesis.synthesis/1` | Fact | 여러 근거를 합친 상위 사실 (DERIVED_FROM 다수) |
| `anamnesis.community/1` | Community | 주제 덩어리 요약. HAS_MEMBER로 구성원 소유 |

이전 초안의 `invalidation/1`은 폐기 — 노드 종류가 아니라 엣지가 역할을
말한다. `synthesis`(사실 수준 통합 = Fact)와 `community`(주제 수준 요약 =
Community)는 서로 다른 것으로 분리한다.

### claim의 sub_kind (태그, 7종 — omni MemoryKind 차용)

타입 증식 규칙: **감쇠·무효화 거동이 다르지 않으면 타입이 아니라
태그다** (omni knowledge-model). 아래 7종은 거동이 실제로 다르므로
`properties.sub_kind`로 부여하고, 역학 입력(S_base 보정, docs/10 카드 2)
으로 쓴다. 새 schema를 만들지 않는다:

| sub_kind | 무효화 거동 | 안정도 경향 |
|---|---|---|
| `fact` | 반증 시 INVALIDATES | 기본 |
| `state` | 후속 상태가 자연 대체 (INVALIDATES 빈번) | 짧음 |
| `event` | 일어난 일은 번복 불가 — 무효화 거의 없음 | 김 (침강만) |
| `preference` | 취향 변화 시 INVALIDATES | 김 |
| `procedure` | 절차 개선 시 INVALIDATES | 김 |
| `decision` | 번복 시 INVALIDATES (이유 보존 중요) | 김 |
| `summary` | 재생성으로 대체 (synthesis 계열) | 재계산 가능 |

### origin.source → 확신 프라이어 (omni SourceKind 차용)

출처 종류는 "얼마나 믿나"의 초기 프라이어만 정한다 — 진위(INVALIDATES)
·침강(m)과 섞지 않는다. 어댑터가 `properties.confidence` 초기값을 출처
종류로 부여한다: 사용자 직접 발화 > 문서 추출 > 에이전트 관찰 >
시스템 이벤트 > 추론(inferred) 순. 정확한 수치는 캘리브레이션 대상.

### 시간 부여 규칙

| 계층 | time.value |
|---|---|
| 원본 | 소스가 기록한 실제 발생 시각 |
| 추출 주장 | 내용이 가리키는 시각 — `explicit`(문장 안 명시) > `relative`(발화 시각 기준 환산, 예: "2주 전") > `inherited`(원본 시각 상속) |
| 통합·매핑·무효화 | 그 사건이 성립한 시각 |

상대 시간 환산은 원본 메시지의 시각을 기준점으로 한다 (Graphiti와 동일).
운영 타임스탬프(ingested_at 등)는 모델 밖이며, 필요하면 저장 계층의 로컬
컬럼으로만 존재하고 계약에는 나타나지 않는다.

## 유입 의미론 — 멱등, 수정, 분기

### 2단 멱등 (분기 감지)

소스가 수정을 노출하지 않는다(카톡 export: 같은 id·같은 보낸 시각에 내용만
다름). 따라서 분기 감지는 엔진 몫이다:

```text
같은 origin 키 재유입 시:
├─ 내용 해시 동일 → 진짜 재유입(백업 재임포트). no-op
└─ 내용 해시 다름 → 분기 감지. 버리지 않고:
    • 새 Episode 생성 (record를 엔진이 파생: "msg-123#h<내용해시8>")
    • 사건 시각 = 원래 보낸 시각 (사슬 위치 유지)
    • (새것)-[:INVALIDATES]->(원본) 자동 배선
    • 분기 관측 시각은 properties에만 기록 (사건 시각으로 승격 금지 —
      수정 시각은 복원 불가능하므로 추정치로 시간축을 오염하지 않는다)
```

수정을 명시적으로 알려주는 소스(웹훅 등)는 리비전을 record에 직접 박아도
된다 — 선택 계약. 비교: graphiti는 같은 uuid 재저장 시 `SET n = {…}` 제자리
덮어쓰기 + `remove_episode` 연쇄 물리 삭제 — 이력 소멸. 우리는 보존.

### 시계열 사슬은 나무다 (NEXT_EPISODE)

- 배선 우선순위: ① remember 입력의 `previous`(부모 record 명시 — 에이전트
  세션 로그처럼 부모를 아는 소스) ② 폴백: 같은 (source, session, schema)에서
  사건 시각이 직전인 에피소드 (카톡 export처럼 부모를 모르는 소스).
- 한 노드에서 가지가 여럿 뻗을 수 있다 — 되감기/되돌리기 = 삭제가 아니라
  분기. 버려진 가지도 실제로 일어난 일이므로 보존되고, 히트가 없어 질량이
  식으며 저절로 가라앉는다 (풍화가 gc 역할).

### 가지 서열 (branch index) — 읽기 시점 유도, 저장 금지

```text
분기점에서 나가는 가지들의 서열:
  각 가지의 "끝단 시각" = 후손 중 가장 최신 말단의 사건 시각
  index 0 = 끝단이 가장 최신인 가지 (본선 — 살아서 자라는 줄기)
  index 1, 2, 3… = 나머지를 끝단 시각 순으로
```

새 메시지 하나에 서열이 바뀌는 값이므로 엣지에 저장하면 CREATE-only가
깨진다 — m(T)·R과 같은 계보로 **읽기 시점 계산** (docs/10 D2). 소비:
맥락 조립은 기본 index 0만(본선 = 정사), PPR은 index로 가지 진입 가중을
감쇠. 느려지면 Outbox급 가변 캐시로 강등 가능(재계산 가능하므로 무손실).

## MemoryLink

관계는 노드가 아니라 별도 링크다. 원본 배열(basis 등)을 내장하지 않는다.

**멱등성**: 링크의 멱등 키는 `(from, to, role, content 해시)` — 원소의
origin unique와 같은 급의 unique 제약으로 잡는다. 추출 파이프라인이
재실행돼도 같은 링크가 중복 생성되지 않는다 (no-op).

```jsonc
{
  "id": "0192f3b2-…",
  "from": "<element-id>",
  "to": "<element-id>",
  "role": "DERIVED_FROM",   // 아래 7종 — Neo4j 관계 실타입으로 물질화
  "content": "이 주장은 해당 메시지에서 추출되었다.", // 자연어 설명
  "weight": 1.0
}
```

### role 7종 (graphiti 어휘 차용, 이 이상 늘리지 않는다 — docs/09 §4)

| role | 방향 | 의미 |
|---|---|---|
| `NEXT_EPISODE` | 에피소드 → 직후 에피소드 | 같은 세션 시계열 사슬. remember() 자동 배선 |
| `MENTIONS` | 기억 → 엔티티 | 매핑·주장이 무엇에 관한 것인지 |
| `RELATES_TO` | 양방향 취급 | 자유 자연어 관계. content가 관계를 서술 |
| `HAS_MEMBER` | 커뮤니티 → 구성원 | dreaming이 생성하는 은하 소속 (v0.3) |
| `DERIVED_FROM` | 파생물 → 근거 | 추출 주장 → 원본, 통합 사실 → 주장들 |
| `INVALIDATES` | 무효화 사건 → 대상 사실 | 대상이 이 사건의 시각부터 유효하지 않음 |
| `CONTRASTS` | 양방향 취급 | 미해소 모순의 보존. 증거 축적 후 INVALIDATES 승격 |

### 격자 — 허용되는 (출발 라벨, 엣지, 도착 라벨) 쌍

graphiti처럼 엣지마다 출발/도착 천체를 고정한다. 이 격자 밖은 계약 위반.

```text
Episode --NEXT_EPISODE--> Episode
Episode|Fact --MENTIONS--> Entity
Fact|Entity --RELATES_TO--> Fact|Entity
Community --HAS_MEMBER--> Entity|Fact
Fact --DERIVED_FROM--> Episode|Fact
Fact|Episode --INVALIDATES--> Fact|Episode   ← Episode 포함: 수정·분기 감지용
Fact --CONTRASTS--> Fact
```

## 무효화 모델

Zep의 bi-temporal(4-타임스탬프)과 같은 표현력을, "객체당 시간 하나" 원칙을
지키며 얻는 방법:

```text
사실 A: "이노는 커피를 끊었다"            time = 2026-03-01
사건 X: "이노가 커피를 다시 마시기 시작했다" time = 2026-08-15
링크:   X --INVALIDATES--> A
```

- A는 한 바이트도 바뀌지 않는다.
- `valid_at(A, T)` = A.time <= T **이고** T보다 이른 INVALIDATES 사건이 없음.
- "언제까지 그랬는가"는 INVALIDATES 사건의 시각이 답한다.
- 시한부 기억("내일 시험 있어")은 추출 시점에 미래 시각을 가진 무효화
  사건을 함께 생성하는 것으로 표현한다 (Supermemory의 forgetAfter 상당).

### 고유 질량 (mass)

모든 원소는 생성 시 한 번 부여되는 불변의 고유 질량 [0, 1]을 갖는다 —
"회의실 예약"과 "결혼 발표"는 태어날 때부터 무게가 다르다. 추출 시 LLM이
평가하며(원본·기본값 0.5), 이후 변하지 않는다.

변하는 부분(감쇠·강화)은 저장하지 않는다. 회상 시점의 유효 질량은
`m(T) = m₀ × R(t, S)` 멱법칙으로 읽기 시점에 평가된다 (정본: docs/10
카드 1·2, 요약: docs/09 §5). 이 분해 덕에 질량이 있어도 원소 불변성이
유지되고 tick 데몬이 필요 없다.

## 파생 저장물 (계약 밖, 재생성 가능)

| 이름 | 내용 | 재생성 |
|---|---|---|
| embedding 프로퍼티 | 임베딩 벡터 (모델별) | 모델 교체 시 점진 재투영(백필) |
| (s, t_last_hit, hit_count) | 히트 원장 캐시 (docs/10 카드 2) | 원장에서 재생 |
| 항성 PPV 캐시 | 정체성 시드 PPR 사전 계산 (docs/11 §3) | dreaming이 재계산 |
| vector/fulltext/range 인덱스 | Neo4j 내장 인덱스 | DDL 재실행 |

히트 원장(:Hit)은 여기 속하지 **않는다** — 원소·링크와 같은 급의 보존
대상이다 (유실되면 m(T)가 비가역 손실, docs/09 §5).

이들은 MemoryElement/Link가 아니며, 삭제가 정보 손실이 아니다.
