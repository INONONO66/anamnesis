# 01 — Data Model

계약의 원천은 Rust(`crates/core`, serde + schemars)이며, JSON Schema로
export되어 TS 타입이 생성된다. 아래는 그 계약의 규범적 서술이다.

## MemoryElement

기억 공간의 유일한 노드 타입. 원본·추출 주장·통합 사실 모두 이 형태다.

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
  "properties": {}                        // schema별 부가 정보. 최소 유지
}
```

### schema 레지스트리 (초기)

```text
anamnesis.original-message/1   원본 대화 메시지
anamnesis.original-document/1  원본 문서·파일
anamnesis.claim/1              추출된 주장 (자연어 사실)
anamnesis.mapping/1            actor ↔ 인물 매핑 기억
anamnesis.synthesis/1          통합 사실·요약 (dreaming 산출물)
anamnesis.invalidation/1       무효화 사건
```

### 시간 부여 규칙

| 계층 | time.value |
|---|---|
| 원본 | 소스가 기록한 실제 발생 시각 |
| 추출 주장 | 내용이 가리키는 시각 — `explicit`(문장 안 명시) > `relative`(발화 시각 기준 환산, 예: "2주 전") > `inherited`(원본 시각 상속) |
| 통합·매핑·무효화 | 그 사건이 성립한 시각 |

상대 시간 환산은 원본 메시지의 시각을 기준점으로 한다 (Graphiti와 동일).
운영 타임스탬프(ingested_at 등)는 모델 밖이며, 필요하면 저장 계층의 로컬
컬럼으로만 존재하고 계약에는 나타나지 않는다.

## MemoryLink

관계는 노드가 아니라 별도 링크다. 원본 배열(basis 등)을 내장하지 않는다.

```jsonc
{
  "id": "0192f3b2-…",
  "from": "<element-id>",
  "to": "<element-id>",
  "role": "provenance",     // 아래 4종
  "content": "이 주장은 해당 메시지에서 추출되었다.", // 자연어 설명
  "weight": 1.0
}
```

### role 4종 (이 이상 늘리지 않는다)

| role | 방향 | 의미 |
|---|---|---|
| `provenance` | 파생물 → 근거 | 추출 주장 → 원본, 통합 사실 → 주장들 |
| `about` | 기억 → 대상 | 매핑·주장이 무엇에 관한 것인지 |
| `invalidates` | 무효화 사건 → 대상 사실 | 대상이 이 사건의 시각부터 유효하지 않음 |
| `semantic` | 양방향 취급 | 자유 자연어 관계. content가 관계를 서술 |

`timeline`은 링크로 저장하지 않는다 — `origin.session` + `time` 정렬로
질의 시점에 계산한다.

## 무효화 모델

Zep의 bi-temporal(4-타임스탬프)과 같은 표현력을, "객체당 시간 하나" 원칙을
지키며 얻는 방법:

```text
사실 A: "이노는 커피를 끊었다"            time = 2026-03-01
사건 X: "이노가 커피를 다시 마시기 시작했다" time = 2026-08-15
링크:   X --invalidates--> A
```

- A는 한 바이트도 바뀌지 않는다.
- `valid_at(A, T)` = A.time <= T **이고** T보다 이른 invalidates 사건이 없음.
- "언제까지 그랬는가"는 invalidates 사건의 시각이 답한다.
- 시한부 기억("내일 시험 있어")은 추출 시점에 미래 시각을 가진 무효화
  사건을 함께 생성하는 것으로 표현한다 (Supermemory의 forgetAfter 상당).

## 파생 저장물 (계약 밖, 재생성 가능)

| 이름 | 내용 | 재생성 |
|---|---|---|
| projection | 임베딩 벡터 (모델별) | 모델 교체 시 전체 재투영 |
| score | mass 계산 캐시 | 언제든 재계산 |
| fts index | 전문검색 인덱스 | DDL 재실행 |

이들은 MemoryElement/Link가 아니며, 삭제가 정보 손실이 아니다.
