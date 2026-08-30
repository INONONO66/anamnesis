# 03 — Recall

회상은 단일 벡터 검색이 아니라 **후보 생성 → 그래프 확산 → 가중 재정렬 →
조립**의 합성 파이프라인이며, 전 과정이 Rust 코어에서 LLM 호출 없이 돈다.
(Zep의 search→rerank→construct, HippoRAG의 PPR을 종합한 구조.)

## 파이프라인

```text
질의 (+ 선택적 시점 T, 기본 T = now)
  │
  ├─ 0. 시점 절단: snapshot(T) — time <= T 원소·링크만
  │
  ├─ 1. 후보 생성 (병렬)
  │     a. LanceDB 벡터 유사도 (시점 프리필터)
  │     b. FTS5 키워드/BM25
  │     c. 그래프 시드: 최근 세션·질의 엔티티의 이웃 확장
  │
  ├─ 2. PPR 확산
  │     후보를 시드로 인메모리 links 그래프에 Personalized PageRank.
  │     다중 홉 연상을 LLM 반복 호출 없이 그래프 연산 한 번으로.
  │     node specificity(희귀 노드 가중) 반영.
  │
  ├─ 3. 가중 재정렬
  │     score = α·semantic + β·temporal + γ·graph(PPR) + δ·entity
  │             + mass(T) 보정
  │     α..δ는 질의 유형별 프로파일 (사실 조회 / 시간 추론 / 인물 중심 …)
  │
  └─ 4. 조립
        invalidates 반영해 유효 사실만 채택 (요청 시 이력 포함),
        각 사실에 provenance 원본 참조 첨부,
        컨텍스트 문자열 또는 구조화 응답으로 반환.
```

## mass(T): 저장이 아닌 평가

```text
mass(T) = base × decay(T − last_reinforced) + Σ reinforcement(events ≤ T)
```

- 시간의 결정론적 함수이므로 **읽기 시점에 T를 넣어 평가**한다.
  백그라운드 tick으로 깎아 내려쓰는 데몬 작업이 존재하지 않는다.
- 강화 이벤트: 재언급(provenance 증가), 회상 적중, dreaming의 승격.
- `scores` 테이블은 캐시일 뿐이며 언제든 재계산된다.

## snapshot(T)

`recall(query, at: T)`는 T 시점의 세계에서 답한다:

- 원소·링크 모두 `time <= T`만 사용.
- invalidates 사건도 T 이전 것만 반영 — "그때는 아직 참이었던 사실"이
  올바르게 살아난다.
- 백필로 과거 데이터를 나중에 넣으면 과거 snapshot이 풍부해진다. 의도된
  동작이다.

## 유효성 판정

```text
valid(fact, T) = fact.time <= T
              ∧ ¬∃ inv: (inv --invalidates--> fact ∧ inv.time <= T)
```

기본 회상은 유효 사실만 반환하고, `include_history` 옵션이 버전 사슬
전체(무효화된 과거 사실 + 무효화 사건)를 함께 준다. "지금 어디 살아?"와
"이사 이력 전부"가 같은 저장소에서 나온다.

## 성능 전제

- links 그래프는 데몬 시작 시 메모리 적재, 쓰기 시 증분 갱신 — 항상 warm.
- 개인 규모(원소 수백만)에서 PPR은 ms~수십 ms.
- LanceDB ANN + FTS5로 후보 생성도 수십 ms — 목표 p50 < 100ms (Mem0의
  0.148s보다 빠르게, 로컬이라 네트워크 왕복 0).

## 프로필: 실체화된 뷰

"프로필은 저장물이 아닌 뷰" 원칙은 유지하되, 자주 쓰는 뷰(정적: 이름·직업·
핵심 선호 / 동적: 최근 관심사)는 dreaming이 **재생성 가능한 캐시**로
실체화한다 (Supermemory 패턴). 대화 시작 시 검색 없이 즉시 주입 가능.
projection과 같은 취급 — 언제든 버리고 재계산한다.
