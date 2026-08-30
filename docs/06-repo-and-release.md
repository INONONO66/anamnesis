# 06 — Repo Layout, Build & Release

## 언어: TypeScript 단독

초기안은 Rust 코어 + TS 하이브리드였으나 **TS 단독**으로 확정했다.

- 성능 크리티컬한 계층은 이미 네이티브 라이브러리가 담당한다 — SQLite는
  better-sqlite3(FTS5 포함), 벡터는 LanceDB 공식 JS SDK(자체가 Rust 코어 +
  napi 바인딩). 우리가 직접 짜는 hot 코드는 PPR·가중 재정렬뿐이고, 개인
  규모(링크 수백만)에서 typed array 기반 PPR은 수십 ms다.
- 코드가 계속 늘어나는 곳(어댑터, LLM 오케스트레이션, MCP)은 TS 생태계가
  압도적이다.
- 두 툴체인·계약 코드젠·5-플랫폼 사이드카 빌드 매트릭스·IPC 드리프트
  관리가 통째로 사라진다.
- 향후 진짜 병목이 나오면 recall 엔진만 인터페이스 뒤에서 네이티브로
  교체한다. 그 seam만 지킨다.

## 모노레포 (bun workspace)

```text
anamnesis/
├── package.json                # bun workspace root
├── tsconfig.base.json
├── packages/
│   ├── protocol/               # zod 계약 (원천) + JSON Schema export
│   │   ├── src/{element,link}.ts
│   │   ├── schemas/*.schema.json   # export 산출물 (커밋함)
│   │   └── scripts/export-schemas.ts
│   ├── core/                   # vault, store, recall, dream
│   ├── daemon/                 # anamnesisd: UDS/HTTP JSON-RPC + 백그라운드 루프
│   ├── client/                 # 소켓 클라이언트 + 데몬 spawn/발견
│   ├── cli/                    # bin "anamnesis"
│   ├── mcp/                    # MCP stdio 브리지
│   └── adapters/               # kakao-export 등
└── .github/workflows/{ci,release}.yml
```

## 계약: zod가 원천

`@anamnesis/protocol`의 zod 스키마가 유일한 정의다. 런타임 검증(시스템
경계에서)과 TS 타입 추론을 동시에 얻고, `z.toJSONSchema()`로 JSON Schema를
export해 언어 중립 계약을 유지한다 (커밋된 `schemas/`가 산출물, CI가
드리프트 게이트).

## 툴체인·런타임

- **개발**: bun (워크스페이스·테스트·스크립트 일체). 버전은 mise로 핀.
- **배포 타깃**: Node LTS — MCP 호스트·범용 호환성 기준. CLI는
  `bun build --target=node` 단일 번들.
- **네이티브 의존**: better-sqlite3, @lancedb/lancedb — 둘 다 자체 prebuilt
  제공. 우리가 관리하는 빌드 매트릭스는 없다.

## 배포

```jsonc
// packages/cli/package.json (요지)
{
  "name": "@anamnesis/cli",
  "bin": { "anamnesis": "dist/cli.js" }
}
```

- 설치 경험: `npm i -g @anamnesis/cli` → `anamnesis init` 끝.
  Docker 없음, 시스템 데몬 등록 없음, postinstall 스크립트 없음.
- 데몬도 같은 패키지의 JS 엔트리(`anamnesisd.js`)다 — 클라이언트가
  spawn-on-demand로 띄운다. 개발 오버라이드: `ANAMNESIS_DAEMON_PATH`.

## CI (GitHub Actions)

### ci.yml — push/PR

```text
bun install → typecheck (tsc) → bun test   (linux + macos)
계약: bun run schemas → git diff --exit-code
```

### release.yml — v* 태그

```text
1. bun build (패키지별 dist)
2. publish 순서: protocol → core → client → daemon → cli, mcp, adapters
   (npm provenance, OIDC)
3. GitHub Release 노트
```

## 버저닝

**전 패키지 lockstep** (0.x 동안 단일 버전, 일괄 갱신 스크립트).
데몬-클라이언트 프로토콜이 잠긴 구조라 독립 버저닝은 오버엔지니어링 —
항상 같이 릴리스된다.

## 네이밍

npm `anamnesis`는 선점됨 (무관한 v1.2.3). 계획:

1. `@anamnesis` org 확보 시 — 전부 스코프 (`@anamnesis/cli` 등),
   bin 이름은 `anamnesis` 유지. ← 1순위
2. org 불가 시 — 대체 이름 결정 (`anamnesisd`는 비어 있음 확인).

## 저장소 운영

- `main` 보호, 작업은 브랜치 (현재: `anamnesis2` 재설계 브랜치).
- 라이선스: MIT.
- 커밋 전 최소: `git diff --check`, tsc, bun test.
