# 0014. Shrink To The Product (Delete The Consumer-less Surface)

- Status: Accepted
- Date: 2026-07-03
- Version: v0.10.0
- Related: [ADR-0002](0002-reservoir-projection-state.md), [ADR-0008](0008-powerlaw-dissipation.md), [ADR-0012](0012-daemon-core-mcp-plugin-clients.md), [ADR-0013](0013-reasoning-capture-pipeline.md)

## Context

An audit of the Engine's public surface against its actual consumers found that **roughly 85% of it had zero callers** — not in `Memory` (the validated framework recipe), not in the MCP server, not in the plugin, not in the benchmarks, not anywhere but its own unit tests. The API was a catalogue of capabilities the project *described* rather than a surface the product *walked*. The map sold more than the territory.

Concretely, several whole subsystems were live in the type system and the docs but dead in practice:

- The **debug/hypothesis lifecycle** (`start_debug … end_debug`, `log_hypothesis`, `log_evidence`, `reject_hypothesis`, `confirm_hypothesis`, `search_rejected_hypotheses`, plus `EvidenceResult` / `DebugOutcome` and the `DebugSession` / `Hypothesis` / `Evidence` node types) — no consumer ever opened a debug session.
- A large **convenience API** on `Memory` and `Engine` (`learn`, `log_activity`, `schedule`, `apply_feedback`, `fact_at` convenience wrappers, `query_perspective`, `reflect_batch`, `support_report`, `Memory::consolidate` / `consolidate_at`, and their input types) — sugar with no caller.
- The **peer/trust subsystem** (`PeerRegistry`, `PeerProfile`, `TrustLevel`, `register_peer` and the other engine peer methods, the trust reservoir, trust-weighted readout) — production always ran with a single `PeerId(0)`; the readout trust term was already effectively constant.
- **15 `KnowledgeType` variants** where the product only ever wrote three (`Episodic`, `Semantic`, and occasionally `Identity`); the rest (`Procedural`, `Convention`, `Decision`, `Gotcha`, `Entity`, `Event`, the three identity tiers, and the debug family) were unreachable through any shipped path.
- The **`ScopeRelation` hierarchy** (`Ancestor` / `Descendant` / `Sibling` / `Disjoint` / `Equal` / `Universal`) — every scope in production was `universal`; the structural comparison machinery was never exercised.
- **`MemoryTier` manual override** (`set_tier` / `get_tier`) — tiers were only ever *read* as a display label derived from salience; no one set one.

Carrying this surface is not free: it is the thing the docs must keep honest, the thing every refactor must preserve, the thing a `PR #84`-style schema proposal must reconcile against. A surface with no consumer cannot be validated, cannot be calibrated, and quietly rots into a claim the code no longer supports. This is the same defect PR #84's audit surfaced from the other direction — a fourth parallel type vocabulary bridged only by lossy one-way projections with zero consumers, layered on an engine whose own vocabulary was already 4× larger than anything it wrote.

## Decision

**Delete the consumer-less surface.** Ship the product that is actually walked, and record what was removed, why, and the condition under which each piece may return.

### What was removed

| Removed | Notes |
|---|---|
| Debug/hypothesis lifecycle | All `*_debug` / `*_hypothesis` / `*_evidence` methods, `EvidenceResult`, `DebugOutcome`, and the `DebugSession` / `Hypothesis` / `Evidence` node types. |
| Convenience API | `learn`, `log_activity`, `schedule`, `apply_feedback`, `query_perspective`, `reflect_batch`, `support_report`, `Memory::consolidate` / `consolidate_at`, and their input types. |
| Peer/trust subsystem | `PeerRegistry`, `PeerProfile`, `TrustLevel`, engine peer methods, the trust reservoir. Readout's trust term is now a neutral `1.0`. |
| `KnowledgeType` 15 → 4 | Collapsed to `Episodic`, `Semantic`, `Identity`, `Custom(String)`. |
| `ScopeRelation` hierarchy | `ScopePath` is now an opaque canonical string plus `is_universal()`; scope scoring is a two-branch weight. |
| `MemoryTier` manual override | `set_tier` / `get_tier` removed. |

### By-design behavioral coarsenings (disclosed)

Collapsing the type taxonomy meant several types that formerly carried their own decay/tau *policy input* now share a rate. These are **coarsenings of policy inputs, not changes to the dynamics** — the base-level / evidence-prior model of [ADR-0008](0008-powerlaw-dissipation.md) is untouched — but they change observable decay for affected nodes, so they are disclosed here:

| Change | Before | After |
|---|---|---|
| `Event` decay multiplier `m_type` | `0.60` | `0.40` (now folds into `Custom` / `Semantic` ordinary-knowledge rate) |
| `Convention` / `Decision` decay `m_type` | `0.30` | `0.40` (ordinary-knowledge rate) |
| ex-inert `Hypothesis` / `Evidence` / `DebugSession` | `0.0` (inert, never decayed) | `0.40` when such legacy rows are decoded as `Custom` (the debug family no longer exists as a protected class) |
| `IdentityLearned` / `IdentityState` decay | slow but non-zero | `0.0` — merged into the single `Identity` variant, tick-protected (never decays) |
| Entity↔Entity `tau` special-case | dedicated seed-`tau` branch | dropped — Entity pairs use the ordinary seed distribution |

The current shipped multipliers are: `Identity = 0.0` (protected), `Semantic` / `Custom = 0.40`, `Episodic = 1.0`, over the intercept `α = 0.40` (`d_j = m_type · (c · e^{m_j} + α)`).

### What survives, and why

- **`PeerId` and `SourceKind` on `Origin`** — the *storage* of provenance is cheap, forward-compatible, and the natural anchor for a future multi-peer layer. Only the registry/trust *machinery* was removed, not the fields.
- **`KnowledgeType::Identity`** — the identity partition in the query pipeline still routes `Identity`-typed nodes and renders an `## IDENTITY` section; the type is a real, reachable retrieval prior even though the default `Memory` recipe (and thus the MCP surface) emits only `Episodic` + `Semantic`. (This is why the `recall` MCP tool's description dropped the word "identity": that agent-facing path never writes an `Identity` node, so advertising the section to the agent was misleading — the *engine* still supports it.)
- **`MemoryTier` as a display label** — the enum and the salience→label mapping survive; only the manual setter/getter went.
- **The internal module tree** (`api`, `graph`, `mechanics`, `query`, `storage`, `embedding`, `snapshot`, …). A B7 finding worth recording: these paths, doc-hidden at the two-door root boundary since [v0.7](../../README.md#status), are frequently mislabeled "legacy." They are **the real implementation tree** — the crate compiles against them and they carry hundreds of internal references across the workspace. What the two-door boundary changed is only the *root re-exports* (`Memory`, `Engine`, `Error`); the modules were never a public API and are not going away. Do not "clean up the legacy modules" — there is nothing legacy about them.

### Migrations

Both run automatically on `SqliteStorage::open`:

- **v5 → v6**: drops the `peers` / `peer_aliases` tables (the peer/trust subsystem is gone).
- **v6 → v7**: normalizes legacy `node_type` rows — the removed variants' wire strings decode to `Semantic`, `Identity`, or `Custom(<original>)` and are rewritten in place, so an old database opens cleanly with no data loss (the original label survives as `Custom`).

## Consequences

- The public surface now matches the product. Every remaining Engine method has a consumer in `Memory`, the MCP server, the plugin, or the benchmarks. The docs can be kept honest against a surface that is actually exercised.
- Databases written by ≤ 0.9.x migrate forward automatically; there is no forward-compat break for stored data, only for the API.
- Downstream code using any removed method breaks at compile time (intentional — this is a breaking release, 0.10.0). The [Roadmap](../../README.md#roadmap) records the re-add path for each subsystem.

### Re-add condition

Nothing here is re-added on speculation. Each removed subsystem returns **only when a real consumer exists** — a shipped path (in `Memory`, the plugin, or a concrete integration) that would call it — not because the design is elegant or because a benchmark might hypothetically use it. The audit's core lesson is that a surface without a consumer is a liability, not an asset.

## Follow-up track

- **Physics reconnect-or-demote gate.** Several mechanics (e.g. the energy objective, desirable-difficulty reinforcement) are implemented but only weakly wired into the shipped readout. The follow-up is a gate: each must either be reconnected to a path the product walks, or demoted to explicitly-experimental status — no third "present but dead" state.
- **Schema redesign (ADR-0015, future).** The post-shrink engine is the right substrate to redesign the storage/type schema on. That redesign explicitly **inherits the vocabulary ideas from PR #84** (a `MemoryKind` / `EntityKind` split) as *design input* — now that the engine's own type vocabulary is small (4 variants) and consumer-driven, a deliberate schema can be designed against real usage rather than bolted on as a fourth parallel vocabulary.
- **Scale work when graphs actually grow.** The O(N)-scan storage defaults and the single un-extracted queue are adequate at current graph sizes. Indexing and per-namespace queues are deferred until real deployments produce graphs large enough to need them — again, consumer-gated, not speculative.
