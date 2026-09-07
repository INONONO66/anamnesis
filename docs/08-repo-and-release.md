# 08 — Repo Layout, Build & Release

## Language: TypeScript only

The initial plan was a Rust core with a TS shell; it is now **TypeScript
only**.

- Storage, indexes and pattern matching are Neo4j's job. The numeric work we
  write ourselves (local PPR, forgetting, RRF) is size-bounded (≤ 2,000 nodes,
  docs/06) and runs in a few ms on Float64Array. Everything else
  (extraction, daemon) is orchestration and I/O-bound.
- Where the code keeps growing (extraction orchestration, daemon I/O, the
  external harness ecosystem this daemon serves) the TS ecosystem is far
  ahead.
- Two toolchains, contract codegen, a 5-platform sidecar build matrix and IPC
  drift management all disappear.
- If a real bottleneck ever shows up, only the `dynamics` package is swapped
  for a native implementation behind its interface. That seam is the only one
  we protect.

## Monorepo (bun workspace)

Target layout. Today only `protocol`, `core` and `backfill` (source
adapters) exist; the rest is created as the roadmap reaches it (docs/09).

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
│   ├── dynamics/               # pure functions: R(t,S), S update, replay, utility U, CSR PPR, RRF, budget packing, ordering conventions (docs/04–06). No Neo4j dependency
│   ├── recall/                 # candidates, seeds, envelope queries, policy filter, conflict bundles, assembly, receipts, degradation ladder (docs/05)
│   ├── daemon/                 # anamnesisd: UDS JSON-RPC, write queue, Outbox worker, dreaming schedule
│   ├── client/                 # socket client + daemon spawn/discovery
│   └── cli/                    # bin "anamnesis" — daemon ops only (up/down/status/verify/gen/gc/dream/bench/backup/restore)
└── .github/workflows/{ci,nightly,release}.yml
```

Harnesses — whatever injects or retrieves text — live in separate repos owned
by the operator and attach over the UDS RPC contract. `remember`/`recall`,
`commit`, `policy.set`, `policy.revoke`, `adjudication.review`,
`adjudication.correct`, `embedding.retry`, `embedding.skip` and
`embedding.cancel` are API-only; the CLI never wraps them (D39, D43, D50,
D51). `gen` gains one control action, `gen(action=qualify)`, which appends an
`EmbeddingQualification` record and never activates a profile by itself (D51). A harness supplies `origin_role` and, for an assistant turn that
received anamnesis context, `lineage_mode: "receipts"` with its parent recall
IDs; the daemon verifies that against the authenticated caller and never
infers lineage from text (D49). `gc` has `--objects`, `--derived` and
`--embedding` modes and nothing else: there is no `gc --erase`, because policy
suppresses and never erases (D43). `gc --embedding` takes an
`embedding_profile_id` and refuses active, building, blocked and rollback
profiles.

`dynamics` having no Neo4j dependency is what makes the CI gates (docs/07 §6)
work — forgetting, PPR and ordering fixtures run without a container.

## Contract: zod is the source of truth

The zod schemas in `@anamnesis/protocol` are the only definition — Element,
Link, Hit and every RPC method, plus the server-owned `episode_digest_version`
discriminator and this exact control-record inventory:

```text
records: EchoLineage;
         AdjudicationAttempt, AdjudicationProposal, AdjudicationReview,
         AdjudicationConsumption, AdjudicationCorrection,
         AdjudicationCorrectionMap;
         TranslationMapping;
         EmbeddingCoverage, EmbeddingWork, EmbeddingAttempt,
         EmbeddingResolution, EmbeddingQualification, EmbeddingBuild,
         EmbeddingBuildSource
RPCs:    adjudication.review, adjudication.correct, embedding.retry,
         embedding.skip, embedding.cancel, gen(action=qualify)
```

The target-layout warning above still holds: this is the contract the schemas
must express when those pipelines are built, not code that ships today. The
`episode_digest_version` discriminator is the sharpest case: this PR defines
the two digest bodies and the stored-version-wins dispatch rule, and ships no
compatibility implementation and no data migration. Nothing in this release
reserializes, relabels or rewrites an Episode stored under version 1.

The schemas give runtime validation (at system boundaries) and TS type
inference at once, and `z.toJSONSchema()` exports JSON Schema to keep a
language-neutral contract (the committed `schemas/` are artifacts; CI is the
drift gate).

## Toolchain and runtime

- **Development**: bun (workspace, tests, scripts). Version pinned with mise.
- **Deployment target**: Node LTS — the baseline for external harness hosts
  and general compatibility. The CLI is a single `bun build --target=node` bundle.
- **No native dependencies**: the store is a Neo4j server and neo4j-driver is
  pure JS. PPR is Float64Array. No prebuilt build matrix.
- **Floating point**: PPR in `dynamics` uses only `+ × ÷` and is
  bit-reproducible. Tests of mass and RRF, which use `Math.exp/pow`, carry a
  1e-12 tolerance (docs/06 §7).
- **Tokenizers are optional, pinned dependencies.** A `tokens` budget is
  accepted only when the named `tokenizer_id` is installed and pinned by
  version or digest; an unknown id is rejected. There is no bytes/4 fallback
  and no estimate of any kind (D44). `utf8_bytes` and `unicode_scalars`
  need nothing installed.
- **GDS is pinned.** Validation and bench containers use GDS 2.13.12. No
  claim in docs/07 is made about GDS master or "latest".

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
  external prerequisite (the Neo4j container) — `up` creates `~/.anamnesis/`
  (0700), generates the per-install Neo4j password, writes `compose.yaml`
  binding bolt to 127.0.0.1 only, and manages the container's lifetime
  (docs/02 §10). GDS exists only in disposable networkless jobs: dreaming
  loads bounded ID/arc exports, while `anamnesis bench` loads fixtures or
  snapshots (docs/02 §7, docs/07 §1).
- The daemon is a JS entry in the same package (`anamnesisd.js`) — clients
  spawn it on demand and it holds `daemon.lock`. Development override:
  `ANAMNESIS_DAEMON_PATH`.
- All user data lives under `~/.anamnesis/` (docs/01 §6).
  `anamnesis backup` orchestrates the required Community offline dump, a
  fixed Payload manifest and restart; `anamnesis restore` restores that
  complete unit and verifies it (docs/01 §9).

## CI (GitHub Actions)

### ci.yml — push/PR

```text
bun install → typecheck (tsc) → bun test              (linux + macos, no container)
contract: bun run schemas → git diff --exit-code
dynamics gates: forgetting fixtures · utility attribution (Σ w_e = 1) · budget packing exactness · PPR convergence/conservation/determinism · RRF invariance · ordering conventions
integration: Neo4j container (service) → core/recall tests · policy suppression fixtures · receipt exact-once fixtures
             · language/evidence: immutable content_language · en-generation language_policy_mismatch with no
               per-claim fallback · projection-derived alias rejected · Latin/Cyrillic homoglyph distinction
               · literal quote + derived span · verbatim query on both channels
             · echo-lineage and grouping: dual digest-version round trip (version-1 write-free, version-2
               role/lineage conflict) · bounded parents/roots/depth with overflow → unknown · receipt
               selection_digest == EchoLineage.context_digests · unknown-lineage Episode and its outputs
               ineligible · complete vs incomplete synthesis support unions · same_scope_l1b vs
               same_scope_group · null subject_keys with no literal fallback
             · shadow-adjudication and operator-correction: no Fact or edge in shadow · persisted
               source_head_revision_key/policy_revision/proposed_claim_digest · W1 stale rejection on each
               stored premise · correction map under policy including denied-invalidator markers
             · embedding identity/failure/coverage: three canonical fingerprints · exact index DDL and options
               · every failure transition incl. worker_lost and cancellation · current-watermark cutover
               barrier · zero-skip production default · embedding_coverages carrying both simultaneously
               blocked (episode,0) and (extraction,N) rows with distinct cursors and omission digests
               on diagnostics and receipt                                       (docs/07 §6)
publication: reject any Markdown link from shipped files whose target contains `.omo/`
gds-solver: Neo4j+GDS container → 20 synthetic solver validations               (docs/07 §2)
```

The four D48–D51 integration groups above are the scenarios the eventual code
must satisfy, written down now so their machine values cannot drift. None of
those fixtures exists in the repository today, and listing one here is not
evidence that the behavior works (docs/07 §6).

### nightly.yml

```text
solver validation (real dumps) · envelope validation overlap@20 · health report   (docs/07 §3, §5)
```

### release.yml — v* tags

```text
1. bun build (per-package dist)
2. 100k scale bench (release gate, docs/07 §4)
3. publish order: protocol → dynamics → core → recall → client → daemon → cli
   (npm provenance, OIDC)
4. GitHub Release notes
```

## Versioning

**Lockstep across all packages** (a single version during 0.x, bulk bump
script). The daemon–client protocol is locked, so independent versioning is
over-engineering — they always ship together. Calibration constants
(`config.jsonc`) carry their own version tag (docs/04 §9) — they may change
independently of the code version. Three more versions are recorded on data,
not on packages: the `m₀` prior/calibration version on each derived
generation (a prior change is a new generation, never a rewrite), the
`tokenizer_id` version or digest on each receipt that used a token budget,
and the policy revision on each receipt (D44, D45, D47). Four more live on
data as well: `fact_language_policy` with its prompt digest,
`extractor_profile_id` and validator version on each extraction generation,
`grouping_version` on each duplicate-group key, `judge_profile_id` with the
frame prompt digest on each adjudication attempt, and the
`embedding_model_id` / `vector_index_id` / `embedding_profile_id` triple on
vector properties and indexes (D48–D51). None of them is a package version:
changing any one means a new generation, a new proposal or a new build, never
an in-place rewrite.

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
- No test pins prose. Tests assert machine-consumed values: parsed fields,
  JSON examples, numeric fixtures, link targets.
