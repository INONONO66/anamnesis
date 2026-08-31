# 08 — graphiti 계열 분석과 차용 결정

> 분석 대상 (2026-08-30, 코드·ADR 정독):
> - [getzep/graphiti](https://github.com/getzep/graphiti) — 본가. Zep의 코어 엔진.
> - [Soju06/graphiti](https://github.com/Soju06/graphiti) — 실전 패치 포크.
> - [Soju06/hermes-graphiti](https://github.com/Soju06/hermes-graphiti) — Hermes 에이전트용
>   메모리 플러그인 + single-writer 데몬. ADR 100+개, 운영 사고 기록 포함.

## 1. graphiti 본가 — 논리 구조

우리 설계와의 대응이 거의 동형이다:

| graphiti | anamnesis | 비고 |
|---|---|---|
| EpisodicNode (raw) | 에피소드 원소 (original-message 등) | 둘 다 원본 비손실 |
| EntityNode (name + summary + name_embedding) | 매핑/통합 원소 | 엔티티도 자연어 요약 중심 |
| **EntityEdge.fact (자연어 문장) + fact_embedding** | RELATES_TO 링크의 content | "사실은 엣지에 자연어로" — 우리 원칙과 동일 |
| valid_at / invalid_at / expired_at (제자리 갱신) | invalidation-as-event (불변) | 우리가 우월 — §3 사고 기록 참조 |
| CommunityNode (클러스터 요약) | dreaming의 통합 계층 | |
| 계층별 하이브리드 검색 (BM25+cosine+BFS) → RRF/MMR/cross-encoder | recall 4단 파이프라인 | 레시피 구조 차용 |

**차용 1 — 링크 임베딩.** graphiti의 fact_embedding처럼 RELATES_TO 링크의
content도 임베딩해서 벡터 검색 대상에 넣는다. 회상 후보가 "원소"만이 아니라
"관계 서술"에서도 나와야 한다.

**차용 2 — 검색 레시피.** 계층(에피소드/가공/통합)별로 BM25+벡터 후보를 만들고
RRF로 합성, 필요 시 MMR(다양성)·cross-encoder(정밀) 재정렬을 옵션 레시피로.

## 2. Soju06/graphiti 포크 — 실전 패치

**차용 3 — 소음 엔티티 필터 (추출 프롬프트 규칙).**
기계 생성 토큰 — run id(`proc_4fe2...`), SHA, `attempt=1` 카운터, `/tmp` 경로,
`OK`/`DONE` 상태 토큰 — 은 엔티티로 뽑지 않는다. "일회성 실행을 식별할 뿐,
사용자 세계의 사물이 아니며, 나중에 검색되지 않는다." 에이전트 로그가 주 소스인
우리에게 필수. 의미가 있으면 fact 문장 안에는 남기되, 주체는 지속물로 명명.

**차용 4 — 재정렬 후보 상한.** 노드 후보만 무제한이라 검색당 수백 번 분류기
호출이 나가던 것을 RRF 시드 → `2×limit` 캡으로 수정한 패치. 우리 recall
재정렬 단계는 처음부터 후보 상한을 계약에 넣는다.

**차용 5 — write-path hook seam.** 몽키패치 없이 쓰기 경로(edge 판정 등)에
끼어드는 명시적 훅 계약(fail-open). 우리 digest 핸들러를 같은 정신으로 정식화:
엔진은 훅이 없으면 기본 동작, 있으면 훅이 기본 구현을 인자로 받아 감싼다.

## 3. hermes-graphiti — 운영 사고에서 얻는 교훈

**교훈 A — 임베디드 그래프 DB의 실패 (ADR-036).**
Kuzu(임베디드)가 프로덕션에서 붕괴: 두 프로세스가 같은 DB를 열면 락 충돌,
SIGKILL 후 첫 쓰기에서 SEGV, 검색당 +0.6MB 누수 → 워크어라운드(부팅 무결성
프로브, 셧다운 센티널, 자기 재시작)를 쌓다가 결국 전부 걷어내고 Neo4j 서버 +
**단일 데몬 강제**로 전환. 진짜 원인은 "여러 프로세스가 임베디드 DB를 직접
여는 것"이고 해법이 single-writer 데몬이다. **anamnesis는 처음부터
anamnesisd가 유일한 접근 경로라 이 사고가 구조적으로 차단된다.**
(당시 SQLite 선택의 보조 근거였던 crash-recovery 논거는 §4 Neo4j 전환 후
서버 프로세스 + 단일 데몬 구조가 대체한다.)

**교훈 B — 무효화 오폭 대참사 (2026-07-08 조사).**
무효화 판정(resolve_extracted_edge)을 mini 모델에 맡긴 결과 **전체 사실의
54%(96.6k/178.6k)가 거짓 무효화**. 강한 모델로 전량 재판정 → 95.3% 복원,
무효화율 2.5%로 정상화. 두 가지를 박는다:
1. **판정(중복/모순/무효화 결정)은 추출과 동급 이상의 모델을 쓴다.** mini로
   내리는 작업이 아니다.
2. graphiti는 `invalid_at`을 제자리 수정하므로 10만 엣지 수리 스크립트가
   필요했다. 우리 invalidation-as-event는 무효화 자체가 불변 원소라서 오폭
   수리가 "무효화를 무효화하는 이벤트 추가"로 끝난다. **불변 설계의 실증적
   정당화.**

**교훈 C — hot path에서 유지보수 추방 (ADR-107).** 커뮤니티 요약/멤버십 갱신을
쓰기 경로에서 빼고 임계치 기반 지연 배치로. 우리 hot/cold 분리·dreaming과 동일
결론. 통합(dreaming)은 절대 remember/digest 지연에 끼어들지 않는다.

**교훈 D — 시간 가중은 곱셈, 하드 필터 아님 (ADR-102).**
`final = (1-w)·vector + w·(decay × recency × validity × kind)`. 우리 mass(T)
읽기 시점 평가와 동일 철학 — 무효화·풍화는 후보를 지우는 게 아니라 가중으로
누른다 (명시적 snapshot 절단 제외).

**교훈 E — 실패는 조용히 버리지 않는다 (ADR-098/101).**
추출 실패 에피소드를 DLQ 파일로 보존 + 멱등 재생 스크립트, bounded ingest
queue(bulkhead)로 폭주 차단. 우리 outbox에 추가할 것: digest 핸들러 실패 시
재시도 카운터 + N회 초과 시 DLQ 마킹(원소는 불변이므로 커서 상태만).

**교훈 F — 재정렬·회상 UX (ADR-041/042/106).** center-node 근접 검색(그래프
거리 재정렬), 2단 회상(엔티티 찾기 → 주변 사실 전개), 커뮤니티를 broad-recall
구조화(Topic map + Evidence by topic)에 활용. recall v0.3+ 후보.

## 4. 결정 — 그래프 DB 전환 여부

> **[갱신 2026-08-30]** "도커 없이 npm 설치" 제약이 해제되어 아래 초기 결정을
> 뒤집는다. **Neo4j 단일 스토어로 전환한다** (그래프+벡터 HNSW+전문검색
> Lucene+GDS를 한 시스템에). Qdrant 등 별도 벡터 DB는 채택하지 않는다 —
> graphiti 본가·hermes-graphiti 모두 임베딩을 그래프 DB 안에 두며, 분리하면
> 동기화 배관만 는다. 벡터 수억 규모가 실측되면 recall seam 뒤에 재검토.
> 상세는 docs/02 저장소 절.

초기 결정(제약 유효 당시): 논리 아키텍처(3계층, 사실=자연어+임베딩, 하이브리드
검색+재정렬, single-writer 데몬, 지연 유지보수)는 graphiti에서 통째로 차용하되
물리 저장소는 Neo4j로 바꾸지 않는다 —

- hermes-graphiti의 Neo4j 스택 = Docker compose + JVM(8GB+ RAM) + autoheal
  사이드카 + systemd 타이머. "도커 없이 npm 설치로 바로 실행" 제약과 정면 충돌.
- 그들이 Neo4j로 간 결정타는 성능이 아니라 "다중 프로세스 동시 접근 + 대시보드
  라우팅"(ADR-036 트레이드오프 표) — 우리는 데몬 단일화로 이미 해결.
- 수천만 원소 규모까지: SQLite 인접 리스트 + typed-array PPR(HippoRAG 방식) +
  LanceDB ANN(IVF-PQ). 그래프 순회는 recall seam 뒤에 있으므로, 실측 한계가
  오면 FalkorDB 등으로 그 지점만 교체한다.

## 5. 로드맵 반영

- v0.2 (추출): 소음 엔티티 필터 규칙, 판정 모델 = 추출 모델 이상, DLQ 커서,
  링크 임베딩.
- v0.2 (회상): RRF 합성 + 후보 상한, 곱셈 시간 가중.
- v0.3: 커뮤니티(통합) 지연 배치, center-node 근접 재정렬, 2단 회상,
  MMR/cross-encoder 레시피.
