# PROJECT KNOWLEDGE BASE

**Project:** Anamnesis
**Language:** Rust (2024 edition)
**Purpose:** Cognitive graph engine for LLMs — graph-structured knowledge with physics-like dynamics

## OVERVIEW

Anamnesis is a standalone Rust library that provides a cognitive graph engine for LLM-based agents. It models knowledge as a graph with physics-like properties:

- **Attraction**: Similar/related nodes cluster together (embedding similarity + co-occurrence)
- **Gravity**: Important nodes (high centrality) attract new knowledge
- **Perception**: Input gating — filters what enters the graph (novelty, confidence, budget)
- **Forgetting**: Time-based salience decay with reinforcement on access

## STRUCTURE

```
src/
├── graph/       # Core types: Node, Edge, Graph
├── mechanics/   # Cognitive dynamics: attraction, gravity, perception, forgetting
├── query/       # Query engine: spreading activation, subgraph extraction
├── storage/     # StorageAdapter trait + implementations
└── api/         # Public API surface
```

## CONVENTIONS

- **Zero external dependencies for core** — only std library
- **Trait-based storage** — `StorageAdapter` trait, swappable implementations
- **Pure functions for mechanics** — all scoring/decay functions are pure (no side effects)
- **Builder pattern for configuration** — GraphConfig, QueryConfig
- **No async in core** — synchronous API, async wrapper can be added externally

## COMMANDS

```bash
cargo build          # Build
cargo test           # Run tests
cargo doc --open     # Generate docs
cargo bench          # Run benchmarks (when added)
```

## ANTI-PATTERNS

- No `unwrap()` in library code — use `Result<T, E>` everywhere
- No `println!` in library code — use `log` crate if logging needed
- No global state — all state is in Graph/Engine instances

## Core API Design

```rust
/// The public API surface of Anamnesis
pub struct Engine {
    graph: Graph,
    config: EngineConfig,
}

impl Engine {
    /// Create a new engine with configuration
    pub fn new(config: EngineConfig) -> Self;

    /// Ingest a new observation into the graph
    /// Returns created/updated nodes
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error>;

    /// Create/strengthen a link between two nodes
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType, weight: f64) -> Result<EdgeId, Error>;

    /// Advance time — apply decay to all nodes
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    /// Query the graph — find relevant subgraph from seed
    pub fn query(&self, seed: &Query, budget: usize) -> Result<SubGraph, Error>;

    /// Touch a node — reinforce on access (memory strengthening)
    pub fn touch(&mut self, node_id: NodeId) -> Result<(), Error>;

    /// Get merge candidates (similar nodes above threshold)
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<MergePair>, Error>;

    /// Execute auto-merge with undo log
    pub fn auto_merge(&mut self, threshold: f64) -> Result<MergeLog, Error>;
}
```

## What Anamnesis Does NOT Do (Boundary)

- ❌ LLM calls (no embedding generation, no extraction)
- ❌ Session management
- ❌ Network/HTTP server
- ❌ Serialization format opinions (consumer decides)
- ✅ Graph storage and traversal
- ✅ Cognitive mechanics (scoring, decay, attraction, gating)
- ✅ Query engine (spreading activation, subgraph extraction)
- ✅ Pluggable storage adapters
