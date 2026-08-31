# 07 — Roadmap

## v0.1 — 뼈대가 도는 최소 엔진

```text
protocol   MemoryElement/Link/RPC의 zod 정의 + JSON Schema export  [✓ 원소/링크]
vault      append + objects 저장 + 멱등 유입 + 무결성 검증
memory     elements/links/scores/FTS5 스키마 + LanceDB projection
daemon     UDS JSON-RPC (remember/recall/snapshot/status/digest),
           spawn-on-demand + keep-alive, 단일 라이터
extraction outbox 콜드패스: 주장 추출 + 중복/보강/모순 판정 + INVALIDATES
recall     vec + FTS 후보 → 가중 재정렬 → INVALIDATES 반영 조립 (PPR 제외)
client     TS 소켓 클라이언트 + 데몬 발견/스폰
cli        init / ingest / recall / daemon / digest
mcp        stdio 브리지 (remember / recall 툴)
adapter    kakao export 파일 1종
```

검증: 카톡 export 실제 유입 → 회상 품질 수동 확인, 전체 재구축
(`rm -rf memory/` → digest) 왕복 테스트.

## v0.2 — 회상 고도화 + 되씹기

```text
recall     PPR 확산 + node specificity, 질의 유형별 가중 프로파일
entity     resolution 전용 파이프라인 (후보검색 + LLM 판정 + mapping 기억)
dreaming   idle 루프: synthesis 통합, 프로필 캐시 실체화, 장거리 모순 스캔
embedding  모델 교체 시 점진 재투영 (백필)
listener   HTTP localhost (opt-in) + SSE
adapter    slack, 에이전트 훅 1종 추가
```

## v0.3 — 검증과 공개

```text
bench      LongMemEval 하네스 — Zep(58~62%), Supermemory(71~77%)와 동일 표
release    npm 정식 배포 (플랫폼 5종 prebuilt), 문서 사이트
hardening  크래시 복구 시나리오, vault 무결성 감사 명령, 마이그레이션 정책
```

## 스코프 밖 (설계는 막지 않음)

- 멀티 디바이스 동기화 (vault 머지 가능 구조만 확보)
- 멀티유저/팀 공유
- activation/parameter 메모리 (MemOS류)
- 클라우드 호스팅
