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

### What Works

- **Graph CRUD**: Node/Edge creation, lookup, iteration
- **In-memory storage**: `InMemoryStorage` implements `StorageAdapter`
- **Engine scaffolding**: `ingest()`, `link()`, `touch()` perform basic operations
- **Standalone mechanics**: `Attraction`, `Gravity`, `Perception`, `Forgetting` modules contain pure scoring functions with unit tests
- **Standalone query**: `SpreadingActivation` module contains BFS-based activation and k-hop traversal

### What's Scaffolded but Not Wired

| Engine Method | Current Behavior | Intended Behavior |
|:---|:---|:---|
| `ingest()` | Stores node directly, ignores Perception | Apply perception gating before storage |
| `tick()` | No-op | Apply `Forgetting::apply_decay()` to all nodes |
| `query()` | Returns `vec![seed]` | Run `SpreadingActivation::activate()` with budget |
| `merge_candidates()` | Returns empty Vec | Use `Attraction::find_merge_candidates()` |
| `auto_merge()` | Returns 0 | Merge candidates with undo log |

### Known Architecture Issues

- **Dual storage**: `Engine` stores nodes in both `Graph` (in-memory HashMap) and `StorageAdapter` (also in-memory by default). Source of truth is ambiguous. This needs resolution before adding complexity.
- **Embeddings not persisted on Node**: `Observation.embedding` is consumed during `ingest()` but not stored on the `Node`. Attraction and perception mechanics require embeddings to be available on nodes for post-ingestion operations.
- **Linear edge scan**: `Graph::edges_from()` / `edges_to()` iterate all edges. This is adequate for small graphs but will not scale.
- **Cosine similarity duplication**: Identical implementation exists in both `attraction.rs` and `perception.rs`.
- **Single query mode**: Only spreading activation is designed. TypeFiltered, Neighborhood, Temporal, and List query modes are needed for real agent usage patterns.
- **No episodic preservation**: Observation embedding is discarded after ingestion. No `source_ref` to link extracted facts to original episodes.

---

## Multi-Agent Support (Planned)

When multiple agents share the same Anamnesis graph, three features extend the single-agent model:

### Origin Attribution

Every node carries an `Origin` struct identifying which agent produced it:

```rust
struct Origin {
    agent_id: String,
    session_id: String,
    confidence: f64,
}
```

Origin enables the consumer-side Reflector to resolve contradictions (same agent correcting itself vs. different agents disagreeing) and weight expertise by source.

### Social Reinforcement

Gravity's centrality scoring gains a social bonus when a node is independently reinforced by multiple distinct agents:

```
social_bonus = 1.0 + ln(distinct_agent_count)   // only if > 1 agent
```

This composes with existing decay/reinforcement mechanics. Logarithmic scaling prevents popularity cascades.

### Batch Reflect

A round-boundary operation that links cross-agent knowledge:

```rust
pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;
```

Collects nodes from completed sessions, groups by shared entities, and creates `Entity` edges between nodes from different agents that reference the same concept. No merging, no salience changes — only edge creation for discoverability via spreading activation.

### Dependency

```
Origin → Social Reinforcement (needs agent_id to count distinct agents)
Origin → Batch Reflect (needs agent_id to identify cross-agent nodes)
```

All three features are design-level plans. See [ADR-003](./design-decisions/003-multi-agent-memory.md) for rationale.
