# 05 — Comparison with Existing Memory Engines

리서치 대상: Zep/Graphiti, Mem0, Supermemory, Letta(MemGPT), HippoRAG,
A-MEM, MemOS, Cognee, Memobase. 출처는 문서 말미.

## 축별 비교

| 축 | **anamnesis** | Zep/Graphiti | Mem0 | Supermemory | Letta | HippoRAG | A-MEM | Cognee | Memobase |
|---|---|---|---|---|---|---|---|---|---|
| 원본 보존 | 물리 분리 불변 금고 | Episode 노드 (같은 DB) | **버림** | Document 계층 (클라우드) | 대화 이력 | 패시지 보존 | 노트만 | relational store | blob |
| 파생 재구축 | **전체 삭제→재구축** | 불가 | 불가 | 불가 | 불가 | 인덱스만 | 불가 | 부분 | 부분 |
| 시간 모델 | 사건 시간 단일축 + INVALIDATES | bi-temporal 4-타임스탬프 | createdAt뿐 | 논문상 2중, 코드엔 부재 | 없음 | 없음 | 생성 시각 | ingest 중심 | 이벤트 타임라인 |
| 모순 처리 | invalidation 사건 (불변) | edge invalidation | **UPDATE/DELETE (이력 파괴)** | updates 링크 + isLatest | 에이전트 재량 | 없음 | 노트 수정 (이력 파괴) | 없음 | 프로필 덮어쓰기 |
| 망각 | mass(T) 읽기시점 평가 | 없음 | 삭제뿐 | forgetAfter + cron | 없음 | 없음 | 없음 | 없음 | 프로필 갱신 |
| 관계 모델 | 자연어 링크 + role 7종(graphiti 어휘) | 온톨로지 fact edge + 커뮤니티 | 옵션 triplet | 사실 위의 사실 3종 | 없음 | schemaless triplet | 의미 링크+태그 | 온톨로지 그래프 | 없음 |
| 회상 | vec+FTS+시드 → PPR → 가중합성 | vec+BM25+BFS → 5종 리랭커 | 벡터 top-k | 사실검색→원본 재주입 | tool call | PPR (원조) | 유사도 | vec+Cypher | 프로필 주입 |
| 배포 | **npm, 로컬 데몬, 서버 0** | 서버+Neo4j | 서버/SaaS | 클라우드 SaaS | 서버+Postgres | 연구 코드 | 연구 코드 | 서버+3종 DB | 서버+Postgres |
| 데이터 주권 | 전부 로컬, 복사=백업 | 무거운 자체호스팅 | SaaS 중심 | 없음 | 자체호스팅 | 로컬 | 로컬 | 자체호스팅 | 자체호스팅 |

## 엔진별 관계 요약

**Zep/Graphiti — 가장 가까운 유사체.** 3계층(Episode→Entity→Community),
사건 시간 중심, 비손실 무효화, 합성 회상까지 철학이 거의 같다. 차이:
(1) Zep은 원본이 파생 그래프와 한 DB라 재구축 불가, 우리는 물리 분리.
(2) bi-temporal 4-타임스탬프 대신 invalidation-as-event로 같은 표현력을
더 적은 개념으로. (3) Neo4j 서버 제품 + 노드별 요약 캐싱으로 토큰 폭발
(대화당 600k+ 관측) — 우리는 요약을 뷰로 계산.

**Mem0 — 반면교사이자 근거.** 자연어 사실이 그래프 triplet을 이긴다는
결과(LOCOMO)와 미니멀 저장의 효율(대화당 ~7k 토큰, p50 148ms)은 채택.
원본을 버리고 UPDATE/DELETE로 이력을 파괴하는 것이 우리 설계의 존재 이유 —
Mem0는 "언제까지 그랬는가"에 답할 수 없고 파이프라인 개선의 소급 적용이
불가능하다.

**Supermemory — 데이터 모델 수렴, 형태는 정반대.** 원본/사실 2계층, 사실
위의 사실 링크(updates/extends/derives), 최소 관계 타입, 프로필 캐시 —
우리 INVALIDATES·시한부 기억·프로필 실체화가 이쪽 검증에서 왔다.
LongMemEval에서 Zep 대폭 우세(multi-session 71.4% vs 57.9%, temporal
76.7% vs 62.4%)가 "최소 구조 + 자연어" 노선의 증거. 차이: 클라우드
블랙박스 vs 우리의 로컬 파일 (열람·수정·백업 가능), cron 마킹 망각 vs
읽기 시점 평가.

**Letta(MemGPT) — 경쟁이 아니라 소비자.** 에이전트가 tool call로 자기
기억을 관리하는 인터페이스 계층이고 저장층은 평범하다. Letta류 에이전트가
우리 MCP 위에 얹히는 관계다.

**HippoRAG — 회상 부품의 출처.** PPR 연상 회상 + node specificity를
차용. 정적 코퍼스용 연구 코드라 시간·모순·유입 파이프라인이 없다.

**A-MEM — 아이디어 채택, 방식 기각.** "새 기억이 옛 기억의 의미를 바꾼다"
(memory evolution)는 mass(T)·dreaming으로 수용하되, 기존 노트를 직접
수정하는 방식은 불변성 위반이라 기각 — 파생 추가 + 재계산으로 대체.

**MemOS — 태도만 참고.** MemCube로 plaintext/activation/parameter 통합은
스코프 밖. "기억을 lifecycle 있는 1급 자원으로"라는 관점만 공유.

**Cognee — 같은 역할 분담, 다른 물성.** relational=원본/provenance,
vector·graph=파생 인덱스라는 분담이 동일하나 물리 DB 3개로 풀어 배포가
무겁고, 온톨로지 검증 노선은 우리의 자연어 최소주의와 반대 방향.

**Memobase — 운영 패턴 차용.** buffer→flush 콜드패스 배치가 우리 outbox
소화의 원형.

## 우리만 있는 것

1. **완전 재구축 가능성** — 금고/엔진 물리 분리. 어떤 엔진도 "파생 전부
   지우고 처음부터"가 안 된다.
2. **snapshot(T) 1급 연산 + 깔끔한 백필 소급** — 원본-파생 분리가 전제라
   타 엔진은 구조적으로 어렵다.
3. **서버 없는 로컬 상주 엔진** — 연구 코드(HippoRAG, A-MEM)는 제품이
   아니고, 제품(Zep, Mem0, Supermemory)은 전부 서버/SaaS. 이 조합의
   빈칸이 우리 포지션이다.

## 감수하는 트레이드오프

- 멀티유저·팀 공유·크로스디바이스 동기화는 스코프 밖 (append-only 금고
  덕에 추후 머지로 확장 가능한 구조만 확보).
- 벤치마크 부재 — v0.3에서 LongMemEval 하네스를 직접 돌려 Zep(58~62%대),
  Supermemory(71~77%대)와 같은 표에서 비교하는 것이 마일스톤.

## 출처

- Zep/Graphiti: <https://arxiv.org/html/2501.13956v1>,
  <https://github.com/getzep/graphiti>
- Mem0: <https://arxiv.org/html/2504.19413v1>
- Supermemory: <https://zebang.li/blog/supermemory-architecture-en>,
  <https://supermemory.ai/docs/concepts/how-it-works>
- MemGPT/Letta: <https://arxiv.org/abs/2310.08560>
- HippoRAG: <https://arxiv.org/abs/2405.14831>
- A-MEM: <https://arxiv.org/abs/2502.12110>
- MemOS: <https://arxiv.org/abs/2505.22101>
- Cognee: <https://docs.cognee.ai/core-concepts/architecture>
- Memobase: <https://github.com/memodb-io/memobase>
