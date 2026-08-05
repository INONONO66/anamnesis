# Migration Policy

Anamnesis stores memory in a SQLite file the user owns and keeps across upgrades.
That makes forward migration a **product guarantee**, not an implementation detail:
a release that cannot open the database a previous release wrote has lost the user's
memory. This document is the normative contract — what every breaking release must
do, the current schema chain, and the test suite that enforces it.

The schema code lives in [`sqlite.rs`](../../crates/anamnesis/src/storage/sqlite.rs)
(`SCHEMA_VERSION`, `migrate_schema`, the `migrate_vN_to_vM` functions); the
guarantees are enforced by the tests named in [Normative test suite](#normative-test-suite).

## Policy

Every breaking release **MUST**:

1. **Open every database any previous release wrote.** Every hop is
   **transactional**: it wraps `BEGIN IMMEDIATE` / `COMMIT`, rolls back on error,
   and stamps its target version inside that transaction. Hop payloads use the
   guarded DDL, deterministic backfills, or selector-limited rewrites documented
   in the source; not every data rewrite is selector-limited. The chain is
   **chained** — a single `SqliteStorage::open` runs any `vN → current` in one
   pass. There is no separate migration step or tool: opening the file migrates it.
2. **Never lose data.** A removed enum variant must **decode via a lossless
   fallback**, never error — an unknown persisted node type becomes
   `Custom(<original>)` (the original label survives verbatim); an unknown tier
   becomes `Auto`. A dropped table must be **migrated or archived**, never silently
   discarded of load-bearing data. Coarsenings are disclosed, not hidden.
3. **Ship tests.** A schema change is not done until it extends the suite:
   - a **per-hop** migration test that fails if the hop is removed,
   - a **full-chain** test from the oldest supported version to current,
   - a **fixture** that plants adversarial values (bare/foreign/legacy strings) and
     proves they survive the chain.
4. **Smoke a real pre-upgrade database.** Before tagging a breaking release, open a
   **real copy** of a database written by the previous release and confirm it
   migrates and reads back — the fixtures cover the mechanism, the smoke covers the
   territory.

`SOURCE WINS`: if this document and the code disagree, the code is authoritative and
this document is the bug. Keep them in sync.

## Current chain

`SCHEMA_VERSION = 13`. Each hop is a `migrate_vN_to_vM` function; the chain runs
forward from whatever version the opened file is at.

| Hop | Change |
|:--|:--|
| v1 → v2 | `agent_id TEXT` replaced by `peer_id INTEGER` + `source_kind TEXT`; `peers` / `peer_aliases` tables added. |
| v2 → v3 | `retained_action` reservoir table + edge `conductance` / `accessed_at` reservoir columns (ADR-0002); valid-interval and salience-projection indexes; reservoirs deterministically backfilled from the existing bounded projections. |
| v3 → v4 | Peer evidence-trust columns `trust_reservoir REAL` + `trust_evidence_count INTEGER`, seeded from each peer's coarse `trust_level` prior. |
| v4 → v5 | `nodes.evidence_prior REAL NOT NULL DEFAULT 0` — the decay-exempt prior `P_i` of `A_i = B_i + P_i` (ADR-0008), backfilled to `0.0` (backfilling from the old `retained_action` scalar would double-count access history). |
| v5 → v6 | **DROP** the `peers` / `peer_aliases` tables — the peer/trust subsystem was removed ([ADR-0014](../adr/0014-shrink-to-product.md)). Nodes' own `peer_id` / `source_kind` columns and the `idx_nodes_peer` index **stay**; no node data is touched. |
| v6 → v7 | **Legacy-type normalization.** Rewrites the `KnowledgeType` 15→4 collapse in place: the three legacy identity wire strings → bare `identity`, and every deleted knowledge/memory wire string (`procedural`, `entity`, `convention`, `decision`, `gotcha`, `hypothesis`, `evidence`, `debug_session`, `event`) → its canonical `custom:<string>` form, so `nodes_by_type` stops missing un-normalized rows. Data only, idempotent. |
| v7 → v8 | **Bare-unknown normalization.** Generalizes v7: each non-canonical `node_type` selected by the source predicate (not a canonical bare variant and not matching SQLite `LIKE 'custom:%'`) is rewritten to canonical `custom:<escaped>` encoding (`%` / tab / CR / LF escaped in Rust via `encode_knowledge_type`), making that stored value visible to `nodes_by_type`. Data only; rows selected by the predicate normalize idempotently. |
| v8 → v9 | **Creation-trace backfill.** Every node whose legacy `access_history` is empty receives the same creation `AccessTrace` seeded by ingest: timestamped at `created_at` (falling back to the hot-field `accessed_at`) with decay `m_type * DECAY_INTERCEPT`. Data only; the empty-history selector makes the hop idempotent. |
| v9 → v10 | Add `edges.leaked_at INTEGER NOT NULL DEFAULT 0`, the per-edge idle-leak checkpoint, and backfill it from each edge's `accessed_at`. The guarded column addition and deterministic backfill converge when retried before post-migration writes. |
| v10 → v11 | Add the `graph_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL)` table for graph-wide persistent metadata, initially the embedding-model identity and migration checkpoint state. `CREATE TABLE IF NOT EXISTS` makes the hop idempotent. |
| v11 → v12 | Add the isolated `atomic_facts` routing sidecar with embeddings, cited raw-source ids, source session/scope, validity, and metadata. These rows remain outside graph topology and reader evidence. |
| v12 → v13 | Add reviewed typed `atomic_fact_relations` with endpoint foreign keys, reviewer/profile/time, idempotency, scope, validity, audit metadata, and directed adjacency indexes. |

Opening a database also initializes the storage-owned node-incarnation metadata
used by atomic-fact source validation. This is a runtime metadata backfill, not a
schema hop: each legacy live node receives a unique generation and the monotonic
high-water mark is persisted in the same transaction. The reserved value is
removed from the public `Node::metadata` map after load. Existing atomic-fact
source bindings are never inferred or rewritten. A fact with a missing or
obsolete binding remains stored but ineligible until a consumer reviews and
writes it again.

The MCP-owned policy side schema is versioned independently. Its v4 → v5 hop
adds the per-candidate source-incarnation vector used by extraction review and
promotion. Existing rows receive an empty vector: they remain available for
audit, but candidate and relation review/promotion fail closed because the
original source allocation cannot be reconstructed safely.

Note: this chain reflects `SCHEMA_VERSION = 13`. [ADR-0014](../adr/0014-shrink-to-product.md)
documents v5 → v6 and v6 → v7 as the shrink's migrations. The `sqlite.rs`
`migrate_schema` doc comment is the authoritative per-hop record.

## Normative test suite

These tests **are** the policy's enforcement. Adding a schema version without
extending them violates the policy. Named exactly as they exist in the tree:

In [`crates/anamnesis/tests/schema_migration.rs`](../../crates/anamnesis/tests/schema_migration.rs):

- `existing_db_migrates_from_v1_to_current` — the **full chain** from an empty,
  hand-built v1 database through v13, asserting the final schema/version and the
  v11 metadata, v12 atomic-fact, and v13 atomic-relation tables.
- `fresh_schema_equals_migrated_schema` — **fresh == migrated convergence**: a
  brand-new v13 DB and a fully migrated old DB have identical edge columns
  (including v10 `leaked_at`), `graph_metadata`, atomic-fact, and
  atomic-relation columns and indexes. Removing a schema hop makes this test
  fail.
- `v5_db_with_planted_peers_reopens_clean_at_v6` — **per-hop** proof for the v5 → v6
  peer-table drop and fails if the drop is removed.
- `v5_db_with_bare_node_type_normalizes_through_full_chain_to_v8` — the historically
  named **adversarial fixture**: plants an arbitrary bare value from a
  foreign/future writer at v5, proves normalization at v8, and verifies that the
  remaining chain lands at v13.
- `fresh_db_gets_current_schema_version` — verifies that fresh storage is stamped
  v13 and contains the current tables.
- `v3_backfill_is_deterministic_and_complete` — deterministic reservoir-backfill
  guard for the v2 → v3 hop.
- `v9_db_adds_and_backfills_edge_leak_checkpoint` — direct v9 → v10 column and
  default/backfill proof.
- `v10_db_adds_graph_metadata_table` — direct v10 → v11 table-shape proof.
- `v11_db_adds_atomic_fact_sidecar` — direct v11 → v12 table/index proof,
  followed by the v12 → v13 relation hop.

In [`crates/anamnesis/tests/migration_backfill.rs`](../../crates/anamnesis/tests/migration_backfill.rs):

- `v9_backfills_creation_trace_for_legacy_empty_access_history` — direct v8 → v9
  proof that an empty legacy history receives exactly one creation trace with
  the authoritative timestamp and decay.
- `v9_backfill_is_idempotent_and_leaves_populated_history_untouched` — proves the
  empty-history selector preserves an already-populated history during migration.

In [`crates/anamnesis/tests/legacy_db_tick_recall.rs`](../../crates/anamnesis/tests/legacy_db_tick_recall.rs):

- `migrated_legacy_empty_history_db_ticks_and_stays_recallable` — despite its
  historical name, proves v8 → v9 open/migrate/tick behavior, one creation trace,
  and finite post-tick salience; it does not execute recall.

In [`crates/anamnesis/tests/edge_leak_idempotent.rs`](../../crates/anamnesis/tests/edge_leak_idempotent.rs):

- `repeated_tick_at_same_now_leaks_idle_edge_only_once` — behavior evidence for
  the v10 `leaked_at` checkpoint: the first tick lowers conductance, and the
  value observed after tick two remains unchanged through tick five. It does
  not compare the first and second post-tick values or exercise the migration.

In [`crates/anamnesis/tests/migration_replay.rs`](../../crates/anamnesis/tests/migration_replay.rs):

- `replay_after_a_stale_version_stamp_does_not_brick_an_already_migrated_db` —
  reopens a current schema stamped as v4 and verifies replay reaches v13.
- `replay_from_stale_v1_against_current_schema_does_not_brick` — repeats that
  stale-version guard from v1. These are broad replay guards, not direct v10 or
  v11 per-hop fixtures.

In [`crates/anamnesis/src/storage/sqlite.rs`](../../crates/anamnesis/src/storage/sqlite.rs)
(unit tests / fixtures):

- `unknown_node_types_decode_as_custom_on_reopen` — an unknown persisted node type
  reopens as `Custom(<original>)`.
- `fallback_decoded_node_type_round_trips_stably` — a fallback-decoded node type
  re-encodes and re-decodes to the same value (no drift on rewrite).
- `known_node_types_are_untouched_by_fallback` — canonical types are never mangled by
  the fallback path.
- `decode_memory_tier_falls_back_to_auto_on_unknown` — an unknown persisted tier
  decodes to `Auto`.
- `migration_v7_normalizes_legacy_node_types_for_nodes_by_type` and
  `migration_v8_normalizes_arbitrary_bare_node_types_for_nodes_by_type` — the v7 and
  v8 normalization hops make previously-invisible rows queryable by `nodes_by_type`.
- `migration_v12_relation_schema_matches_fresh_schema` — an isolated v12 → v13
  migration produces the same atomic-relation columns and indexes as a fresh
  v13 database and durably seeds the sidecar ID high-water marks.
- `reopening_backfills_missing_node_incarnation_once` — a legacy node receives
  one durable backend-owned generation, hidden from the public metadata map, and
  keeps it across subsequent opens.
- `byte_identical_node_id_reuse_rotates_incarnation_across_reopen` and
  `storage_clone_preserves_deleted_incarnation_high_water` — deletion, numeric-id
  reuse, reopen, and clone cannot recycle source authority even when replacement
  node bytes are identical.
- `authority_field_change_invalidates_existing_atomic_fact_binding` — changing
  evidence or provenance fields on the same live allocation invalidates its
  earlier fact binding without rotating the allocation generation.
- `reopening_leaves_unverifiable_atomic_source_ineligible` — opening never binds
  a legacy atomic fact to the node currently occupying its cited numeric id.
- `reopening_rejects_malformed_or_duplicate_node_incarnations` — corrupt backend
  generations fail open instead of aliasing two source allocations.

In [`crates/anamnesis/tests/sqlite_storage.rs`](../../crates/anamnesis/tests/sqlite_storage.rs):

- `graph_metadata_round_trips_and_persists` — v11 table behavior: one graph-wide
  metadata write survives reopen.

## Decode-fallback rule

Node types and memory tiers are the two persisted vocabularies whose load paths
currently tolerate unknown values. Other persisted enums remain strict and can
return a storage error. Every newly introduced persisted vocabulary **MUST**
define its compatibility behavior explicitly:

- **Node types.** An unrecognized `node_type` decodes to `Custom(<original>)` — the
  original string is preserved verbatim, so no future migration or foreign writer can
  cause a hard failure or silent data loss.
- **Memory tiers.** An unrecognized tier decodes to `Auto`.

This is why the normalization migrations (v7, v8) are safe: even before a row is
rewritten it already decodes losslessly in memory; the migration only closes the gap
where the *raw stored string* was invisible to `nodes_by_type` (which filters on the
encoded form). Decode-fallback is the safety net; normalization is the cleanup.
