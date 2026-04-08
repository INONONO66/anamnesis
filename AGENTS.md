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
- **Salience as shared signal** — all mechanics read/write salience; memory tiers emerge naturally from salience ranges
- **Fragments over summaries** — preserve individual conversation turns as nodes; summaries are emergent via consolidation, not lossy rewrites
- **Origin on every node** (planned) — multi-agent graphs require tracking which agent produced each fragment (`agent_id`, `session_id`, `confidence`)

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

## Core API Design (Target)

> Methods marked ⬚ are defined but currently return placeholder results. See architecture.md for current state.

```rust
/// The public API surface of Anamnesis
pub struct Engine {
    graph: Graph,
    config: EngineConfig,
}

impl Engine {
    /// Create a new engine with configuration
    pub fn new(config: EngineConfig) -> Self;

    /// Ingest a new observation into the graph (⬚ perception gating not yet applied)
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error>;

    /// Create/strengthen a link between two nodes
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType, weight: f64) -> Result<EdgeId, Error>;

    /// ⬚ Advance time — apply decay to all nodes (currently no-op)
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    /// ⬚ Query the graph — find relevant subgraph from seed (currently returns seed only)
    pub fn query(&self, seed: &Query, budget: usize) -> Result<SubGraph, Error>;

    /// Touch a node — reinforce on access (memory strengthening)
    pub fn touch(&mut self, node_id: NodeId) -> Result<(), Error>;

    /// ⬚ Get merge candidates (currently returns empty)
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<MergePair>, Error>;

    /// ⬚ Execute auto-merge with undo log (currently returns 0)
    pub fn auto_merge(&mut self, threshold: f64) -> Result<MergeLog, Error>;

    /// (Planned) Cross-agent entity linking after parallel execution round
    /// Creates Entity edges between nodes from different agents that share entities
    /// No LLM calls — metadata matching only
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;
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
- ✅ Origin attribution (planned — tracks agent provenance per node)
- ✅ Social reinforcement scoring (planned — multi-agent salience bonus)
- ✅ Cross-agent entity linking via `reflect_batch()` (planned)
- ✅ Identity management (agent personas as graph nodes with physics)
- ✅ Episodic preservation (original text + extracted knowledge linked)
- ✅ Contradiction detection (Contradicts edges surfaced during queries)
- ✅ Pure graph algorithms (clustering, bridge detection, entity matching)
- ✅ Multiple query modes (associative, type-filtered, neighborhood, temporal, list)

## Key Types (Planned)

```rust
/// Knowledge type taxonomy
enum KnowledgeType {
    Episodic,        // Raw conversation turn / session text
    Semantic,        // Extracted fact
    Procedural,      // Agent execution pattern / how-to
    Entity,          // Named concept (module, person, service)
    Event,           // Time-bound occurrence
    IdentityCore,    // L0: Immutable agent trait (no decay)
    IdentityLearned, // L1: Experience-formed trait (slow decay)
    IdentityState,   // L2: Current state (normal decay)
    Custom(String),
}

/// Tracks which agent produced a knowledge fragment
struct Origin {
    agent_id: String,
    session_id: String,
    confidence: f64,
}

/// Input to reflect_batch()
struct SessionSummary {
    agent_id: String,
    session_id: String,
    node_ids: Vec<NodeId>,
}

/// Additional edge types for reasoning and conflict
enum EdgeType {
    // existing
    Semantic, Causal, Temporal, Custom(String),
    // planned: reasoning preservation
    Reason,               // why a decision was made
    RejectedAlternative,  // option considered and discarded
    Supersedes,           // replaces outdated knowledge
    ReinforcedBy,         // confirmed by repeated experience
    ConsolidatedFrom,     // derived from multiple fragments
    // planned: cross-agent and structural
    Entity,               // cross-agent shared entity link
    ExtractedFrom,        // derived knowledge → source episode
    Contradicts,          // conflicting assertions (repulsion in activation)
}

/// Query modes for different retrieval patterns
enum Query {
    Associative { seed: NodeId, budget: usize },
    TypeFiltered { node_type: KnowledgeType, limit: usize },
    Neighborhood { entity: NodeId, depth: usize },
    Temporal { since: Timestamp, node_types: Option<Vec<KnowledgeType>>, limit: usize },
    List { min_salience: f64, limit: usize },
}
```
