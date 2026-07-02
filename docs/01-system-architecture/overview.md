# Architecture Overview

Anamnesis is a synchronous Rust library for cognitive memory. It owns graph storage, graph traversal, cognitive dynamics, snapshots, and context packaging. It does not own LLM calls, extraction, remote synchronization, or session orchestration.

## System Boundary

| Inside Core | Outside Core |
|---|---|
| Graph data model | LLM extraction prompts |
| Storage adapter trait | Embedding model download policy |
| SQLite default adapter | HTTP/RPC server |
| Perception, conductance, dissipation, frustration | Session management |
| Search and query pipeline | Authorization and tenancy |
| Snapshot and restore | UI and visualization |
| Origin, scope, temporal validity | Remote sync |

The core must remain deterministic for the same graph state and query input. Optional embedding providers may exist behind features, but embedding generation is a provider boundary, not a core behavior.

## Main Modules

| Module | Responsibility |
|---|---|
| `memory` | **Framework API** (`Memory`) — bench-proven ingest recipe; see [framework-layer.md](framework-layer.md) |
| `graph` | Site, edge, id, origin, scope, time, and type definitions |
| `storage` | `StorageAdapter` trait and default SQLite adapter |
| `mechanics` | Pure scoring, decay, conductance, and interaction functions |
| `query` | Activation flow, readout, packaging, and trace assembly |
| `snapshot` | Clone-based snapshot storage |
| `embedding` | Provider trait and optional local provider |
| `api` | Public `Engine` surface and request/result types (**Kernel API**) |

## Three Loops

```mermaid
flowchart TB
    ingest["Ingest"] --> graph["Graph state"]
    tick["Maintenance / tick"] --> graph
    graph --> query["Query / search"]
    query --> package["ContextPackage"]
    package --> commit["Committed usage"]
    commit --> graph
```

1. **Ingest loop.** Observations pass through perception, allocate or route to sites, seed the evidence prior `P_i` with encoding surprise, and may create initial conductance.
2. **Maintenance loop.** Time ages base-level activation `B_i` (recomputed from each node's access-history window at the new `now`) and applies edge leakage; the evidence prior `P_i` is not use-decayed.
3. **Retrieval loop.** Queries compute transient activation, package context, and only update persistent state when use is committed (a committed access appends an access trace, raising `B_i`).

## Public Surface

The engine is generic over storage:

```rust
pub struct Engine<S: StorageAdapter + Clone = SqliteStorage> {
    graph: Graph<S>,
    config: EngineConfig,
    snapshots: SnapshotStore<S>,
}
```

Default construction uses in-memory SQLite. Persistent use passes a file-backed `SqliteStorage` or another adapter implementing the same trait.

## EngineConfig

| Field | Meaning |
|---|---|
| `max_nodes` | Hard budget before perception rejects low-value new observations |
| `novelty_threshold` | Backward-compatible projection of pattern-separation threshold |
| `confidence_threshold` | Minimum origin confidence for admission |
| `dedup_threshold` | Backward-compatible duplicate routing threshold |
| `dedup_enabled` | Enables duplicate routing instead of unconditional allocation |
| `decay_model` | ACT-R activation-dependent power-law dissipation model (Pavlik & Anderson 2005); the multi-trace base-level form `B_i = ln( Σ_j (now − t_j)^(−d_j) )` over the node's access-history window, where each trace stores its own decay rate `d_j = m_type·(c·e^{m_j} + α)` computed at creation from current activation |
| `energy_model` | Readout scoring objective |
| `spreading_model` | Activation-flow traversal model |

Not all of these are calibrated priors. `max_nodes` and `dedup_enabled` are operational knobs; `decay_model`, `energy_model`, and `spreading_model` select a formula family. The threshold fields (`novelty_threshold`, `confidence_threshold`, `dedup_threshold`) are calibrated priors that project onto the underlying behavioral priors (see [ADR-0010](../adr/0010-calibrated-priors-not-laws.md)); they are not physical constants, and consumers may refit them to their graph statistics.

## Core Method Contracts

| Method | Contract |
|---|---|
| `ingest` | Applies perception, creates or routes a site, and returns created/reinforced ids |
| `query` | Runs retrieval without hidden mutation and returns `ContextPackage` |
| `search` | Fuses text, vector, lexical, temporal, scope, and graph cues before packaging |
| `touch` | Computes `d_now = m_type·(c·e^{m_now} + α)` from the current activation `m_now` of existing traces, then appends a committed access trace `(now, d_now)`, raising `B_i` (the base-level sum ages prior traces to `now` before adding the new one with its stored `d_now`, so decay-first ordering is intrinsic) |
| `tick` | Ages `B_i` by recomputing from each node's access-history window at `now` (`P_i` is not use-decayed) and flushes storage |
| `link` | Creates or strengthens an edge through a typed relationship |
| `crystallize` | Adds a synthesis site and `ConsolidatedFrom` edges; never overwrites sources |
| `snapshot` / `restore` | Captures and restores cloned storage state |
| `fact_at` | Filters facts by valid-time interval |

The debug lifecycle methods and `reflect_batch` (cross-agent entity linking) were **removed in the [v0.10.0 shrink](../adr/0014-shrink-to-product.md)** — both had no consumer; see ADR-0014 for the re-add conditions.

## Embedding Boundary

The core stores and compares embeddings when provided, but it does not decide how they are generated. `EmbeddingProvider` is synchronous and thread-safe. Optional providers may download local models on first use; that side effect belongs to the provider, not to the core engine.

## Core Boundary

The core must not:

- start background tasks,
- open network connections,
- call an LLM API,
- mutate graph state during read-only retrieval,
- hide storage errors behind panics,
- treat display projections as authoritative reservoirs.

## Debug Lifecycle

> **Removed in [v0.10.0](../adr/0014-shrink-to-product.md).** Debugging was formerly modeled as first-class graph state (`DebugSession` → `Hypothesis` → `Evidence` → confirmed/rejected, inert against dissipation, with rejected hypotheses kept searchable). That lifecycle had no consumer and was removed in the shrink. A reasoning-session capture may return through the [capture pipeline](../adr/0013-reasoning-capture-pipeline.md); see ADR-0014 for the re-add condition.

## Data Flow

```mermaid
flowchart LR
    input["Observation / Query"] --> boundary["API boundary"]
    boundary --> storage["StorageAdapter"]
    storage --> mechanics["Pure mechanics"]
    mechanics --> query["Activation / readout"]
    query --> output["ContextPackage / report"]
```

Storage access is orchestrated by `Engine`. Mechanics functions take typed inputs and return typed outputs so they can be tested without a database.

## Non-Functional Requirements

| Requirement | Target |
|---|---|
| Determinism | Same graph state and query produce same retrieval result |
| Local-first | Default engine needs no server or network |
| Synchronous core | No async runtime required by the library |
| Pluggability | Storage adapters satisfy one trait |
| Traceability | Search, tick, reflect, and commit paths expose structured reports |
| Bounded projections | Salience and edge weight stay in the closed public range `[0, 1]` — salience is the bounded logistic projection of the unbounded sum, `s_i = logistic(B_i + P_i)`, whose extreme values saturate to the `0`/`1` endpoints in storage (the `projection_range` invariant validates `[0, 1]`) |
| No hidden mutation | Retrieval does not change reservoirs unless committed |

## Dependency Direction

API orchestrates storage and mechanics. Mechanics must not depend on SQLite. Storage must not know query semantics beyond indexes and typed fields. Query may read graph structure and mechanics outputs but must not own persistence policy.
