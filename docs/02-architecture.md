# 02 — Architecture

## 전체 그림

```text
                 ┌────────────────────────────────────────┐
 Claude Code ───┤                                        │
 (MCP stdio 브리지)│  anamnesisd  (Node 프로세스, 상주)       │
 커스텀 에이전트 ──┤                                        │
 (UDS/HTTP 직결)  │  단일 라이터: Neo4j bolt 커넥션 보유     │
 CLI ───────────┤  백그라운드: outbox 소화, dreaming,      │
 어댑터·워처 ────┤            임베딩 백필                  │
                 └───────────────────┬────────────────────┘
                                     │ bolt (localhost)
                 ┌───────────────────▼────────────────────┐
                 │  Neo4j 5.26+ (Docker, localhost-only)   │
                 │  그래프 + 벡터(HNSW) + 전문검색(Lucene)  │
                 │  + GDS 플러그인 (PPR, Leiden)           │
                 └────────────────────────────────────────┘
```

## 상주 엔진: anamnesisd

에이전트들이 요청을 보내는 **항상 켜져 있는 엔진**이다. 다만 사용자가 시스템
데몬을 등록·관리하게 하지 않는다:

- **spawn-on-demand**: 클라이언트가 소켓에 붙어보고 없으면 데몬을 띄운다
  (ollama 방식). 사용자는 데몬의 존재를 몰라도 된다.
- **수명 정책**: 기본 keep-alive (24시간 상주 — idle Node 프로세스 수준의
  메모리는 개인 머신에서 무시 가능).
  설정으로 idle-timeout 가능. OS 스케줄러(launchd/systemd)는 쓰지 않는다.
- **단일 라이터**: 모든 쓰기가 데몬을 통과한다. CLI + MCP + 어댑터가 동시에
  붙어도 WAL 락 경합이 원천적으로 없다.
- **크래시 내성**: 정합성은 이벤트 기반(쓰기 시점 모순판정 + 읽기 시점 풍화
  평가)이 보장한다. 데몬은 신선도·품질 담당이므로 죽어도 기억이 깨지지
  않으며, 재시작하면 outbox부터 이어간다.

노출 명령: `anamnesis daemon status|stop|restart`.

## 리스너 3종

```text
1. UDS JSON-RPC     ~/.anamnesis/sock        기본. CLI·어댑터·로컬 에이전트
2. MCP stdio 브리지  anamnesis mcp            MCP 호스트가 스폰하는 얇은 프록시.
                                             여러 개 떠도 전부 데몬으로 수렴
3. HTTP localhost   127.0.0.1:<port> (opt-in) 소켓 못 쓰는 런타임용. SSE 스트리밍
```

핵심 RPC: `remember`(즉시 vault append 후 리턴 — ms 단위, 추출은 비동기),
`recall`(warm 인덱스로 즉답), `snapshot`, `status`, `digest`(수동 배치 트리거).

## 저장소: Neo4j 단일 스토어

> 결정 이력: SQLite+LanceDB(초안) → 단일 SQLite + 불변 트리거 → **Neo4j
> 단일 스토어** (docker-없이 제약 해제, docs/08 §4 갱신). graphiti가 검증한
> 방식 그대로 — 임베딩과 전문검색 인덱스가 그래프 DB 안에 산다. 별도 벡터
> DB(Qdrant 등)는 벡터 수억 규모가 실측될 때 recall seam 뒤에 붙인다.

```text
~/.anamnesis/
├── sock                    UDS
├── neo4j/                  Neo4j 볼륨 (data/, 백업 dump)
└── compose.yaml            CLI가 관리하는 Neo4j 컨테이너 정의

Neo4j 안:
(:Element {id, schema, time_value, time_utc, time_precision, content,
           origin_*, mass, properties, payload_hash, digest,
           embedding})                        ← 에피소드+가공 전부. 원소 하나 = 노드 하나
(:Element)-[:LINK {id, role, content, weight, embedding}]->(:Element)
           role ∈ NEXT_EPISODE | MENTIONS | RELATES_TO | HAS_MEMBER | DERIVED_FROM | INVALIDATES | CONTRASTS
(:Payload {hash, bytes})                      원본 바이트 (content-addressed)
(:Outbox …)                                   콜드패스 커서 (가변)

인덱스: origin 3-튜플 unique 제약(멱등 재유입),
       vector index (element.embedding, link.embedding — HNSW),
       fulltext index (content — Lucene, CJK 대응),
       time_utc range index (시점 절단)
```

### 선택 근거

- **그래프+벡터+전문검색이 한 시스템**: graphiti는 fact_embedding을 Neo4j
  벡터 인덱스에, 전문검색을 내장 Lucene에 둔다. 벡터 DB를 따로 두면 동기화
  배관만 늘고 얻는 게 없다 (hermes-graphiti 수개월 운영으로 검증).
- **GDS 플러그인 (무료)**: PPR/PageRank, Leiden/Louvain이 내장 —
  recall의 그래프 확산과 dreaming의 커뮤니티 검출을 손으로 짜지 않는다.
- **검증된 운영 설정 차용**: hermes-graphiti의 compose 구성, JVM 메모리
  튜닝, `neo4j-admin database dump` 백업, autoheal 패턴을 그대로 가져온다.
- **임베디드 그래프 DB는 기각**: Kuzu 붕괴 사례 (docs/08 교훈 A).

### 불변성 규율

SQLite 시절의 DB-트리거 봉인은 Neo4j Community에 없다. 불변성은 두 겹으로
지킨다:

1. **데몬이 유일한 쓰기 경로** — bolt는 localhost 전용이고 데몬만 잡는다.
2. **데몬 코드에 UPDATE/DELETE Cypher가 존재하지 않는다** — Element·LINK·
   Payload는 CREATE만. 틀린 사실은 INVALIDATES 이벤트로, 오폭 수리는 그
   무효화를 다시 무효화하는 이벤트로 (docs/08 교훈 B — graphiti의 in-place
   `invalid_at` 갱신이 10만 엣지 수리 스크립트를 부른 사고의 교훈).

무결성은 원소별 SHA-256 digest 전수 감사(verify)로 확인한다.

## 계층 순수성 — core에는 수식이 없다

`@anamnesis/core`는 **순수하게 Neo4j 구성·구축만** 담는다: 스키마(라벨·
인덱스·제약), CREATE-only 쓰기, 멱등·분기 감지, 사슬 배선, 시간축 절단,
감사. docs/10의 역학 수식(풍화 R, 간격 효과, PPR 가중, RRF 융합, 가지
서열 계산)은 core에 들어가지 않는다 — 읽기 계층(추후 `@anamnesis/dynamics`)
이 Cypher 질의 안에서 또는 그 위에서 계산한다. 경계: core가 아는 유일한
수치는 저장 필드로서의 m₀와 weight뿐이고, 이들을 소비하는 함수는 전부
밖이다. 수식 캐리브레이션이 저장 계층을 건드리지 않게 하는 격리다.

## 외부 의존

- Docker (Neo4j 컨테이너 — CLI `anamnesis daemon`이 compose 수명을 관리)
- LLM/임베딩 API 키 하나 (또는 로컬 ollama)
- Node LTS

## 멀티 디바이스 (스코프 밖, 구조만 확보)

원소가 append-only + `origin` unique이므로 두 머신의 기억은 노드 머지로
합칠 수 있다. 동기화 제품화는 로드맵 밖이지만 설계가 막지 않는다.
