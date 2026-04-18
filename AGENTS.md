# PROJECT KNOWLEDGE BASE

**Project:** Anamnesis
**Language:** Rust (2024 edition)
**Purpose:** Cognitive dynamics engine for LLMs — graph-structured knowledge with attraction, gravity, perception, and forgetting

## OVERVIEW

Anamnesis is a standalone Rust library that provides a cognitive dynamics engine for LLM-based agents. It models knowledge as a graph with dynamics that mimic cognitive processes:

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
- **Builder pattern for configuration** — EngineConfig, QueryConfig
- **No async in core** — synchronous API, async wrapper can be added externally
- **Salience as shared signal** — all mechanics read/write salience; memory tiers emerge naturally from salience ranges
- **Fragments over summaries** — preserve individual conversation turns as nodes; summaries are emergent via consolidation, not lossy rewrites
- **Origin on every node** — multi-agent graphs track which agent produced each fragment (`agent_id`, `session_id`, `project_id`, `confidence`)

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

> Methods marked ⬚ are defined but currently return placeholder results.

```rust
/// The Anamnesis cognitive dynamics engine.
///
/// Generic over storage backend. Default: InMemoryStorage (arena-based, sub-millisecond access).
pub struct Engine<S: StorageAdapter = InMemoryStorage> {
    graph: Graph<S>,
    config: EngineConfig,
}

impl Engine<InMemoryStorage> {
    /// Create a new engine with default configuration and in-memory storage.
    pub fn new() -> Self;

    /// Create a new engine with custom configuration and in-memory storage.
    pub fn with_config(config: EngineConfig) -> Self;
}

impl<S: StorageAdapter> Engine<S> {
    /// Create an engine with a custom storage backend.
    pub fn with_storage(config: EngineConfig, storage: S) -> Self;

    /// Ingest a new observation — applies perception gating and attraction auto-linking.
    /// Returns the IDs of created nodes (typically one).
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error>;

    /// Create a link between two nodes.
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType, weight: f64) -> Result<EdgeId, Error>;

    /// Touch a node — apply lazy decay then reinforce on access.
    /// Ordering invariant: decay (eq 4) BEFORE reinforcement (eq 5).
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error>;

    /// Advance time — apply batch decay (eq 4) to all nodes, returns TickReport.
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    /// Query the graph — returns structured ContextPackage for LLM consumption.
    /// Associative mode: full spreading activation pipeline (eqs 9-14).
    /// Other modes (TypeFiltered, Neighborhood, Temporal, List): return empty ContextPackage.
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error>;

    /// ⬚ Get merge candidates above similarity threshold (currently returns empty).
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<MergePair>, Error>;

    /// ⬚ Execute auto-merge with undo log (currently returns empty log).
    pub fn auto_merge(&mut self, threshold: f64) -> Result<MergeLog, Error>;

    /// ⬚ Cross-agent entity linking after parallel execution round.
    /// Creates Entity edges between nodes from different agents sharing entity tags.
    /// No LLM calls — metadata matching only. Currently returns empty report.
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;
}
```

## What Anamnesis Does NOT Do (Boundary)

- ❌ LLM calls (no embedding generation, no extraction)
- ❌ Session management
- ❌ Network/HTTP server
- ❌ Serialization format opinions (consumer decides)
- ✅ Graph storage and traversal
- ✅ Cognitive dynamics (scoring, decay, attraction, gating)
- ✅ Query engine (spreading activation, subgraph extraction)
- ✅ Pluggable storage adapters
- ✅ Origin attribution (tracks agent provenance per node)
- ✅ Scoped knowledge (session/project/universal via Origin.project_id)
- ✅ Structured query output (ContextPackage with identity/knowledge/memories/tensions)
- ✅ Multi-resolution content (L0 name / L1 summary / L2 full content)
- ✅ Identity management (agent personas as graph nodes with dynamics)
- ✅ Episodic preservation (original text + extracted knowledge linked)
- ✅ Contradiction detection (Contradicts edges surfaced during queries)
- ✅ Multiple query modes (associative implemented; type-filtered, neighborhood, temporal, list planned)
- ⬚ Cross-agent entity linking via `reflect_batch()` (placeholder)
- ⬚ Node consolidation via `merge_candidates()` / `auto_merge()` (placeholder)

## Key Types

```rust
/// Knowledge type taxonomy — determines decay rate, mass prior, and dynamics behavior.
///
/// Three classes:
/// - Identity (Star): high mass, low/no decay
/// - Knowledge (Planet): medium mass, moderate decay
/// - Memory (Dust): low mass, fast decay
enum KnowledgeType {
    // Identity (Star — high mass, low/no decay)
    IdentityCore,    // L0: Immutable core trait, no decay ("I am a code architect")
    IdentityLearned, // L1: Experience-formed trait, very slow decay ("prefers factory pattern")
    IdentityState,   // L2: Current state, normal decay ("refactoring auth module")

    // Knowledge (Planet — medium mass, moderate decay)
    Semantic,        // Extracted fact from conversation or document
    Procedural,      // How-to or execution pattern
    Entity,          // Named concept, module, person, or service
    Convention,      // Project rule or convention
    Decision,        // Decision with rationale
    Gotcha,          // Pitfall or warning

    // Memory (Dust — low mass, fast decay)
    Episodic,        // Raw conversation turn or session text
    Event,           // Time-bound occurrence

    Custom(String),  // Consumer-defined type
}

/// Edge type — determines propagation multiplier (kappa) during spreading activation.
/// Supportive edges propagate activation; Contradicts is inhibitory (applies repulsion).
enum EdgeType {
    // Supportive (kappa > 0)
    Semantic,            // Conceptual relationship. kappa = 1.00
    Causal,              // Cause-effect relationship. kappa = 1.00
    Temporal,            // Temporal sequence. kappa = 0.85
    Reason,              // Decision rationale. kappa = 1.15
    ReinforcedBy,        // Repeated confirmation. kappa = 1.10
    ConsolidatedFrom,    // Derived from multiple fragments. kappa = 1.00
    ExtractedFrom,       // Derived knowledge to source episode. kappa = 1.00
    Entity,              // Shared entity link across agents. kappa = 0.95
    Supersedes,          // Replaces outdated knowledge. kappa = 1.20 (forward) / 0.40 (backward)
    RejectedAlternative, // Considered and discarded option. kappa = 0.60

    // Inhibitory
    Contradicts,         // Conflicting assertions. Excluded from propagation; applies repulsion.

    Custom(String),      // Consumer-defined edge type
}

/// Tracks which agent produced a knowledge fragment.
struct Origin {
    agent_id: String,
    session_id: String,
    project_id: Option<String>, // None = universal; Some = project-scoped
    confidence: f64,
}

/// Input to reflect_batch().
struct SessionSummary {
    agent_id: String,
    session_id: String,
    node_ids: Vec<NodeId>,
}

/// Query modes for different retrieval patterns.
enum Query {
    Associative { seed: NodeId, budget: usize },             // ✅ full pipeline implemented
    TypeFiltered { node_type: KnowledgeType, limit: usize }, // ⬚ returns empty
    Neighborhood { entity: NodeId, depth: usize },            // ⬚ returns empty
    Temporal { since: Timestamp, node_types: Option<Vec<KnowledgeType>>, limit: usize }, // ⬚ returns empty
    List { min_salience: f64, limit: usize },                 // ⬚ returns empty
}
```

## StorageAdapter Interface

21 methods across five groups:

```rust
pub trait StorageAdapter: Send + Sync {
    // ID allocation (reuses freed IDs)
    fn next_node_id(&mut self) -> NodeId;
    fn next_edge_id(&mut self) -> EdgeId;

    // Node CRUD
    fn set_node(&mut self, node: Node) -> Result<(), Error>;
    fn get_node(&self, id: NodeId) -> Result<&Node, Error>;
    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error>; // non-hot fields only
    fn delete_node(&mut self, id: NodeId) -> Result<(), Error>;

    // Edge CRUD
    fn set_edge(&mut self, edge: Edge) -> Result<(), Error>;
    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error>;
    fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error>; // weight/metadata only
    fn delete_edge(&mut self, id: EdgeId) -> Result<(), Error>;

    // Adjacency index (O(degree))
    fn edges_from(&self, id: NodeId) -> &[EdgeId];
    fn edges_to(&self, id: NodeId) -> &[EdgeId];

    // Hot fields — SoA arrays, cache-friendly for dynamics iteration
    fn get_salience(&self, id: NodeId) -> Result<f64, Error>;
    fn set_salience(&mut self, id: NodeId, salience: f64) -> Result<(), Error>;
    fn get_accessed_at(&self, id: NodeId) -> Result<Timestamp, Error>;
    fn set_accessed_at(&mut self, id: NodeId, ts: Timestamp) -> Result<(), Error>;
    fn get_node_type(&self, id: NodeId) -> Result<&KnowledgeType, Error>;

    // Counts and iteration
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
    fn all_node_ids(&self) -> Vec<NodeId>;
    fn all_edge_ids(&self) -> Vec<EdgeId>;
}
```

Ships with `InMemoryStorage` (arena-based Vec, adjacency index, ID recycling). Implement the trait for SQLite, PostgreSQL, Neo4j, or any other backend.

## Direction

Planned features for future releases — none of these are implemented yet:

- **Non-Associative query modes** (`TypeFiltered`, `Neighborhood`, `Temporal`, `List`) — currently return empty `ContextPackage`
- **`search(query: &str, limit: usize) -> Result<Vec<NodeId>, Error>`** — unified text + salience search combining FTS and graph traversal
- **`crystallize(session_id: &str) -> Result<Vec<NodeId>, Error>`** — post-session consolidation: detect patterns, create `ConsolidatedFrom` edges, promote salience on repeated fragments
- **`text_search(query: &str) -> Result<Vec<NodeId>, Error>`** on `StorageAdapter` — optional full-text search capability for storage backends that support it (e.g., SQLite FTS5)
- **`merge_candidates()` / `auto_merge()`** — attraction-based near-duplicate detection and merge with undo log
- **`reflect_batch()`** — cross-agent entity linking via entity tag matching, creating `Entity` edges between nodes from different agents sharing the same concepts
- **SQLite storage adapter** — persistent storage with FTS5 support
- **Social reinforcement scoring** — multi-agent salience bonus when multiple agents independently observe the same fragment
