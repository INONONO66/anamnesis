# Architecture Overview

> **Implementation status:** v0.3.0 — Cognitive engine operational. Core mechanics, scoped graph recall, unified search, crystallization, snapshots, debug lifecycle, and bitemporal queries are implemented. See [Implementation Status](#implementation-status) for method-level detail.

## High-Level Design

Anamnesis is a cognitive dynamics engine that models knowledge as a directed graph with cognitive dynamics. The architecture consists of six main layers:

### 1. Graph Layer (`src/graph/`)

Core data structures representing the knowledge graph:

- **Node**: Represents a concept or entity with:
  - Unique ID
  - Multi-resolution content: `name` (L0 label), `summary` (L1 optional), `content` (L2 full text)
  - Salience (importance/recency score, written by all mechanics)
  - Timestamps: created_at, updated_at, accessed_at
  - Embedding: persisted vector for similarity operations (consumer-provided)
  - **KnowledgeType**: 12 variants — 3 Identity tiers + 6 Knowledge types + 2 Memory types + Custom
  - **Origin**: agent_id, session_id, project_id, confidence — provenance on every node
  - **entity_tags**: List of entity identifiers for automatic cross-node linking
  - **valid_from / valid_until**: Temporal validity for time-bounded facts

- **Edge**: Represents a relationship between two nodes with:
  - Source and target node IDs
  - **EdgeType**: 12 variants, each with a directional κ (kappa) propagation multiplier
  - Weight (strength of relationship)
  - created_at timestamp
  - Metadata

- **Graph**: Generic container `Graph<S: StorageAdapter = InMemoryStorage>` — manages nodes and edges via static dispatch over the storage backend

### 2. Mechanics Layer (`src/mechanics/`)

Implements cognitive dynamics as pure scoring and propagation functions (no side effects, no storage access):

- **Attraction** ✅: Similarity-based auto-linking
  - Cosine similarity σ + type affinity multiplier τ
  - Attraction score: A = σ × τ × (1 + 0.20m) (eq 3)
  - Wired into `ingest()` — creates up to 4 edges to the most similar candidates (last 256 + entity-tag matches)

- **Gravity** ✅: Centrality-based importance
  - Node mass: m = 0.55s + 0.30c + 0.15μ (eq 1, where s=salience, c=access count normalized, μ=type prior)
  - Gravity boost in spreading activation: 1 + 0.20m (eq 6)
  - Wired into `ingest()` and `query()`

- **Perception** ✅: Input gating
  - Computes max cosine similarity of new observation to existing nodes
  - Rejects if confidence too low, node budget exceeded, or observation too similar to existing knowledge (novelty threshold)
  - Wired into `ingest()` before node creation

- **Forgetting** ✅: Temporal decay
  - Exponential decay with type-specific floor: s(t+dt) = b + (s−b)·exp(−λ·dt) (eq 4)
  - Reinforcement on access: s ← s + 0.20·(1−s) (eq 5)
  - IdentityCore: no decay (floor = 1.0); Episodic: fast decay; Semantic: moderate decay
  - Lazy decay applied in `touch()` before reinforcement; batch decay in `tick()`

- **Repulsion** ✅: Contradiction damping
  - Repulsion accumulation: H = Σ w·ρ·X (eq 7)
  - Activation damping: X' = X·exp(−1.5·H) (eq 8)
  - Applied in Associative query pipeline (stage 4)

### 3. Query Layer (`src/query/`)

Implements graph traversal and structured output assembly:

- **Trigger-based Search** ✅: Text, keyword, and optional vector similarity locate candidate seed nodes
  - Trigger indexes are access paths, not memory stores
  - `StorageAdapter::text_search()` provides the lexical seam; richer backends may override it with BM25/FTS
  - `SearchInput::query_embedding` provides semantic cues when the consumer supplies an embedding
  - Candidate seeds enter graph recall through spreading activation

- **Spreading Activation** ✅: Priority-queue BFS from initially activated nodes
  - Activation spreads through edges with per-hop decay δ, salience gate ψ(s), gravity boost
  - Budget-constrained, cycle-safe (visited set), max-hops limit
  - Contradicts edges excluded from propagation (handled separately as repulsion)

- **ContextPackage Assembly** ✅: Structured output for LLM consumption
  - Partitioned into `identity` / `knowledge` / `memories` / `tensions`
  - Resolution (L0/L1/L2) assigned per token budget; top-3 knowledge fragments upgraded to L2 if budget allows
  - Agent tension computed via eq 14

- **Non-Associative modes** ✅: TypeFiltered, Neighborhood, Temporal, List — structural retrieval modes for typed, local, time-bounded, and salience-filtered access

### 4. Storage and Index Layer (`src/storage/`)

Abstracts the storage backend behind a trait:

- **StorageAdapter**: Interface for node/edge persistence — 21 methods across 6 groups:
  - ID allocation: `next_node_id()`, `next_edge_id()` (reuse freed IDs from free-list stacks)
  - Node CRUD: `set_node()`, `get_node()`, `get_node_mut()`, `delete_node()`
  - Edge CRUD: `set_edge()`, `get_edge()`, `get_edge_mut()`, `delete_edge()`
  - Adjacency index: `edges_from()`, `edges_to()` — O(degree)
  - SoA hot fields: `get_salience()`, `set_salience()`, `get_accessed_at()`, `set_accessed_at()`, `get_node_type()` — O(1) direct array access
  - Counts & iteration: `node_count()`, `edge_count()`, `all_node_ids()`, `all_edge_ids()`

- **InMemoryStorage** ✅: Arena-based `Vec<Option<Node>>` with SoA hot fields
  - `nodes: Vec<Option<Node>>` — arena with tombstones for O(1) access by ID
  - `salience: Vec<f64>`, `accessed_at: Vec<Timestamp>`, `node_types: Vec<Option<KnowledgeType>>` — Struct-of-Arrays for cache-friendly mechanics iteration
  - `adjacency_out: Vec<Vec<EdgeId>>`, `adjacency_in: Vec<Vec<EdgeId>>` — outgoing/incoming adjacency lists
  - ID recycling: `free_node_ids: Vec<NodeId>`, `free_edge_ids: Vec<EdgeId>` stacks
  - SoA mutation invariant: `get_node_mut()` does not update SoA arrays; callers must use `set_salience()` / `set_accessed_at()` for hot fields

- **Trigger indexes** ✅/🔜: Rebuildable projections from graph nodes to candidate `NodeId`s
  - Text/keyword search returns `(NodeId, score)` and never becomes the source of truth
  - Embeddings stored on nodes are semantic cues, not standalone memories
  - BM25, FTS, ANN, or other optimized indexes belong in storage adapters or optional features
  - Every index hit must resolve back to graph nodes and edges before packaging

- **Future Implementations**: SQLite, PostgreSQL adapters (implement `StorageAdapter` trait)

### 5. Cognitive Physics Layer (`src/mechanics/` + query scoring)

The graph behaves like a memory field rather than a passive record store:

- **Attraction** links similar or co-mentioned fragments so future cues can cross between them.
- **Gravity** gives high-salience, high-centrality, high-prior nodes more pull during ingestion and recall.
- **Forgetting** decays unused salience while preserving the node for precise reactivation.
- **Reinforcement** via `touch()` revives accessed nodes after applying lazy decay.
- **Spreading activation** reconstructs context from a seed through typed edges.
- **Repulsion** damps contradictory paths and surfaces tensions instead of silently merging conflicts.

Indexes answer “where should recall start?” The cognitive graph answers “what else comes back with it, why, from whom, and when?”

### 6. API Layer (`src/api/`)

Public interface for consumers:

```rust
/// Engine<S: StorageAdapter = InMemoryStorage>
pub struct Engine<S: StorageAdapter = InMemoryStorage> {
    graph: Graph<S>,
    config: EngineConfig,
}

impl Engine<InMemoryStorage> {
    /// Default in-memory engine, default configuration
    pub fn new() -> Self;
    /// Custom configuration, in-memory storage
    pub fn with_config(config: EngineConfig) -> Self;
}

impl<S: StorageAdapter> Engine<S> {
    /// Custom storage backend
    pub fn with_storage(config: EngineConfig, storage: S) -> Self;

    /// Ingest an observation — applies perception gate, then attraction auto-linking
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error>;
    /// Create or strengthen a typed link between two nodes
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType, weight: f64) -> Result<EdgeId, Error>;
    /// Lazy decay (eq 4) then reinforcement on access (eq 5)
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error>;
    /// Batch decay all nodes (eq 4), returns TickReport
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    /// Query — returns structured ContextPackage for LLM consumption
    /// Associative mode: full 7-stage pipeline ✅
    /// Other modes: return ContextPackage::empty() ⬚
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error>;

    /// ⬚ Returns empty Vec (planned: attraction-based candidate detection)
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<MergePair>, Error>;
    /// ⬚ Returns empty MergeLog (planned: merge with undo log)
    pub fn auto_merge(&mut self, threshold: f64) -> Result<MergeLog, Error>;
    /// ⬚ Returns empty ReflectReport (planned: cross-agent entity linking)
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;
}
```

## Data Flow

1. **Ingestion**: Observation → Perception gate (novelty/confidence/budget) → Node creation → Attraction auto-linking → Graph
2. **Linking**: Manual `link()` call or automatic during `ingest()` via attraction scoring
3. **Decay**: Periodic `tick()` applies batch forgetting to all nodes; lazy decay computed in `touch()` before reinforcement
4. **Search / Query**: trigger indexes find seeds → Associative mode runs 7-stage graph recall pipeline → `ContextPackage`
5. **Reinforcement**: `touch()` strengthens accessed nodes (lazy decay first, then salience boost)
6. **Batch Reflect** 🔜: `reflect_batch()` will create cross-agent Entity edges by matching shared `entity_tags` — no LLM calls, metadata matching only

## Recall Architecture: Triggers vs. Memory

Anamnesis separates access paths from memory representation:

| Layer | Examples | Responsibility |
|:------|:---------|:---------------|
| Trigger indexes | keyword search, BM25/FTS, optional embeddings, entity tags, temporal filters | Find candidate `NodeId`s that may start recall |
| Cognitive graph | nodes, typed edges, salience, origin, validity windows, access counts | Store memory and its relationships |
| Physics | attraction, gravity, forgetting, reinforcement, spreading activation, repulsion | Decide how memories connect, fade, revive, and activate |
| Packaging | identity/knowledge/memories/tensions with L0/L1/L2 resolution | Shape activated memory for the consumer’s context budget |

The intended search flow is:

```text
SearchInput
  ├─ lexical triggers: text_search / BM25 / exact keywords
  ├─ semantic triggers: query embedding vs. node embeddings
  ├─ structural triggers: entity tags, type filters, temporal windows
  ▼
Candidate NodeIds + per-source ranks/scores
  ▼
Seed fusion (for example, reciprocal rank fusion)
  ▼
Spreading activation over the graph
  ▼
ContextPackage with provenance, time, reasoning edges, and tensions
```

This boundary is intentional: changing an embedding model, tokenizer, BM25 parameters, or storage adapter must not rewrite the memory itself. Indexes are rebuildable projections. The graph is the source of truth.

## Associative Query Pipeline (7 Stages)

> Implemented in `src/api/mod.rs` (`query_associative`) and `src/query/`.

| Stage | Description | Key Equation |
|:------|:------------|:-------------|
| 1 | **Identity collection** — gather IdentityCore/Learned/State nodes for `config.agent_id` | — |
| 2 | **Initial activation** — seed gets 0.60, all nodes scored by vector sim + identity prior | eq 10: y⁰ = clamp(0.60·seed + 0.30·q_sim + 0.10·I_prior, 0, 1) |
| 3 | **Spreading activation** — priority-queue BFS, depth-aware, cycle-safe | eq 11: y_j = y_i · w · κ · δ · ψ(s_j) · (1 + 0.20m_j) |
| 4 | **Repulsion damping** — Contradicts edges reduce activation of contradicted nodes | eqs 7-8: H = Σ w·ρ·X; X' = X·exp(−1.5·H) |
| 5 | **Final scoring** — weighted combination of activation, vector sim, salience, mass, scope weight | eq 13: R = (0.50X' + 0.20q + 0.15s + 0.15m) · scope_w |
| 6 | **Contradiction & identity collection** — gather active Contradicts edges and identity activations | — |
| 7 | **ContextPackage assembly** — partition identity/knowledge/memories, token budget, tensions, agent tension | eq 14: T = Σ ρ·Σ w·X |

The salience gate ψ(s) = 0.2 + 0.8s ensures low-salience nodes receive some activation (floor 0.2 instead of 0).

Scope weights in stage 5: same project → 1.0, universal node → 0.85, other project → 0.30 (+ entity overlap bonus: 1 shared tag → +0.15, 2+ → +0.25, capped at 1.0).

## Type System

### KnowledgeType (12 variants)

Three classes of cognitive matter with different decay rates and mass priors:

| Class | Variants | Decay | Role |
|:------|:---------|:------|:-----|
| **Identity (Star)** | `IdentityCore`, `IdentityLearned`, `IdentityState` | None / Very slow / Normal | Agent persona, traits, current state |
| **Knowledge (Planet)** | `Semantic`, `Procedural`, `Entity`, `Convention`, `Decision`, `Gotcha` | Moderate | Facts, patterns, rules, warnings |
| **Memory (Dust)** | `Episodic`, `Event` | Fast | Raw conversation turns, time-bound events |
| **Custom** | `Custom(String)` | Default | Consumer-defined |

Identity nodes bias initial activation (stage 2) and contribute to agent tension (stage 7).

### Scope Model: Universal, Domain, Project, Session, Agent

Every node carries an `Origin`:

| Origin field | Meaning | Query role |
|:-------------|:--------|:-----------|
| `agent_id` | Which agent or persona produced the node | Identity prior, provenance, disagreement analysis |
| `session_id` | Which run or interaction produced the node | Episodic reconstruction and temporal grouping |
| `project_id: Some(path)` | Scoped memory path such as `work/company-a` or `personal-projects/anamnesis` | Domain/project/workspace recall weighting |
| `project_id: None` | Universal memory | Available across scopes with lower but persistent scope weight |
| `confidence` | Source confidence at creation | Used by consumers and future ranking/reinforcement policies |

Conceptually, `project_id` is a hierarchical scope path:

```text
universal
  -> domain/category        (work, personal, personal-projects)
    -> project/workspace    (company-a, anamnesis)
      -> session
        -> event/fragment
```

Scope weighting prevents unrelated domains from contaminating recall while still allowing same-domain habits and universal principles to participate. Same-project nodes receive the strongest weight, same-domain nodes receive a medium boost, universal nodes remain broadly available, and other-domain nodes are downweighted unless entity overlap or explicit consumer filters justify them.

### Identity as High-Mass Memory

Persona is represented by identity nodes, not by a separate prompt string. Identity nodes create a high-mass prior that biases recall and packaging:

| Identity type | Role | Dynamics |
|:--------------|:-----|:---------|
| `IdentityCore` | Stable traits and operating principles | No decay; fixed high salience |
| `IdentityLearned` | Experience-formed preferences and habits | Very slow decay; reinforced by repeated success |
| `IdentityState` | Current task, local focus, temporary stance | Normal decay; project/session-sensitive |

This lets the engine retrieve not only “what happened,” but “what matters for this agent, in this domain, project, and session right now.”

### Promotion Across Scope Levels

Scoped memories may move upward only through additive consolidation. The source nodes remain intact; a broader node is created and linked back to supporting evidence with `ConsolidatedFrom` edges. Promotion may be session → project, project → domain, or domain → universal.

Promotion is appropriate when a pattern has:

- support from multiple projects, domains, or sessions,
- high average confidence,
- low contradiction or exception rate,
- sustained salience after repeated use,
- an abstraction that removes project-specific names, paths, and tools.

Example:

```text
Scoped memories:
  “This Rust crate requires checking public API before refactors.”
  “This Python service breaks if refactors ignore existing tests.”
  “This frontend repo relies on established test fixtures.”

Broader crystallization:
  “Before refactoring unfamiliar code, inspect public boundaries and existing tests first.”
```

This is a crystallization step, not a destructive merge. Exceptions should stay attached as scoped `Gotcha`, `Contradicts`, or `RejectedAlternative` nodes so broader recall can surface caveats.

### EdgeType (12 variants) with Propagation Multipliers (κ)

| Edge Type | κ (forward) | κ (reverse) | Purpose |
|:----------|:-----------|:-----------|:--------|
| `Reason` | 1.15 | 1.15 | Decision rationale |
| `Supersedes` | 1.20 | 0.40 | Replaces outdated knowledge (asymmetric) |
| `ReinforcedBy` | 1.10 | 1.10 | Repeated confirmation |
| `Semantic` | 1.00 | 1.00 | Conceptual relationship |
| `Causal` | 1.00 | 1.00 | Cause-effect |
| `ConsolidatedFrom` | 1.00 | 1.00 | Derived from multiple fragments |
| `ExtractedFrom` | 1.00 | 1.00 | Derived knowledge → source episode |
| `Entity` | 0.95 | 0.95 | Cross-agent shared entity |
| `Temporal` | 0.85 | 0.85 | Temporal sequence |
| `RejectedAlternative` | 0.60 | 0.60 | Considered and discarded option |
| `Contradicts` | 0.00 | 0.00 | Inhibitory — excluded from propagation, triggers repulsion |
| `Custom(String)` | 1.00 | 1.00 | Consumer-defined |

Only `Supersedes` has asymmetric kappa (new knowledge → old gets 1.20; old → new gets 0.40).

## Key Equations

| Eq | Module | Formula | Purpose |
|:---|:-------|:--------|:--------|
| (1) | gravity | `m = 0.55s + 0.30c + 0.15μ` | Node mass (salience + access + type prior) |
| (2) | attraction | `σ = cosine(e_i, e_j)` | Embedding similarity |
| (3) | attraction | `A = σ × τ × (1 + 0.20m)` | Attraction score |
| (4) | forgetting | `s(t+dt) = b + (s-b)·exp(-λ·dt)` | Exponential decay with floor |
| (5) | forgetting | `s ← s + 0.20·(1-s)` | Reinforcement on access |
| (6) | gravity | `boost = 1 + 0.20·m` | Gravity boost in spreading activation |
| (7) | repulsion | `H = Σ w·ρ·X` | Repulsion accumulation |
| (8) | repulsion | `X' = X·exp(-1.5·H)` | Activation damping |
| (9) | identity | `I(a) = max[π·σ]` over top-3 | Identity prior for initial activation |
| (10) | activation | `y⁰ = clamp(0.60·seed + 0.30·vsim + 0.10·I, 0, 1)` | Initial activation (eq 10) |
| (11) | activation | `y_j = y_i·w·κ·δ·ψ(s)·(1+0.20m)` | Spreading propagation (eq 11) |
| (13) | scoring | `R = (0.50X' + 0.20q + 0.15s + 0.15m)·scope_w` | Final relevance score |
| (14) | assembly | `T = Σ ρ·Σ w·X` | Agent tension |
| (15) | crystallize | `s_c = 0.60·s̄ + 0.25·conf + 0.15·β` | Crystallization initial salience (ADR-005, planned) |

## Design Principles

- **Zero external dependencies** for core library (`std` only)
- **Pure functions** for all mechanics — testable, benchmarkable, no side effects
- **Pluggable storage** via `StorageAdapter` trait — static dispatch, no boxing overhead
- **No async** in core — consumers wrap with async if needed
- **No LLM calls** — engine provides graph primitives; extraction is the consumer's responsibility
- **No global state** — all state in `Engine` instances
- **Salience as shared signal** — all mechanics read/write salience; memory tiers emerge naturally from salience ranges
- **Fragments over summaries** — original content preserved; consolidation is emergent via `ConsolidatedFrom` edges
- **Indexes trigger; graph remembers** — keyword, BM25, embedding, and temporal indexes find entry points, while graph nodes and edges remain the source of truth

## Boundaries

**In Scope:**
- Graph storage and traversal
- Cognitive dynamics (scoring, decay, attraction, gating)
- Query engine (spreading activation, ContextPackage assembly)
- Pluggable storage adapters
- Origin attribution and scoped knowledge (session/project/domain/universal)
- Contradiction detection and agent tension measurement
- Multi-resolution content (L0/L1/L2 token budget)

**Out of Scope:**
- LLM calls (embedding generation, knowledge extraction)
- Session management
- Network/HTTP server
- Serialization format opinions (consumer decides)

## Implementation Status

> v0.3.0 — Cognitive engine operational.

### Implemented ✅

**Graph Layer:**
- Node/Edge CRUD with arena-based `InMemoryStorage` (`Vec<Option<Node>>` + SoA hot fields + adjacency index)
- Generic `Graph<S: StorageAdapter = InMemoryStorage>` with static dispatch
- 12 KnowledgeType variants (Identity Star / Knowledge Planet / Memory Dust)
- 12 EdgeType variants with directional κ multipliers
- Node schema: name (L0), summary (L1), content (L2), embedding, origin, entity_tags, valid_from/until

**Mechanics (all pure functions, wired into Engine):**
- Forgetting: lazy decay in `touch()` (eq 4), batch decay in `tick()`, reinforcement (eq 5)
- Attraction: cosine similarity + type affinity + auto-linking in `ingest()` (eqs 2, 3)
- Gravity: mass computation (eq 1) + gravity boost (eq 6)
- Perception: novelty/confidence/budget gating in `ingest()`
- Repulsion: contradiction damping in `query()` (eqs 7, 8)

**Query Pipeline (Associative mode — full 7-stage pipeline):**
- Initial activation: seed + vector similarity + identity prior (eqs 9, 10)
- Spreading activation: priority-queue BFS, depth-aware, cycle-safe (eq 11)
- Repulsion: Contradicts edge damping (eqs 7-8, stage 4)
- Final scoring: activation + vector sim + salience + mass, scope-weighted (eq 13)
- Agent tension: identity contradiction measurement (eq 14)
- ContextPackage assembly: identity/knowledge/memories/tensions with L0/L1/L2 token budget

**Scoping:**
- `Origin.project_id`: hierarchical scope path for project/domain/universal knowledge scoping
- Query-time scope weights: same project strongest, same domain medium, universal broad, other domain low + entity overlap bonus

### Deprecated / compatibility APIs

| Engine Method | Current Behavior | Planned |
|:-------------|:-----------------|:--------|
| `merge_candidates()` | Deprecated | Use `EngineConfig::dedup_threshold` in `ingest()` |
| `auto_merge()` | Deprecated | Use dedup/reinforcement during ingest |

### Planned 🔜

- **Search v2 diagnostics**: Raw candidate trace, trigger recall metrics, graph reconstruction metrics, and packaging loss analysis
- **Hybrid trigger fusion**: Preserve lexical/vector/entity/temporal candidate ranks before graph recall
- **Social reinforcement**: Multi-agent salience bonus — logarithmic boost for nodes independently confirmed by multiple agents
- **Cognitive engine benchmarks**: CRUD/storage benchmarks exist; spreading activation and query pipeline benchmarks needed
- **SQLite storage adapter**: Persist graph across processes

## Direction

The next evolution targets a more ergonomic API and deeper automation:

- **Unified `search()` method**: Single entry point accepting text, optional embeddings, and scope/time filters. Search should preserve raw candidates, fuse trigger ranks, then run graph recall before packaging.
- **`crystallize()`**: Re-ingest insights derived from query results back into the graph with provenance links. Salience initialized from source node average (eq 15). Enables the graph to grow from its own reasoning, not just external observations.
- **Hybrid trigger integration**: Keyword/BM25-style lexical search plus optional vector similarity should feed seed discovery. Scores from trigger indexes should be traceable, but not treated as memory truth.
- **Three-equal structure**: Principled token budget allocation across identity / knowledge / memories in `ContextPackage`. Current assembly prioritizes knowledge fragments; a balanced three-way split respects all partitions.
- **Non-Associative query modes**: TypeFiltered for "all conventions", Neighborhood for "everything about auth module", Temporal for "what changed recently?", List for "what do I know?" at session start.

## Remaining Architecture Notes

- **Cosine similarity location**: Identical implementation in `mechanics/attraction.rs` and inlined in `api/mod.rs` — consolidation into a shared utility is a minor cleanup item
- **Bridge node protection**: Bridge nodes (whose removal disconnects parts of the graph) are not yet detected or protected from decay — a planned extension of the gravity mechanic
- **Convergence-based termination**: Spreading activation uses fixed exit conditions (budget, min_activation, max_hops); convergence detection is a future optimization
