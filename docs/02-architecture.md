# 02 — Architecture

## 전체 그림

```text
                 ┌────────────────────────────────────────┐
 Claude Code ───┤                                        │
 (MCP stdio 브리지)│  anamnesisd  (Rust 단일 바이너리, 상주)  │
 커스텀 에이전트 ──┤                                        │
 (UDS/HTTP 직결)  │  단일 라이터: vault / memory / vectors  │
 CLI ───────────┤  상주 상태: links 그래프 인메모리,        │
 어댑터·워처 ────┤            PPR 인덱스, LanceDB 핸들     │
                 │  백그라운드: outbox 소화, dreaming,      │
                 │            임베딩 백필                  │
                 └────────────────────────────────────────┘
```

## 상주 엔진: anamnesisd

에이전트들이 요청을 보내는 **항상 켜져 있는 엔진**이다. 다만 사용자가 시스템
데몬을 등록·관리하게 하지 않는다:

- **spawn-on-demand**: 클라이언트가 소켓에 붙어보고 없으면 데몬을 띄운다
  (ollama 방식). 사용자는 데몬의 존재를 몰라도 된다.
- **수명 정책**: 기본 keep-alive (24시간 상주 — idle Rust 프로세스는 수십 MB).
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

## 저장소 배치

```text
~/.anamnesis/
├── sock                       UDS
├── vault/                     ── 불변 금고 ──
│   ├── vault.db               SQLite. records 장부 + outbox. INSERT만
│   └── objects/sha256/…       원본 바이트 (content-addressed)
└── memory/                    ── 전부 파생, 삭제→재구축 가능 ──
    ├── memory.db              SQLite. elements / links / scores / FTS5
    └── vectors/               LanceDB 디렉토리 (임베딩 projection)
```

### 저장소 선택 근거

- **vault = SQLite**: 평생 보존 대상은 포맷 수명이 성능보다 중요하다.
  SQLite는 2050년까지 후방 호환을 공식 보장하는 유일한 DB이고, 단일 파일이라
  복사가 곧 백업이다. `journal_mode=WAL`, 스냅샷은 `VACUUM INTO`.
- **memory.db = SQLite**: 원소·링크·질량·FTS5. 관계형 질의와 전문검색.
- **vectors = LanceDB**: 벡터 전용 임베디드 DB (Rust crate, 서버 없음).
  진짜 ANN 인덱스(IVF-PQ/HNSW), 시점 절단용 프리필터 지원. 포맷 수명이
  SQLite만 못한 약점은 무관하다 — **재생성 가능한 projection이라 금고가
  아니기 때문**. 통째로 지워도 정보 손실이 없다.
- **그래프 = 전용 DB 없음**: links 테이블을 데몬이 메모리에 올려 PPR/BFS를
  직접 돌린다. 개인 규모(기억 수백만)에서 인메모리 그래프는 수십 MB이며,
  HippoRAG도 그래프 DB 없이 인메모리 PPR로 SOTA를 냈다.

### vault.db 스키마

```sql
CREATE TABLE records (
  id             TEXT PRIMARY KEY,            -- UUIDv7
  time_value     TEXT NOT NULL,
  time_precision TEXT NOT NULL,
  content        TEXT NOT NULL,
  origin_source  TEXT NOT NULL,
  origin_session TEXT NOT NULL,
  origin_actor   TEXT NOT NULL,
  origin_record  TEXT NOT NULL,
  payload_hash   TEXT,                        -- objects/ 참조
  content_digest TEXT NOT NULL,
  UNIQUE (origin_source, origin_session, origin_record)  -- 멱등 재유입
);

CREATE TABLE outbox (                         -- 콜드패스 커서
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  record_id TEXT NOT NULL,
  processed_at TEXT
);
```

Rust API가 INSERT만 노출한다. UPDATE/DELETE 경로 자체가 없다.
무결성은 record별 SHA-256으로 검증한다.

### memory.db 스키마 (요지)

```sql
CREATE TABLE elements ( id, schema, time_value, time_precision, content,
                        origin_source, origin_session, origin_actor,
                        origin_record, properties );
CREATE TABLE links    ( id, from_id, to_id,
                        role CHECK (role IN ('provenance','about',
                                             'invalidates','semantic')),
                        content, weight );
CREATE TABLE scores   ( element_id, mass REAL, computed_at, model );
CREATE VIRTUAL TABLE fts USING fts5(content, ...);
```

## 외부 의존

LLM/임베딩 API 키 하나 (또는 로컬 ollama). 그 외 런타임 시스템 의존성 0 —
SQLite는 rusqlite `bundled`, LanceDB는 crate 정적 링크, Linux는 musl 정적
빌드.

## 멀티 디바이스 (스코프 밖, 구조만 확보)

vault가 append-only + `origin` unique이므로 두 머신의 금고는 레코드 머지로
합칠 수 있다. 동기화 제품화는 로드맵 밖이지만 설계가 막지 않는다.
