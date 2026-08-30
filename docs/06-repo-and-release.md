# 06 — Repo Layout, Build & Release

## 모노레포 (Cargo workspace + bun workspace 동거)

```text
anamnesis/
├── Cargo.toml                  # [workspace] members = ["crates/*", "xtask"]
├── crates/
│   ├── core/                   # 라이브러리: vault, store, recall, dream, schema
│   └── daemon/                 # anamnesisd: UDS/HTTP JSON-RPC + 백그라운드 루프
├── package.json                # bun workspace root
├── packages/
│   ├── protocol/               # JSON Schema 산출물 + 생성 TS 타입
│   ├── client/                 # 소켓 클라이언트 + 데몬 spawn/발견
│   ├── cli/                    # bin "anamnesis" — 메인 배포 패키지
│   ├── mcp/                    # MCP stdio 브리지
│   └── adapters/               # kakao-export 등
├── npm/                        # 플랫폼 바이너리 패키지 (CI 생성, 커밋 안 함)
├── xtask/                      # cargo xtask: schema export, dist, version
└── .github/workflows/{ci,release}.yml
```

## 계약 코드젠: Rust가 원천

```text
crates/core (serde + schemars 타입 정의)
  → cargo xtask schema
  → packages/protocol/schemas/*.json   (JSON Schema)
  → json-schema-to-typescript
  → packages/protocol/src/generated/*.ts
```

CI가 "schema export 후 `git diff --exit-code`"로 드리프트를 게이트한다.
한 곳에서 정의, 양쪽이 소비 — 수동 동기화 없음.

## TS 툴체인

- **bun**: 워크스페이스·테스트·번들 일체형. 버전은 mise로 핀.
- CLI는 `bun build --target=node`로 단일 JS 번들 — 사용자는 node만 필요.

## 바이너리 배포: 사이드카 + optionalDependencies (esbuild/Biome 패턴)

데몬이 있으므로 napi 애드온이 불필요하다 — TS는 소켓 클라이언트만 있으면
되고, Rust 바이너리만 플랫폼별로 배포한다. N-API 호환 매트릭스가 통째로
사라진다.

```jsonc
// packages/cli/package.json
{
  "name": "@anamnesis/cli",
  "bin": { "anamnesis": "dist/cli.js" },
  "optionalDependencies": {
    "@anamnesis/daemon-darwin-arm64": "0.1.0",
    "@anamnesis/daemon-darwin-x64":   "0.1.0",
    "@anamnesis/daemon-linux-x64":    "0.1.0",   // musl 정적
    "@anamnesis/daemon-linux-arm64":  "0.1.0",
    "@anamnesis/daemon-win32-x64":    "0.1.0"
  }
}
```

- 플랫폼 패키지는 `os`/`cpu` 필드로 제한 → npm이 자기 것 하나만 설치.
- 런타임에 `require.resolve("@anamnesis/daemon-" + platform)`로 경로 해석.
- **postinstall 스크립트 0개** (감사·설치속도 유리).
- 개발 오버라이드: `ANAMNESIS_DAEMON_PATH=target/debug/anamnesisd`.
- 정적 링크: rusqlite `bundled`, LanceDB crate, Linux musl —
  런타임 시스템 의존성 0.

설치 경험: `npm i -g @anamnesis/cli` → `anamnesis init` 끝.

## CI (GitHub Actions)

### ci.yml — push/PR

```text
rust:  cargo fmt --check → clippy -D warnings → cargo test  (linux + macos)
ts:    bun install → typecheck → bun test
계약:  cargo xtask schema → git diff --exit-code
```

### release.yml — v* 태그

```text
1. matrix 빌드 5 타깃:
   macos-14 (arm64) / macos-13 (x64) / ubuntu (x64, arm64 cross musl) / windows
2. cargo xtask dist → npm/daemon-*/ 패키지 조립
3. publish 순서: 플랫폼 5개 → protocol → client → cli, mcp, adapters
   (npm provenance, OIDC — 토큰 시크릿 불필요)
4. GitHub Release에 tarball 첨부 (npm 외 설치 경로)
```

## 버저닝

**전 패키지 lockstep** (0.x 동안 단일 버전). `cargo xtask version 0.2.0`이
Cargo.toml + 전 package.json 일괄 갱신. 데몬-클라이언트 프로토콜이 잠긴
구조라 독립 버저닝(changesets)은 오버엔지니어링 — 항상 같이 릴리스된다.

## 네이밍

npm `anamnesis`는 선점됨 (무관한 v1.2.3). 계획:

1. `@anamnesis` org 확보 시 — 전부 스코프 (`@anamnesis/cli` 등),
   bin 이름은 `anamnesis` 유지. ← 1순위
2. org 불가 시 — 대체 이름 결정 (`anamnesisd`는 비어 있음 확인).

## 저장소 운영

- `main` 보호, 작업은 브랜치 (현재: `anamnesis2` 재설계 브랜치).
- 라이선스: MIT OR Apache-2.0 (Rust 생태 관행).
- 커밋 전 최소: `git diff --check`, rust fmt/clippy, bun typecheck.
