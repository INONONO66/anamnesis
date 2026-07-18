# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.21.0] - 2026-07-18

### Added
- **Fallible snapshot seam** — `StorageAdapter::try_clone` (additive, `Clone`-based default so external implementors compile unchanged) and additive `Engine::snapshot_at(label, now)` stamping caller-supplied logical time (#137, #142).
- **Crash-recovery gate** — a kill-9 integration test proving flushed state survives exactly and unflushed dirty writes are never resurrected (#141).

### Fixed
- **Snapshot/restore preserves the embedding-migration checkpoint** — a mid-migration snapshot no longer drops the `embedding.migration.*` metadata, so restore keeps routing to migration resume instead of misreporting `DimensionMismatch` and risking a mixed-dimension graph (#137).
- **Snapshot paths return `Err` instead of panicking** — `snapshot()`/`restore()`/`check_invariants()` no longer clone through the panicking infallible path on SQLite failures (#142).
- **Free-id consumption is atomic with allocation** — an id is no longer assigned while still recorded free; the reopen counter collision is skipped, eliminating silent id resurrection and `INSERT OR REPLACE` overwrite of live nodes (#138).
- **`delete_node` runs in one immediate transaction** — a mid-sequence failure rolls back instead of leaving a partially deleted node (#139).
- **`append_access_trace` stages until the write-through succeeds** — a failed UPDATE no longer applies the trace in memory, so retries don't double-count `B_i` (#140).

### Documentation
- AGENTS.md shrunk to durable commands + invariants; `overview.md`/`storage.md` corrected to the live Engine/EngineConfig/SoA schema; `SqliteStorage::open` documents the no-engine-lock multi-process hazard; `dedup_enabled` documents that observations without an embedding skip dedup (#143).

### Removed
- Write-only `SearchPlan.packaging_mode` field, orphaned `MemoryRegistry::file_backed_unlocked` wrapper; `file_backed` is now `#[cfg(test)]` (#144).

## [0.20.1] - 2026-07-18

### Fixed
- **FastEmbed cache directory is configured before the Tokio runtime starts** (#112) — the env var is now set while the process is still single-threaded, removing the unsound post-spawn `set_var`.
- **Embedded `anamnesis stats` output matches the daemon path byte-for-byte** (#113).
- **npm wrapper honors a single local-binary override variable** (#114) — the two divergent override paths are unified.

### Changed
- Release workflow now fails closed when the pushed tag and manifest versions drift (#120); first-party GitHub Actions are pinned by commit SHA (#121).
- Shadow-extraction E2E harnesses serialize their nested Cargo invocations, and daemon grace-exit phases are instrumented with measured budgets — CI flake root causes, not symptom timeouts.

### Documentation
- SQLite migration policy documented through schema v11, hook top-k default corrected to `3`, and the published MCP tool inventory refreshed to all 11 tools (#115, #116, #117).

## [0.20.0] - 2026-07-16

### Added
- **R1 recall telemetry (first released in 0.20.0)** — fail-open `recall_events` telemetry retains only the newest **10,000** minimized rows and never stores raw queries. `anamnesis stats --recall` reports eligibility counts and rates, abstention categories, cosine percentiles, and a threshold sweep.
- **R2 shadow extraction** — disabled by default; opt in with `ANAMNESIS_EXTRACT_MODE=shadow` to run one configured provider over bounded captured-turn batches with bounded execution time and output size. Provider output undergoes strict validation, remains graph-nonmutating, and is reviewable with `anamnesis extract --audit`.

### Changed
- **Nextest test-group serialization** — the shadow-extraction and latency-regression tests now share a serialized test group to avoid CI resource contention.

## [0.19.0] - 2026-07-15

### Added
- **Backup-first embedding migration** — `anamnesis migrate-embeddings` provides a manual migration command, and the daemon migrates automatically; before any writes, it creates and verifies a deterministic dated backup.
- **Resumable, isolated migrations** — batched checkpoints resume interrupted work, with at most one migration job per namespace.
- **Privacy-minimized recall eligibility telemetry** — recall records bounded (newest **10,000**) side-schema rows with query character counts and gate metadata, never raw queries, transcripts, or rendered context. `anamnesis stats --recall` reports injection eligibility rather than delivery or quality; unsupported side schemas and telemetry failures fail open without blocking recall or hook prompt delivery.

### Changed
- **Migration-time recall fails open** through the existing hook path and injects no context.
- **Automatic migration control** — set `ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS=0` to opt out and run `anamnesis migrate-embeddings` manually.
- **Model-stamp safety** — the target model stamp is written only after the migration completes.

## [0.18.0] - 2026-07-08

### Added
- **Hook cosine abstention gates** — recall now carries query-embedding cosine through the readout path, and hook recall abstains when top hits fail calibrated cosine floors.
- **Transcript-aware hook queries** — `UserPromptSubmit` recall folds in a bounded tail of recent transcript turns alongside the project cue and current prompt.
- **Knowledge-only hook rendering** — hook injections now omit raw episodic/capture fragments and keep compact node references for follow-up `relate`.
- **Scope-native project boundaries** — capture and hook recall use `project/<normalized-cwd-basename>` scopes so unrelated project memories are filtered out.
- **Hook precision battery** — `scripts/hook-battery.sh` seeds an isolated DB and checks content-free, topical, cross-project, and unknown-project hook behavior.

### Changed
- **Default hook top-k is now 3** for tighter prompt injections.
- **Default MCP embedding model is now `multilingual-e5-small`** via `ANAMNESIS_EMBED_MODEL`; existing 768-d BGE DBs should be backed up/reset or opened with `ANAMNESIS_EMBED_MODEL=bge-base-en-v1.5`.
- **Scoped recall keeps universal plus same-scope nodes and drops foreign-scope nodes**, which is a precision-oriented behavior change for callers using `scope`.
- **Default user-prompt cosine gate is calibrated to `0.86`** from the 2026-07-08 e5-small battery; override with `ANAMNESIS_HOOK_COSINE_GATE` if a graph needs higher recall.

### Fixed
- **System preamble capture noise is filtered** from automatic transcript capture, including `# AGENTS.md`, system reminders, command wrappers, caveats, and interruption markers.

## [0.17.0] - 2026-07-05

### Added
- **Timeline scrubbing** in the graph dashboard — a dual-handle range slider over node `created_at` filters the galaxy to a chosen time window (nodes outside the window fade out via visibility, so the force layout stays put), with human-readable bounds and a reset.
- **Saved views / presets** — persist the current view (color mode, focus, label toggle, category/community filters, timeline window, depth) to `localStorage` under named presets; load or delete them from the sidebar.
- **Mini-map** — a corner inset projecting all loaded node positions with a live camera indicator, throttled and gated by the render-on-demand loop so it never defeats idle-pause.

### Fixed
- **`/api/graph` node-budget contract** — the `limit` node budget now defaults to 250 and is capped at 2000 (previously defaulted to 100 with no upper bound), matching the documented endpoint contract.
- **`Memory::subgraph` truncation flag** — `truncated` is now set only when the BFS frontier is genuinely cut by the node budget, instead of being inferred from the global node count (which false-positived when the reachable set was fully collected but the graph held unrelated disconnected nodes).

Dashboard features are additive UI; the `/api/graph` budget change is backward-compatible (larger default, new upper bound). Minor bump.

## [0.16.1] - 2026-07-05

### Fixed
- **Dashboard repaints on window resize while idle** — the render-on-demand loop now wakes on `resize`, so resizing the browser window while the galaxy is paused immediately re-fits the canvas and reprojects the labels (previously the last frame stayed stretched at the old size until the next pointer/wheel interaction).

## [0.16.0] - 2026-07-05

### Added
- **Cinematic atmosphere** in the 3D graph dashboard — a deep-space backdrop with layered nebula haze, an in-scene WebGL starfield, and an overview HUD (node / edge / community counts) for the premium "memory galaxy" look.
- **Dense-scale galaxy** — the dashboard bootstrap now loads a much denser neighborhood (up to 400 seeds) so the graph reads as a luminous star-cloud instead of a sparse scatter, with density-tuned bloom (strength/threshold/radius) that keeps individual nodes distinct at hundreds of nodes (no whiteout).

### Changed
- **Label level-of-detail** — "Show labels" now renders labels only for the nodes nearest the camera (revealed as you zoom into a cluster), with on-screen culling, a hard cap, and ~10 fps throttling, instead of projecting every node's label every frame. Zoomed out shows a clean graph; zoom in for a legible, bounded set.
- **Render-on-demand & adaptive cost** — the render loop pauses once the layout settles and the camera is still (resuming on interaction or data change), and node geometry resolution + link particles scale down on large graphs. At ~350 nodes this lifts "Show labels" from ~14 fps to ~120 fps and drops idle GPU work to zero, while preserving the resting look.

Dashboard-only (no engine / public-API change); the minor bump keeps the crate and plugin versions in lockstep with the graph-viz phase series (0.14 → 0.15 → 0.16).

## [0.15.0] - 2026-07-04

### Added
- **Community coloring & DOI focus+context** in the 3D graph dashboard — `/api/graph` nodes now carry `cluster` (Leiden community id) and `doi` (degree-of-interest = relevance + salience + recency − graph-distance), computed server-side; the dashboard adds a Type/Cluster color toggle (color-by-community, golden-angle hues) and a Focus mode that fades low-DOI context while enlarging/brightening the relevant core.

Additive (new JSON fields, new mcp-only dependency) → minor bump.

## [0.14.0] - 2026-07-04

### Added
- **3D graph visualization** in `anamnesis dashboard` — the dashboard is now an interactive force-directed **galaxy** of your memory graph (vendored `3d-force-graph` + three.js, offline/no-CDN): retrieval-seeded neighborhood, node color-by-type + size-by-salience with real `UnrealBloomPass` bloom, **search-to-focus**, **click-to-expand** (k-hop), a node **detail panel** (content, provenance, validity, forget), and a category **filter sidebar**. Replaces the previous read-only table view.
- **`GET /api/graph`** MCP dashboard endpoint (seed/query-seeded bounded subgraph as canonical `{schema,nodes,edges}` JSON) + **`Memory::subgraph`** engine API (bounded k-hop export: nodes + induced edges + per-node depth).

Additive public API (new engine method, new endpoint) → minor bump.

## [0.13.0] - 2026-07-04

### Added
- **Local dashboard** — `anamnesis dashboard [--port N] [--namespace ns]` serves a read-only localhost web UI to browse/manage memories and view stats. A daemon client (never opens the DB directly); binds 127.0.0.1 only.
- **Origin provenance in `get`/`list`** — the MCP `get`/`list` output (and `MemoryView`) now surface `peer_id` / `session_id` / `scope` / `confidence`, so a consumer can see which writer/session/scope produced each memory.
- Broadened MCP client docs — verified install configs for Cursor, Windsurf, OpenCode, plus a generic any-MCP-client stdio config.

### Fixed
- **`install.js` now verifies binary integrity** — the fetched native binary is checked against the release `SHA256SUMS.txt` before use (previously downloaded and executed with no integrity check); aborts on mismatch. Supply-chain hardening.

### Changed
- Strengthened the README differentiation narrative (evidence-linked).

Additive public API (new `MemoryView` fields on a `#[non_exhaustive]` type, new subcommand); minor bump.

## [0.12.0] - 2026-07-04

### Added — agent-facing memory management
- MCP tools `update`, `forget` (soft-retract, with a `hard` delete flag), `supersede`, `list`, `get`: the agent can now edit, forget/delete, mark-outdated, browse, and inspect memories. `relate` also accepts `supersedes`.
- Kernel/framework: `Relation::Supersedes`, `Engine::unretract`, and a `Memory` management API (`update_content`, `get`, `list`, `forget`, `unforget`, `delete_hard`, `supersede`) with the `MemoryView` and `ListFilter` types.
- `remember` accepts optional `tags`, `metadata`, and `scope`; `list` and `recall` support tag / metadata / scope filtering (recall filters the rendered context package, not just the relate-candidate ids).

### Fixed
- Extraction queue is now per-namespace instead of a single global queue: captured turns no longer leak across projects, and a non-default namespace's un-extracted backlog is rebuilt on first open after a daemon restart.
- README benchmark numbers reconciled to the calibration-records SSOT (LoCoMo 77.6% / MRR 0.291, LongMemEval 93.8% / MRR 0.872).

### Changed
- Internal refactor: `dispatch.rs` and `memory.rs` split into submodules (behavior-preserving).

Public API changes are purely additive (new types, methods, and a non-exhaustive enum variant); minor bump per policy.

## [0.11.0]

External-review findings (round 2) — verified against source; only genuine bugs
fixed. **Breaking:** `Edge` gains a `leaked_at` field and `StorageAdapter` gains
`leaked_at` accessors (custom storage backends and `Edge` struct-literal
constructors must update) — hence the minor (0.x-breaking) bump.

Engine — cognitive-dynamics & storage correctness:

- **tick-death on legacy DBs (#1)**: an empty `access_history` made the base level `-inf`, so the first `tick()` after an upgrade returned `Err(NonFinite)` and bricked recall. A v8→v9 migration backfills a creation trace, and a defensive tick finite-guard floors trace-less nodes instead of aborting the batch.
- **edge-leak self-erosion (#2)**: idle-edge conductance leaked per tick-call (and MCP recall ticked twice), so the reasoning graph eroded the more it was used. A per-edge `leaked_at` checkpoint (v9→v10) makes leak elapsed-time-based and idempotent; MCP recall now ticks once.
- **temporal ms/seconds (#3)**: the cue parser assumed seconds but `Timestamp` is milliseconds, so "yesterday" and explicit dates never matched. `temporal.rs` is now millisecond-native.
- **BM25 inversion (#5)**: the best FTS matches received the lowest score. Fixed to be monotone-increasing in `-rank` (raises LoCoMo MRR to ~0.46).
- **migration brick (#6)**: `schema_version` was stamped once after the whole chain, so a crash mid-chain replayed a completed hop → duplicate-column → the DB never reopened. Each hop now stamps its version inside its own transaction (crash-safe replay).
- **retract/Supersedes persistence (#7)**: both mutated the node in memory only and were lost on reopen; now the full row is persisted.
- **salience recalibration (#4)**: `SURPRISE_GAIN_K` is decoupled from `INITIAL_RETAINED_ACTION` and set to `12.0`, so ordinary captured turns reach the archive floor after ~6 months of disuse instead of saturating salience for years (validated: LoCoMo Recall@20 77.3%, MRR 0.46; cognitive-fidelity gates unaffected).

MCP daemon & plugin — hardening:

- **supply-chain (S1)**: the SessionStart hook fetched and executed a GitHub-release binary with no verification. The release workflow now emits `SHA256SUMS.txt`; the installer verifies the sha256 fail-closed before `chmod`/`mv` and drops `--clobber`.
- **version pin (S2)**: the Codex MCP config no longer uses `anamnesis-mcp@latest`; it is pinned to the daemon version.
- **`ANAMNESIS_SOCKET` (M1)**: documented (and suggested in the daemon's own error) but never read; the daemon now honors it.
- **secret redaction (G1)**: raw transcript turns were ingested verbatim into plaintext SQLite; obvious secrets (`sk-`/`gh*_`/`AKIA`/`Bearer`) are now scrubbed before ingest.
- **failure observability (O1)**: `stats` gains daemon-observed `dispatch_errors` / `ingest_errors` / `empty_recalls` counters.
- **registry-lock starvation (C1)**: the daemon served every request under one global registry `Mutex`, so a slow ingest starved other sessions' recall. Refactored to per-namespace `Arc<Mutex<Memory>>` with a deadlock-free two-phase dispatch.

## [0.10.2]

Ops hardening — the product-layer gaps from the post-0.10 assessment:

- **stats**: new dogfood usage section — daemon-lifetime op counters (recalls/reinforcing, remembers, relates, captured turns, extraction pulls), live extraction backlog, captured total, and a 14-day stale ratio.
- **docs**: product-definition SSOT in the README ("What it is not" + "Success criteria"); a new [operations contract](docs/06-operations/operations.md) (tool timing, failure/recovery semantics incl. redelivery, daemon lifecycle & version-skew workaround, all env knobs); a [migration policy](docs/03-persistence/migration-policy.md) codifying the no-data-loss guarantees and declaring the existing migration/fixture tests normative.
- **tests**: killed two flake classes — capture drop-then-reopen flock races (retry helper) and fixed-tempdir namespace tests (unique tempdirs).

## [0.10.1]

Fixes from post-0.10.0 external review (all four findings verified before fixing):

- **docs**: removed stale `fact_at` references (method deleted in 0.10.0) across README and docs; corrected `snapshot()` signature (`Result<SnapshotId, Error>`); full README API-block signature audit.
- **storage (breaking-safe)**: v7→v8 migration normalizes ALL bare non-canonical `node_type` strings to the canonical `custom:` encoding (Rust-side re-encode, escape-correct), so foreign/future bare types are visible to `nodes_by_type` — closes the class, not just the fixed legacy list.
- **query**: tension endpoints are exempt from result-limit trimming — the "why did we switch?" tension now survives small `limit`s (the demo/tests no longer need an oversized limit).
- **demo**: the flat-cosine baseline now ranks the full episodic corpus independently of graph recall.

## [0.10.0] — Shrink to product

Breaking release. An audit found ~85% of the Engine's public surface had zero
consumers; this release deletes the consumer-less surface so the map matches the
territory. See [ADR-0014](docs/adr/0014-shrink-to-product.md) for the full record,
the by-design decay coarsenings, what survives and why, and the re-add conditions.

### Breaking Changes

- **Debug/hypothesis lifecycle removed** — `start_debug`, `log_hypothesis`, `log_evidence`, `reject_hypothesis`, `confirm_hypothesis`, `end_debug`, `search_rejected_hypotheses`, the `EvidenceResult` / `DebugOutcome` types, and the `DebugSession` / `Hypothesis` / `Evidence` node types. No consumer.
- **Convenience API removed** — `learn`, `log_activity`, `schedule`, `apply_feedback`, `query_perspective`, `reflect_batch`, `support_report`, `Memory::consolidate` / `consolidate_at`, and their input types. No consumer.
- **Peer/trust subsystem removed** — `PeerRegistry`, `PeerProfile`, `TrustLevel`, `Engine::register_peer` and the other engine peer methods, and the trust reservoir. Readout's trust term is now a neutral `1.0`. `PeerId` and `SourceKind` remain on `Origin` (storage-level provenance survives).
- **`KnowledgeType` collapsed 15 → 4** — now `Episodic`, `Semantic`, `Identity`, `Custom(String)`. The removed variants' stored rows are normalized to these on open.
- **`ScopeRelation` hierarchy removed** — `ScopePath` is now an opaque canonical string plus `is_universal()`; scope scoring is a two-branch weight (all production scopes were `universal`).
- **`MemoryTier` manual override removed** — `Engine::set_tier` / `get_tier`. The `MemoryTier` enum and the salience→label display mapping remain.

### Changed — by-design decay/tau coarsenings (disclosed)

Collapsing the type taxonomy coarsened several per-type decay *policy inputs* (the dynamics of [ADR-0008](docs/adr/0008-powerlaw-dissipation.md) are unchanged):

- `Event` decay multiplier `m_type` `0.60 → 0.40` (folds into the ordinary-knowledge rate).
- `Convention` / `Decision` `m_type` `0.30 → 0.40`.
- ex-inert `Hypothesis` / `Evidence` / `DebugSession` legacy rows: `0.0` (never decayed) → `0.40` when decoded as `Custom`.
- `IdentityLearned` / `IdentityState` merged into `Identity`: now `0.0` (tick-protected, never decays).
- Entity↔Entity seed-`tau` special-case dropped (Entity pairs use the ordinary seed distribution).

### Migrations

- **v5 → v6** — drops the `peers` / `peer_aliases` tables.
- **v6 → v7** — normalizes legacy `node_type` rows to `Episodic` / `Semantic` / `Identity` / `Custom(<original>)` in place; old databases open with no data loss (the original label is preserved as `Custom`).

Both run automatically on `SqliteStorage::open`.

### Added (recent releases folded in)

- **Automatic capture pipeline** (0.9.x, [ADR-0013](docs/adr/0013-reasoning-capture-pipeline.md)): `Stop` / `PreCompact` / `SessionEnd` hooks stream turns to Anamnesis as raw `Episodic` memories (content-hash deduped, fire-and-forget); a Stage-2 nudge asks the agent to distill the un-extracted queue via the `extract_pending` MCP tool. Capture hardening (queue durability, nudge ungating, bounded I/O) in 0.9.1.
- **Reasoning-advantage demo** (PR-A): `examples/reasoning_demo.rs` + `tests/reasoning_advantage.rs` — the why-query a flat vector list cannot answer, showing contradiction-as-tension and typed why-chains over the same nodes.
- **`capture.rs`** — the MCP capture/extraction pipeline extracted into its own module (move-only).

## [0.5.0]

### Breaking Changes

- **`Origin.agent_id: String` removed** — replaced by `Origin.peer_id: PeerId` and `Origin.source_kind: SourceKind`.
  Migration: replace `agent_id: "my-agent".to_string()` with `peer_id: PeerId(0), source_kind: SourceKind::AgentObservation`.
- **`SessionSummary.agent_id: String` removed** — replaced by `SessionSummary.peer_id: PeerId`.
- **`PerspectiveKey.observer_agent_id: String` removed** — replaced by `observer_peer_id: PeerId`.
- **`ObservedRef::Agent(String)` changed** — now `ObservedRef::Agent(PeerId)`.
- **`StorageAdapter::nodes_by_agent()` removed** — replaced by `nodes_by_peer(PeerId)`.
- **`Observation` struct** — two new required fields: `valid_from: Option<Timestamp>` and `valid_until: Option<Timestamp>`. Set to `None` for existing code.
- **`SearchInput` struct** — new field `peer_filter: Option<Vec<PeerId>>`. Set to `None` for existing code.
- **`IngestResult` enum** — new variant `CreatedWithConflict { node_ids, conflict }`. Update match arms.
- **`Engine::health()`** — now returns `HealthReport` (was `GraphHealth`). Use `Engine::graph_health()` for the old behavior.
- **SQLite schema** — migrated from v1 to v2. Existing databases are auto-migrated on open.
- **Minimum Supported Rust Version raised 1.85 → 1.88** — required by the new `anamnesis-mcp` crate's `rmcp`/`schemars` dependency tree (`darling 0.23` requires rustc 1.88); the workspace now tracks a single MSRV.

### Added

#### Peer Identity System
- `PeerId(u64)` newtype — stable identifier for humans and agents.
- `TrustLevel` enum — `Owner`, `Admin`, `Member`, `Agent`, `Observer`, `Untrusted` with `scope_weight_bonus()`.
- `SourceKind` enum — `AgentObservation`, `HumanInput`, `DocumentExtract`, `SystemEvent`, `Inferred`, `External`.
- `EdgeSource` enum — `Auto`, `Manual`, `Inferred` on every `Edge`.
- `PeerProfile` struct — name, trust level, aliases, platform usernames.
- `PeerRegistry` — in-memory registry with O(1) alias resolution.
- `Engine::register_peer()`, `resolve_peer()`, `get_peer()`, `update_peer_trust()`, `add_peer_alias()`, `add_peer_platform()`, `list_peers()`, `peer_count()`.

#### Knowledge Integrity
- `Engine::retract(node_id, reason, timestamp)` — explicit node invalidation (metadata flag, edges preserved).
- `Engine::is_retracted(node_id)` — check retraction status.
- Retracted nodes are excluded from `search()`, `query()`, and `fact_at()` results.
- `IngestResult::CreatedWithConflict` — returned when similarity is in `(conflict_threshold, dedup_threshold)`.
- `ConflictHint` struct — `existing_node`, `similarity`, `suggestion` (`ProbableUpdate`, `ProbableDisagreement`, `ProbableDuplicate`).
- `EngineConfig::conflict_threshold` — default 0.75.
- Automatic `Contradicts` edge created on conflict detection.

#### Health Diagnostics
- `Engine::health()` → `HealthReport` — `total_nodes`, `orphan_count`, `contradiction_count`, `supersede_count`, `retracted_count`, `missing_embedding_count`, `peer_count`, `avg_salience`, `grade`.
- `HealthGrade` enum — `A`, `B`, `C`, `D` based on orphan/contradiction/supersede rates.
- `Engine::graph_health()` — legacy `GraphHealth` struct (unchanged).

#### Search Enhancements
- `SearchInput::peer_filter: Option<Vec<PeerId>>` — restrict results to specific peers (AND with scope filter).
- `TrustLevel` bonus applied to `scope_weight` during search ranking (`Owner` +0.10, `Untrusted` -0.05).

#### Bitemporal Ingest
- `Observation::valid_from: Option<Timestamp>` — passed through to `Node.valid_from`.
- `Observation::valid_until: Option<Timestamp>` — passed through to `Node.valid_until`.

#### Convenience API
- `Engine::learn(LearnInput)` — project knowledge injection (Semantic/Convention/Decision).
- `Engine::remember_peer(PeerProfileInput)` — peer profile recording under `peer/{id}/profile` scope. Auto-registers unknown peers.
- `Engine::log_activity(ActivityInput)` — activity recording under `peer/{id}/activity` scope. Supports `valid_from`/`valid_until`.
- `Engine::schedule(ScheduleInput)` — event scheduling with participants → entity_tags conversion.
- `Engine::ingest_document(DocumentInput)` — document chunk ingestion with automatic `Temporal` edge chain.
- `Engine::ingest_conversation(ConversationInput)` — raw episode + extracted facts with `ExtractedFrom` edges. `about_peer` updates peer profile.

#### ScopePath Convention
- `peer/{peer_id}/profile` — peer profile nodes (from `remember_peer()`).
- `peer/{peer_id}/activity` — peer activity nodes (from `log_activity()`, `schedule()`).

#### SQLite Schema v2
- `schema_version` table — tracks migration state.
- `nodes.peer_id INTEGER` — replaces `agent_id TEXT`.
- `nodes.source_kind TEXT` — new column.
- `edges.edge_source TEXT` — new column (`auto`, `manual`, `inferred`).
- `peers` table — peer registry storage.
- `peer_aliases` table — alias and platform username mappings.
- Auto-migration from v1 (no `schema_version` table) to v2.

### Deprecated

- `Engine::merge_candidates()` / `Engine::auto_merge()` — deprecated since 0.3.0. Use `EngineConfig::dedup_threshold` instead.

---

## [0.4.0] — 2025-05-12

- SqliteStorage replaces InMemoryStorage as the sole backend.
- Full cognitive engine with debug lifecycle, snapshots, embedding provider, and unified search.
