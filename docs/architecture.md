# Architecture Overview

> **Implementation status:** The module structure and types described below exist in code. However, the Engine does not yet wire the mechanics and query modules into its API methods. See [Current State](#current-state) at the bottom for specifics.

## High-Level Design

Anamnesis is a cognitive graph engine that models knowledge as a directed graph with physics-like dynamics. The architecture consists of four main layers:

### 1. Graph Layer (`src/graph/`)

Core data structures representing the knowledge graph:

- **Node**: Represents a concept or entity with:
  - Unique ID
  - Content/embedding representation
  - Salience (importance/recency score)
  - Timestamp of creation and last access
  - Metadata (type, tags, etc.)
  - **Origin** (planned): agent_id, session_id, confidence — tracks which agent produced this knowledge and with what certainty
  - **Embedding** (planned): Vector representation for similarity-based operations (attraction, perception). Currently only in Observation, needs to persist on Node.
  - **KnowledgeType** (planned): Enum distinguishing Episodic, Semantic, Procedural, Entity, Identity (L0/L1/L2), Event — enables type-filtered queries and differential decay rates
  - **Source reference** (planned): Optional link to the episodic node this knowledge was extracted from — enables provenance tracing
  - **Entity tags** (planned): List of entity identifiers for automatic tag-based linking
  - **Temporal validity** (planned): `valid_at` / `invalid_at` timestamps for facts that have a time-bounded truth (e.g., "Alice worked at Acme until 2024")

- **Edge**: Represents relationships between nodes with:
  - Source and target node IDs
  - Edge type: Semantic, Causal, Temporal, Custom(String)
  - Planned edge types:
    - `Reason` — why a decision was made
    - `RejectedAlternative` — option considered and discarded
    - `Supersedes` — replaces outdated knowledge
    - `ReinforcedBy` — confirmed by repeated experience
    - `ConsolidatedFrom` — derived from multiple fragments
    - `Entity` — cross-agent shared entity link
    - `ExtractedFrom` — links derived knowledge to source episode
    - `Contradicts` — marks conflicting assertions (enables repulsion in spreading activation)
  - Weight (strength of relationship)
  - Metadata
  - **Fact sentence** (planned): Human-readable description of the relationship (e.g., "auth module depends on session service") — makes edges searchable
  - **Temporal validity** (planned): `valid_at` / `invalid_at` for time-bounded relationships
  - **Source episodes** (planned): List of episodic nodes that evidenced this relationship

- **Graph**: Container managing nodes and edges with efficient lookup and traversal

### 2. Mechanics Layer (`src/mechanics/`)

Implements cognitive dynamics as pure scoring and propagation functions:

- **Attraction**: Similarity-based clustering
  - Computes affinity between nodes based on embedding distance
  - Identifies merge candidates (duplicate/near-duplicate nodes)
- **Gravity**: Centrality-based importance
  - PageRank-like algorithm to identify hub nodes
  - Influences which nodes attract new knowledge
  - Social reinforcement bonus (planned): logarithmic boost for nodes independently confirmed by multiple distinct agents
- **Perception**: Input gating
  - Novelty scoring (how different from existing knowledge)
  - Confidence filtering (only high-confidence observations enter)
  - Budget constraints (limit graph growth)
- **Forgetting**: Temporal decay
  - Salience decay over time (exponential or polynomial)
  - Reinforcement on access (touching a node strengthens it)
  - Pruning of low-salience nodes

### 3. Query Layer (`src/query/`)

Implements graph traversal and subgraph extraction:

- **Spreading Activation**: k-hop traversal from seed nodes
  - Activation spreads through edges with decay
  - Returns relevant subgraph within budget
- **Subgraph Extraction**: Extracts connected components or neighborhoods

### 4. Storage Layer (`src/storage/`)

Abstracts storage backend behind a trait:

- **StorageAdapter**: Interface for node/edge persistence
  - `get_node()`, `set_node()`, `delete_node()`
  - `get_edge()`, `set_edge()`, `delete_edge()`
  - `list_nodes()`, `list_edges()`
- **In-Memory Implementation**: Default implementation using HashMap
- **Future Implementations**: SQLite, PostgreSQL, Neo4j adapters

### 5. API Layer (`src/api/`)

Public interface for consumers:

```rust
pub struct Engine {
    graph: Graph,
    config: EngineConfig,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self;
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error>;
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType, weight: f64) -> Result<EdgeId, Error>;
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;
    pub fn query(&self, seed: &Query, budget: usize) -> Result<SubGraph, Error>;
    pub fn touch(&mut self, node_id: NodeId) -> Result<(), Error>;
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<MergePair>, Error>;
    pub fn auto_merge(&mut self, threshold: f64) -> Result<MergeLog, Error>;

    // Planned: Multi-agent support
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;
}
```

### 6. Query Modes (Planned)

Spreading activation from a seed node covers ~40% of real agent query patterns. Five query modes are needed:

| Mode | Use Case | Mechanism |
|:--|:--|:--|
| **Associative** | "What's related to X?" | Spreading activation from seed (existing) |
| **TypeFiltered** | "All conventions" or "all gotchas" | Filter by KnowledgeType, order by salience |
| **Neighborhood** | "Everything about auth module" | Entity node + k-hop subgraph |
| **Temporal** | "What changed recently?" | Filter by timestamp, optional type filter |
| **List** | "What do I know?" (session start) | All nodes above salience threshold, ordered |

These modes correspond to real query patterns observed in production agent systems (CrewAI's `recall(categories=...)`, Zep's entity graph search, mem0's filtered search).

### 7. Pure Graph Algorithms (Planned)

Graph-structural operations that require no LLM, implementable with zero external dependencies:

| Algorithm | Complexity | Incremental? | Purpose |
|:--|:--|:--|:--|
| Union-Find (connected components) | O(α(n)) ≈ O(1) | Yes | Detect disconnected knowledge islands |
| Bridge node detection | O(V+E) | Periodic | Protect critical connector nodes from decay |
| Degree centrality | O(1) per update | Yes | Identify hub nodes |
| Label Propagation (clustering) | O(E) per iteration | Yes | Identify knowledge clusters |
| Metadata entity matching | O(1) hash lookup | Yes | Auto-link nodes sharing entity tags |

Bridge nodes — nodes whose removal would disconnect parts of the graph — receive decay protection. This prevents the forgetting mechanic from destroying critical knowledge connections.

## Data Flow (Target)

> Steps marked ⬚ are not yet wired in the Engine. The mechanics modules exist as standalone pure functions.

1. **Ingestion**: Observation → ⬚ Perception gate → Node creation → Graph
2. **Linking**: Manual edge creation (automatic similarity-based linking is ⬚ planned)
3. **Decay**: ⬚ Periodic `tick()` applies forgetting mechanics to all nodes
4. **Query**: ⬚ Spreading activation from seed nodes returns relevant subgraph
5. **Reinforcement**: `touch()` strengthens accessed nodes ✅
6. **Batch Reflect** (planned): After parallel agent execution, `reflect_batch()` creates cross-agent `Entity` edges by matching shared entities across sessions — no LLM calls, metadata matching only

## Design Principles

- **Zero external dependencies** for core library
- **Pure functions** for all mechanics (testable, benchmarkable)
- **Pluggable storage** enables multiple backends
- **No async** in core (consumers can wrap with async)
- **No LLM integration** in engine (consumer's responsibility)
- **No global state** (all state in Engine instance)

## Boundaries

**In Scope:**

- Graph storage and traversal
- Cognitive mechanics (scoring, decay, attraction, gating)
- Query engine (spreading activation, subgraph extraction)
- Pluggable storage adapters

**Out of Scope:**

- LLM calls (embedding generation, knowledge extraction)
- Session management
- Network/HTTP server
- Serialization format opinions

## Current State

> Last updated after Phase 2 completion. The cognitive engine is functional for Associative queries.

### What Works

**Graph Layer:**
- Node/Edge CRUD with arena-based `InMemoryStorage` (Vec + SoA hot fields + adjacency index)
- Generic `Graph<S: StorageAdapter = InMemoryStorage>` with static dispatch
- 12 KnowledgeType variants (Identity Star / Knowledge Planet / Memory Dust)
- 12 EdgeType variants with directional kappa multipliers
- Node schema: name (L0), summary (L1), content (L2), embedding, origin, entity_tags, valid_from/until

**Mechanics (all pure functions, wired into Engine):**
- Forgetting: lazy decay in `touch()` (eq 4), batch decay in `tick()`, reinforcement (eq 5)
- Attraction: cosine similarity + type affinity + auto-linking in `ingest()` (eqs 2, 3)
- Gravity: mass computation (eq 1) + gravity boost (eq 6)
- Perception: novelty/confidence/budget gating in `ingest()`
- Repulsion: contradiction damping in `query()` (eqs 7, 8)

**Query Pipeline (Associative mode -- full pipeline):**
- Initial activation: seed + vector similarity + identity prior (eqs 9, 10)
- Spreading activation: priority-queue BFS, depth-aware, cycle-safe (eq 11)
- Repulsion: Contradicts edge damping (eq 12)
- Final scoring: activation + vector + salience + mass, scope-weighted (eq 13)
- Agent tension: identity contradiction measurement (eq 14)
- ContextPackage assembly: identity/knowledge/memories/tensions with L0/L1/L2 token budget

**Scoping:**
- Origin.project_id: session/project/universal knowledge scoping
- Query-time scope weights: same project 1.0, universal 0.85, other project 0.30 + entity overlap bonus

### Placeholder (Phase 3)

| Engine Method | Current Behavior | Planned |
|:-------------|:-----------------|:--------|
| `query()` -- non-Associative modes | Returns `ContextPackage::empty()` | TypeFiltered, Neighborhood, Temporal, List |
| `merge_candidates()` | Returns empty Vec | Attraction-based candidate detection |
| `auto_merge()` | Returns empty MergeLog | Merge with undo log |
| `reflect_batch()` | Returns empty ReflectReport | Cross-agent entity linking |

### Remaining Architecture Items

- **Non-Associative query modes**: TypeFiltered, Neighborhood, Temporal, List are defined in the Query enum but return empty results
- **Cosine similarity location**: Identical implementation exists in `mechanics/attraction.rs` -- consider deduplication if perception also needs it
- **Cognitive engine benchmarks**: CRUD/storage benchmarks exist; spreading activation, decay, and query pipeline benchmarks are needed
- **Social reinforcement**: Multi-agent salience bonus not yet implemented
- **Convergence-based termination**: Spreading activation uses fixed conditions (budget, min_activation, diminishing returns); convergence detection is a future optimization
