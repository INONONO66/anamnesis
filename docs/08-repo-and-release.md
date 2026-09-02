# 08 — Repo Layout, Build & Release

## Language: TypeScript only

The initial plan was a Rust core with a TS shell; it is now **TypeScript
only**.

- Storage, indexes and pattern matching are Neo4j's job. The numeric work we
  write ourselves (local PPR, forgetting, RRF) is size-bounded (≤ 2,000 nodes,
  docs/06) and runs in a few ms on Float64Array. Everything else
  (adapters, extraction, daemon) is orchestration and I/O-bound.
- Where the code keeps growing (adapters, LLM orchestration, MCP) the TS
  ecosystem is far ahead.
- Two toolchains, contract codegen, a 5-platform sidecar build matrix and IPC
  drift management all disappear.
- If a real bottleneck ever shows up, only the `dynamics` package is swapped
  for a native implementation behind its interface. That seam is the only one
  we protect.

## Monorepo (bun workspace)

```text
anamnesis/
├── package.json                # bun workspace root
├── tsconfig.base.json
├── packages/
│   ├── protocol/               # zod contract (source of truth) + JSON Schema export
│   │   ├── src/{element,link,hit,rpc}.ts
│   │   ├── schemas/*.schema.json   # exported artifacts (committed)
│   │   └── scripts/export-schemas.ts
│   ├── core/                   # Neo4j schema, write path, idempotency, generations, objects/, spool (docs/01–02)
│   ├── dynamics/               # pure functions: R(t,S), S update, replay, CSR PPR, RRF, ordering conventions (docs/04–06). No Neo4j dependency
│   ├── recall/                 # candidates, seeds, envelope queries, assembly, degradation ladder (docs/05)
│   ├── daemon/                 # anamnesisd: UDS JSON-RPC, write queue, Outbox worker, dreaming schedule
│   ├── client/                 # socket client + daemon spawn/discovery
│   ├── cli/                    # bin "anamnesis" (up/down/remember/recall/status/verify/gen/gc/dream/bench)
│   ├── mcp/                    # MCP stdio bridge (receipt mode)
│   ├── hooks/                  # host hooks such as claude-code (auto mode, exit-0 guaranteed)
│   └── adapters/               # kakao-export etc.: source → Episode conversion
└── .github/workflows/{ci,nightly,release}.yml
```

`dynamics` having no Neo4j dependency is what makes the CI gates (docs/07 §6)
work — forgetting, PPR and ordering fixtures run without a container.

## Contract: zod is the source of truth

The zod schemas in `@anamnesis/protocol` are the only definition — Element,
Link, Hit and every RPC method. They give runtime validation (at system
boundaries) and TS type inference at once, and `z.toJSONSchema()` exports JSON
Schema to keep a language-neutral contract (the committed `schemas/` are
artifacts; CI is the drift gate).

## Toolchain and runtime

- **Development**: bun (workspace, tests, scripts). Version pinned with mise.
- **Deployment target**: Node LTS — the baseline for MCP hosts and general
  compatibility. The CLI is a single `bun build --target=node` bundle.
- **No native dependencies**: the store is a Neo4j server and neo4j-driver is
  pure JS. PPR is Float64Array. No prebuilt build matrix.
- **Floating point**: PPR in `dynamics` uses only `+ × ÷` and is
  bit-reproducible. Tests of mass and RRF, which use `Math.exp/pow`, carry a
  1e-12 tolerance (docs/06 §7).

## Distribution

```jsonc
// packages/cli/package.json (essentials)
{
  "name": "@anamnesis/cli",
  "bin": { "anamnesis": "dist/cli.js" }
}
```

- Install experience: `npm i -g @anamnesis/cli` → `anamnesis up`, done. No
  system daemon registration, no postinstall scripts. Docker is the only
  external prerequisite (the Neo4j container) — `up` writes
  `~/.anamnesis/compose.yaml` and manages the container's lifetime
  (docs/02). GDS only under `--profile gds` (docs/07 §1).
- The daemon is a JS entry in the same package (`anamnesisd.js`) — clients
  spawn it on demand. Development override: `ANAMNESIS_DAEMON_PATH`.
- All user data lives under `~/.anamnesis/` (docs/01 §6). Backup =
  `neo4j-admin dump` + `objects/` + `spool/`.

## CI (GitHub Actions)

### ci.yml — push/PR

```text
bun install → typecheck (tsc) → bun test              (linux + macos, no container)
contract: bun run schemas → git diff --exit-code
dynamics gates: forgetting fixtures · PPR convergence/conservation/determinism · RRF invariance · ordering conventions
integration: Neo4j container (service) → core/recall tests
gds-solver: Neo4j+GDS container → 20 synthetic solver validations               (docs/07 §2)
```

### nightly.yml

```text
solver validation (real dumps) · envelope validation overlap@20 · health report   (docs/07 §3, §5)
```

### release.yml — v* tags

```text
1. bun build (per-package dist)
2. 100k scale bench (release gate, docs/07 §4)
3. publish order: protocol → dynamics → core → recall → client → daemon → cli, mcp, hooks, adapters
   (npm provenance, OIDC)
4. GitHub Release notes
```

## Versioning

**Lockstep across all packages** (a single version during 0.x, bulk bump
script). The daemon–client protocol is locked, so independent versioning is
over-engineering — they always ship together. Calibration constants
(`config.jsonc`) carry their own version tag (docs/04 §9) — they may change
independently of the code version.

## Naming

npm `anamnesis` is taken (an unrelated v1.2.3). Plan:

1. If the `@anamnesis` org can be obtained — everything scoped
   (`@anamnesis/cli` etc.), bin name stays `anamnesis`. ← preferred
2. Otherwise — pick an alternative name (`anamnesisd` is confirmed free).

## Repository operations

- `anamnesis2` is the main branch. `main` is retired (docs/10 D0). Work
  branches off `anamnesis2` → PR.
- License: MIT.
- Minimum before commit: `git diff --check`, tsc, bun test.
- Any write outside the SET/DELETE list in docs/01 §8 is rejected in review.
