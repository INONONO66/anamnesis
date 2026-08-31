# 11 — 인지역학의 Neo4j 실행 설계

> docs/10(수식 스펙)이 "무엇을 계산하나"라면, 이 문서는 "Neo4j 위 어디서
> 어떻게 계산하나"다. 원칙: **상태는 그래프에, 풀이는 세 곳에 분산** —
> 감쇠는 읽기 시점 Cypher 산식, 확산은 GDS, 융합·학습은 엔진(TS).
> 데몬/tick 없음.

## 1. 상태의 물리적 배치

```text
(:Element {id, schema, content, origin_*, payload_hash,
           mass,                            // 카드 4의 m₀: 탄생 시 1회, 불변
           s, t_last_hit, hit_count,        // 카드 2 캐시 (원장에서 재생 가능)
           embedding, time_utc})

(:Hit {element_id, t_utc, kind})            // 히트 원장. CREATE-only, 보존 대상
(:Element)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER
            |DERIVED_FROM|INVALIDATES|CONTRASTS]->(:Element)   // docs/09 §4
```

CREATE-only와의 정합: **원장(Hit)이 정본**, `(s, t_last_hit, hit_count)`는
원장에서 언제든 재생되는 명시적 캐시 예외 — Outbox 커서와 같은 급.
그래프에서 유일하게 SET되는 것이 이 캐시 3필드다.

## 2. 감쇠 — 데몬이 아니라 recall 쿼리의 한 줄

```cypher
WITH e, duration.between(e.t_last_hit, $T).days AS t
WITH e, e.mass * (1 + $FACTOR * t / e.s) ^ $DECAY AS massT
```

- `$T` = recall의 snapshot 시각. **유효성 판정(time_utc ≤ T + INVALIDATES
  절단, docs/03)과 같은 T를 쓴다** — 시간여행 질의에서 질량도 그 시점 값으로 평가되는 게
  공짜로 따라온다.
- 엔티티(S=∞)는 `massT = mass` 분기. 스캔 없음, O(후보 수).

## 3. 확산 — GDS PPR + 선형성 분해

```text
프로젝션: gds.graph.project — NEXT_EPISODE/MENTIONS/RELATES_TO/
          HAS_MEMBER/DERIVED_FROM만, 타입별 가중치 = relationshipWeightProperty.
          INVALIDATES/CONTRASTS는 프로젝션에서 제외 (비전도체, 카드 3).
풀이:     gds.pageRank.stream { dampingFactor: 0.85, sourceNodes: [...],
          maxIterations: 100, tolerance: 1e-6 }    // 오차 ≤ 0.85^k 보장
```

**선형성 정리의 실전 활용 (핵심 트릭).** GDS `sourceNodes`는 시드별
가중을 지원하지 않으므로, 시드 혼합(질의 0.7 / 세션 0.2 / 항성 0.1)을
한 번에 돌리지 않고 **성분별 분리 실행 후 선형 결합**한다:

```text
p_final = 0.7·PPR(질의 시드) + 0.2·PPR(세션 시드) + 0.1·PPR(항성 시드)
```

Jeh-Widom 선형성 정리(docs/10 카드 3)에 의해 이것은 근사가 아니라
**정확**하다. 보너스 둘:

- **항성 PPV 캐시**: 항성 시드(정체성 엔티티)는 거의 안 변하므로 그
  PPV를 dreaming 때 미리 계산해 노드 프로퍼티로 캐시 → 질의당 PPR
  실행 2회로 감소.
- **설명가능성**: 성분별 기여를 로그로 남기면 "이 결과가 왜 나왔나"를
  질의/세션/정체성 기여로 분해 설명 가능.

node specificity(허브 억제)는 시드 선정 단계에서 적용 — 엔티티 시드의
차수 상한 + 1/deg 가중으로 상위 시드를 고른다.

## 4. 융합 — 엔진 사이드, 랭크 공간

```text
후보 3채널 (전부 rank만 사용):
  vector : db.index.vector.queryNodes(...)      → rank_v
  bm25   : db.index.fulltext.queryNodes(...)    → rank_b   (Lucene CJK)
  ppr    : p_final 상위                          → rank_p
엔진(TS): rel = Σ w_c/(60+rank_c)  →  score = rel × massT^γ  →  예산 절단
```

recall 경로는 **완전 읽기 전용** (main 인지역학의 commit-only 헌법 승계).

**적응적 깊이 (이중과정, docs/10 카드 5)**: vector/bm25 = Type 1(빠른
직관), PPR+judge 필터 = Type 2(느린 숙고). S1 두 채널의 상위가 강하게
일치하면(rank 상관 임계 초과) PPR·judge를 생략하는 조기 종료 경로를
열어둔다 — 지연·비용은 질의 난이도에 비례해야 한다.

## 5. 커밋 — 유일한 상태 변경 트랜잭션

호출자가 실제 채택분을 알려오면 단일 트랜잭션:

```cypher
UNWIND $hits AS h
CREATE (:Hit {element_id: h.id, t_utc: $now, kind: h.kind})
WITH h MATCH (e:Element {id: h.id})
SET e.s = e.s * (1 + $a * h.kappa * (exp($b * (1 - h.rHit)) - 1) * e.s ^ (-$c)),
    e.t_last_hit = $now, e.hit_count = e.hit_count + 1
```

- `rHit`은 recall 때 이미 계산한 값을 넘긴다 (같은 T).
- 같은 recall 내 중복 히트는 엔진에서 병합 (카드 2 실패 조건).
- "노출"이 아니라 "사용"만 커밋 — 상위 k + 실제 컨텍스트 채택분.

## 6. dreaming — GDS 배치 3종 (같은 프로젝션 재사용)

1. **Leiden** → community 원소 생성/갱신 (은하).
2. **승격** — 승격 기준은 반복 횟수가 아니라 **일반화 이득**(docs/10
   카드 6): 예측 가능한 규칙성만 행성으로. promotion 히트 커밋(κ=0.3),
   항성 PPV 캐시 재계산.
3. **재정규화(SHY)** — 중복 RELATES_TO 병합, CONTRASTS→INVALIDATES 판정 큐
   (최근 히트분 우선 — 재응고화 규칙). 고질량 사실의 무효화는 debate
   2-패스 (docs/10 카드 6).

프로젝션 재구축(그래프 카탈로그 갱신)도 이 주기에 묶는다.

## 7. 학습 루프

히트 원장 export(Cypher 한 방) → 오프라인 피팅(srs-benchmark 프로토콜:
TimeSeriesSplit + LogLoss) → DECAY, a/b/c, κ, w_c, γ 갱신. 파라미터는
설정 표면 하나로 주입 — 코드에 박지 않는다 (docs/10 캘리브레이션 총목록).

## 8. CI 게이트 (docs/10 픽스처의 실행 위치)

| 검증 | 위치 |
|---|---|
| R(S,S)=0.9, 단조 감소, spaced>massed | 엔진 유닛 (수식 모듈) |
| PPR 수렴 오차 ≤ α^k, 선형 결합 = 혼합 시드 실행 결과 | GDS 통합 테스트 (Docker Neo4j) |
| RRF 스케일 불변성, m=0 원소의 정면 지목 노출 | recall 통합 테스트 |
| 원장 → 캐시 재생 일치 | 회복 테스트 (캐시 삭제 후 재생) |
