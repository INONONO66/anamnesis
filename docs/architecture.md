# Architecture Overview

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

- **Edge**: Represents relationships between nodes with:
  - Source and target node IDs
  - Edge type (semantic, causal, temporal, entity, reasoning types, etc.)
  - Weight (strength of relationship)
  - Metadata

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

## Data Flow

1. **Ingestion**: Observation → Perception gate → Node creation/update → Attraction scoring
2. **Linking**: Manual or automatic edge creation based on similarity/gravity
3. **Decay**: Periodic `tick()` applies forgetting mechanics to all nodes
4. **Query**: Spreading activation from seed nodes returns relevant subgraph
5. **Reinforcement**: `touch()` strengthens accessed nodes
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
