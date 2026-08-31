# 04 — Pipelines

원칙: **hot path에는 LLM이 없다.** `remember`는 원본층 CREATE 후 즉시
리턴하고(ms), 지능이 필요한 모든 작업은 데몬의 콜드패스가 수행한다.
(Zep의 "반영까지 수 시간" 문제와 Memobase의 buffer/flush 교훈 반영 —
단, 우리는 원본이 즉시 존재하므로 추출 전에도 FTS·타임라인 회상이 된다.)

## Ingest (hot path)

```text
remember(payload)
  → 정규화 (어댑터가 소스 형식 → 자연어 content + origin + time)
  → Neo4j 단일 트랜잭션 (원자적 — 부분 유입 상태 불가):
      · (:Element:Episode) CREATE  (origin unique로 멱등 — 재유입 no-op)
      · (:Payload) CREATE          (원본 바이트, content-addressed)
      · NEXT_EPISODE 자동 배선     (docs/01 유입 의미론)
      · Outbox enqueue
  → 리턴
```

트랜잭션이 깨지면 전체 롤백 — "에피소드는 있는데 payload가 없다" 같은
반쪽 상태가 존재할 수 없다. 멱등성 + 원자성으로 재시도가 항상 안전하다.

어댑터는 TS로 작성한다 (v0.1: kakao export 파일. 이후 slack, 에이전트 훅).

## Extraction (cold path, outbox 소화)

```text
outbox에서 record 배치 획득
  → 컨텍스트 조립: 해당 record + 같은 세션의 이웃 메시지 n개
  → LLM 추출: 자연어 주장(claim) 후보들
      · 시간 부여: explicit > relative(발화 시각 기준 환산) > inherited
      · 발화자·언급 대상을 엔티티 후보로 함께 추출
  → 각 주장마다:
      1. entity resolution (아래)
      2. 유사 기존 주장 top-k 조회 (벡터 + FTS)
      3. LLM 판정: 신규 / 중복(강화) / 보강 / 모순
         · 신규   → claim 원소 + DERIVED_FROM 링크 생성
         · 중복   → 기존 주장에 재언급 히트 커밋 (κ=re_mention,
                    docs/10 카드 2 — 원소 생성 없음)
         · 보강   → 신규 생성 + RELATES_TO 링크 ("~를 보강한다")
         · 모순   → 신규 claim 생성 + INVALIDATES 또는 CONTRASTS 링크
                    (무효화도 그냥 claim이다 — docs/01. 기존 주장 불변,
                    Mem0식 UPDATE/DELETE 금지. 판정 모델은 추출 이상 —
                    docs/08 교훈 B)
  → 임베딩 계산 → embedding 프로퍼티 기록 (명시적 캐시 예외 — docs/02)
  → outbox processed 마킹
```

생성되는 모든 claim의 properties에 **`extraction_version`**(프롬프트·
모델 조합 식별자)을 박는다 — 파이프라인 개선 후 재소화하면 신버전
claim이 생성되고, 구버전과의 중복·모순은 위 판정 단계가 흡수한다.
어느 추출기가 만든 주장인지 감사 가능 + 버전별 품질 비교 가능.

멱등성: 파이프라인 전체가 record 단위 재실행 안전. 파생층 재구축 =
outbox 커서 리셋 후 전체 재소화.

## Entity Resolution (전용 단계)

모든 엔진이 가장 공들이는 지점 (Zep: 임베딩+전문검색+LLM 판정+reflexion).

```text
엔티티 후보 (이름/별칭/플랫폼 ID)
  → 후보 검색: 이름 임베딩 + FTS로 기존 엔티티·매핑 기억 조회
  → LLM 동일성 판정
  → 동일 판정 시: mapping 기억 (anamnesis.mapping/1) 생성
     "U098765는 김철수다" + MENTIONS 링크
  → 원본의 origin.actor는 절대 수정하지 않는다
```

매핑이 나중에 틀린 것으로 밝혀지면 매핑 기억을 invalidate하면 된다 —
원본과 주장은 무사하다.

## Dreaming (idle / 주기 배치)

데몬이 한가할 때 수행하는 되씹기. 긴급성이 없으므로 실패·지연이 정합성을
해치지 않는다.

```text
- 통합: 주장 클러스터 → 상위 사실(synthesis) 생성 + DERIVED_FROM 링크
- 프로필 캐시 재실체화 (정적/동적)
- 미해소 모순 스캔: 신규 유입 시 놓친 장거리 모순 탐지
- 임베딩 백필: 모델 교체 시 점진 재투영
- 항성 PPV 캐시 재계산 + 히트 캐시 검증/재생 (docs/11 §6)
```

산출물은 전부 synthesis 원소이거나 캐시 — **기존 주장·원본을 수정하는
dreaming은 없다** (A-MEM의 기존 노트 수정 방식을 의도적으로 기각).
같은 효과는 파생 추가 + mass 재계산으로 얻는다.

## LLM 사용 지점 정리

| 지점 | 경로 | 모델 요구 |
|---|---|---|
| 주장 추출 | cold | 중간 |
| 중복/보강/모순 판정 | cold | 중간 |
| entity resolution | cold | 중간 |
| dreaming 통합 | idle | 상위 |
| **회상** | **hot** | **없음 — 무LLM** |

토큰 예산은 Mem0 수준(대화당 수 k)을 목표로 하고, Zep의 노드별 요약 전면
캐싱(대화당 600k+ 사례)은 하지 않는다.
