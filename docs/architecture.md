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

- **Edge**: Represents relationships between nodes with:
  - Source and target node IDs
  - Edge type (semantic, causal, temporal, etc.)
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
}
```

## Data Flow

1. **Ingestion**: Observation → Perception gate → Node creation/update → Attraction scoring
2. **Linking**: Manual or automatic edge creation based on similarity/gravity
3. **Decay**: Periodic `tick()` applies forgetting mechanics to all nodes
4. **Query**: Spreading activation from seed nodes returns relevant subgraph
5. **Reinforcement**: `touch()` strengthens accessed nodes

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
