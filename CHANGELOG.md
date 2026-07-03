# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
