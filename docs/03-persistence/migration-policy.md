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

1. **Open every database any previous release wrote.** Schema migrations are
   **transactional** (each hop wraps `BEGIN IMMEDIATE` / `COMMIT`, rolling back on
   error), **idempotent** (`CREATE … IF NOT EXISTS`, and data rewrites whose
   selectors only match un-migrated rows), and **chained** — a single
   `SqliteStorage::open` runs any `vN → current` in one pass. There is no separate
   migration step or tool: opening the file migrates it.
2. **Never lose data.** A removed enum variant must **decode via a lossless
   fallback**, never error — an unknown persisted node type becomes
   `Custom(<original>)` (the original label survives verbatim); an unknown tier
   becomes `Auto`. A dropped table must be **migrated or archived**, never silently
   discarded of load-bearing data. Coarsenings are disclosed, not hidden.
3. **Ship tests.** A schema change is not done until it extends the suite:
   - a **per-hop** migration test (RED-provable — it must fail if the hop is removed),
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

`SCHEMA_VERSION = 8`. Each hop is a `migrate_vN_to_vM` function; the chain runs
forward from whatever version the opened file is at.

| Hop | Change |
|:--|:--|
| v1 → v2 | `agent_id TEXT` replaced by `peer_id INTEGER` + `source_kind TEXT`; `peers` / `peer_aliases` tables added. |
| v2 → v3 | `retained_action` reservoir table + edge `conductance` / `accessed_at` reservoir columns (ADR-0002); valid-interval and salience-projection indexes; reservoirs deterministically backfilled from the existing bounded projections. |
| v3 → v4 | Peer evidence-trust columns `trust_reservoir REAL` + `trust_evidence_count INTEGER`, seeded from each peer's coarse `trust_level` prior. |
| v4 → v5 | `nodes.evidence_prior REAL NOT NULL DEFAULT 0` — the decay-exempt prior `P_i` of `A_i = B_i + P_i` (ADR-0008), backfilled to `0.0` (backfilling from the old `retained_action` scalar would double-count access history). |
| v5 → v6 | **DROP** the `peers` / `peer_aliases` tables — the peer/trust subsystem was removed ([ADR-0014](../adr/0014-shrink-to-product.md)). Nodes' own `peer_id` / `source_kind` columns and the `idx_nodes_peer` index **stay**; no node data is touched. |
| v6 → v7 | **Legacy-type normalization.** Rewrites the `KnowledgeType` 15→4 collapse in place: the three legacy identity wire strings → bare `identity`, and every deleted knowledge/memory wire string (`procedural`, `entity`, `convention`, `decision`, `gotcha`, `hypothesis`, `evidence`, `debug_session`, `event`) → its canonical `custom:<string>` form, so `nodes_by_type` stops missing un-normalized rows. Data only, idempotent. |
| v7 → v8 | **Bare-unknown normalization.** Generalizes v7: every remaining bare, non-canonical `node_type` (an arbitrary string from a foreign/future writer) is rewritten to its canonical `custom:<escaped>` encoding (`%` / tab / CR / LF escaped in Rust via `encode_knowledge_type`), so it too is visible to `nodes_by_type`. Data only, idempotent. |

Note: this chain reflects `SCHEMA_VERSION = 8`. [ADR-0014](../adr/0014-shrink-to-product.md)
documents v5 → v6 and v6 → v7 as the shrink's migrations; v7 → v8 was added
afterward for foreign/future bare types. The `sqlite.rs` `migrate_schema` doc comment
is the authoritative per-hop record.

## Normative test suite

These tests **are** the policy's enforcement. Adding a schema version without
extending them violates the policy. Named exactly as they exist in the tree:

In [`crates/anamnesis/tests/schema_migration.rs`](../../crates/anamnesis/tests/schema_migration.rs):

- `existing_db_migrates_from_v1_to_current` — the **full chain** from a hand-built v1
  database through to `SCHEMA_VERSION`, asserting schema and data at the end.
- `fresh_schema_equals_migrated_schema` — **fresh == migrated convergence**: a
  brand-new DB and a fully-migrated old DB have identical schema (columns + indexes).
- `v5_db_with_planted_peers_reopens_clean_at_v6` — **per-hop** proof for the v5 → v6
  peer-table drop (RED-provable: fails if the drop is removed).
- `v5_db_with_bare_node_type_normalizes_through_full_chain_to_v8` — the **adversarial
  fixture**: plants a bare legacy `node_type` at v5 and proves it normalizes cleanly
  through the full chain to v8.
- `fresh_db_gets_current_schema_version` and `v3_backfill_is_deterministic_and_complete`
  — fresh-version and deterministic-backfill guards.

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

## Decode-fallback rule

The load path **never errors on an unknown persisted string**. Two vocabularies
currently have a fallback, and **every new persisted vocabulary must ship the same
rule**:

- **Node types.** An unrecognized `node_type` decodes to `Custom(<original>)` — the
  original string is preserved verbatim, so no future migration or foreign writer can
  cause a hard failure or silent data loss.
- **Memory tiers.** An unrecognized tier decodes to `Auto`.

This is why the normalization migrations (v7, v8) are safe: even before a row is
rewritten it already decodes losslessly in memory; the migration only closes the gap
where the *raw stored string* was invisible to `nodes_by_type` (which filters on the
encoded form). Decode-fallback is the safety net; normalization is the cleanup.
