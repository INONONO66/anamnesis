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
├── embedding/   # EmbeddingProvider trait + optional FastEmbedProvider
├── snapshot/    # Clone-based snapshot storage
└── api/         # Public API surface
```

## CONVENTIONS

- **rusqlite (bundled SQLite) is the sole external dependency for core** — `feature = "embed"` adds optional FastEmbed
- **Trait-based storage** — `StorageAdapter` trait, swappable implementations
- **Pure functions for mechanics** — all scoring/decay functions are pure (no side effects)
- **Builder pattern for configuration** — EngineConfig, QueryConfig
- **No async in core** — synchronous API, async wrapper can be added externally
- **Salience as shared signal** — all mechanics read/write salience; memory tiers emerge naturally from salience ranges
- **Fragments over summaries** — preserve individual conversation turns as nodes; summaries are emergent via consolidation, not lossy rewrites
- **Origin on every node** — multi-agent graphs track which agent produced each fragment (`agent_id`, `session_id`, `scope`, `confidence`)

## COMMANDS

```bash
cargo build                    # Build (default features, no FastEmbed)
cargo build --features embed   # Build with optional FastEmbed provider
cargo test                     # Run tests
cargo doc --open               # Generate docs
cargo bench                    # Run benchmarks
```

## ANTI-PATTERNS

- No `unwrap()` in library code — use `Result<T, E>` everywhere
- No `println!` in library code — use `log` crate if logging needed
- No global state — all state is in Graph/Engine instances

## Core API Design

```rust
/// The Anamnesis cognitive graph engine.
///
/// Generic over storage backend. Default: SqliteStorage (in-memory SQLite, zero-config).
/// Requires `S: Clone` for snapshot support.
pub struct Engine<S: StorageAdapter + Clone = SqliteStorage> {
    graph: Graph<S>,
    config: EngineConfig,
    snapshots: SnapshotStore<S>,
}

impl Engine<SqliteStorage> {
    /// Create a new engine with default configuration and in-memory SQLite storage.
    pub fn new() -> Self;

    /// Create a new engine with custom configuration and in-memory SQLite storage.
    pub fn with_config(config: EngineConfig) -> Self;
}

impl<S: StorageAdapter + Clone> Engine<S> {
    /// Create an engine with a custom storage backend.
    pub fn with_storage(config: EngineConfig, storage: S) -> Self;

    // --- Snapshot ---

    /// Store a clone of the current storage state under a label.
    pub fn snapshot(&mut self, label: &str) -> Result<SnapshotId, Error>;

    /// Restore the graph storage from a previously captured snapshot.
    pub fn restore(&mut self, id: &SnapshotId) -> Result<(), Error>;

    /// List stored snapshot metadata in insertion order.
    pub fn list_snapshots(&self) -> Vec<(SnapshotId, String, Timestamp)>;

    // --- Core operations ---

    /// Ingest a new observation — applies perception gating and attraction auto-linking.
    /// Returns IngestResult::Created (new node) or IngestResult::Reinforced (dedup hit).
    pub fn ingest(&mut self, observation: Observation) -> Result<IngestResult, Error>;

    /// Crystallize query results into a higher-level knowledge node.
    /// Creates a synthesis node, links it to sources with ConsolidatedFrom edges,
    /// and reinforces each source via touch().
    pub fn crystallize(&mut self, request: CrystallizeRequest) -> Result<CrystallizeResult, Error>;

    /// Create or strengthen a link between two nodes.
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType, weight: f64) -> Result<EdgeId, Error>;

    /// Touch a node — apply lazy decay then reinforce on access.
    /// Ordering invariant: decay (eq 4) BEFORE reinforcement (eq 5).
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error>;

    /// Set the explicit memory tier for a node (Core tier is protected from decay).
    pub fn set_tier(&mut self, node_id: NodeId, tier: MemoryTier) -> Result<(), Error>;

    /// Get the current memory tier of a node.
    pub fn get_tier(&self, node_id: NodeId) -> Result<MemoryTier, Error>;

    /// Advance time — apply batch decay (eq 4) to all nodes, returns TickReport.
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    // --- Query ---

    /// Query the graph — returns structured ContextPackage for LLM consumption.
    /// Associative mode: full spreading activation pipeline (eqs 9-14).
    /// TypeFiltered, Neighborhood, Temporal, List: retrieve nodes by structural criteria.
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error>;

    /// Unified search — combines text search, vector similarity, and graph traversal.
    /// Requires text or query_embedding; returns SearchResult with ContextPackage and trace.
    pub fn search(&self, input: SearchInput) -> Result<SearchResult, Error>;

    /// Query for facts valid at a specific point in time (bitemporal filtering).
    pub fn fact_at(&self, query: &Query, as_of: Timestamp) -> Result<ContextPackage, Error>;

    // --- Debug lifecycle ---

    /// Start a debugging session for a problem statement.
    pub fn start_debug(&mut self, problem: &str, origin: Origin, timestamp: Timestamp) -> Result<NodeId, Error>;

    /// Log a hypothesis inside an existing debugging session.
    pub fn log_hypothesis(&mut self, session: NodeId, text: &str, origin: Origin, timestamp: Timestamp) -> Result<NodeId, Error>;

    /// Log evidence against an existing hypothesis.
    pub fn log_evidence(&mut self, hypothesis: NodeId, text: &str, result: EvidenceResult, origin: Origin, timestamp: Timestamp) -> Result<NodeId, Error>;

    /// Mark a hypothesis as rejected with a reason.
    pub fn reject_hypothesis(&mut self, hypothesis: NodeId, reason: &str, timestamp: Timestamp) -> Result<(), Error>;

    /// Mark a hypothesis as confirmed with a conclusion.
    pub fn confirm_hypothesis(&mut self, hypothesis: NodeId, conclusion: &str, timestamp: Timestamp) -> Result<(), Error>;

    /// End a debugging session and record its final outcome.
    pub fn end_debug(&mut self, session: NodeId, outcome: DebugOutcome, timestamp: Timestamp) -> Result<(), Error>;

    /// Search for rejected hypotheses matching a query string (case-insensitive).
    pub fn search_rejected_hypotheses(&self, query: &str, limit: usize) -> Result<Vec<NodeId>, Error>;

    // --- Cross-agent ---

    /// Cross-agent entity linking after parallel execution round.
    /// Creates Entity edges between nodes from different agents sharing entity tags.
    /// No LLM calls — metadata matching only.
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;

    // --- Deprecated ---

    /// Deprecated since 0.3.0: use EngineConfig::dedup_threshold in ingest() instead.
    #[deprecated(since = "0.3.0", note = "Dedup gate in ingest() replaces this need. See EngineConfig::dedup_threshold.")]
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<MergePair>, Error>;

    /// Deprecated since 0.3.0: use EngineConfig::dedup_threshold in ingest() instead.
    #[deprecated(since = "0.3.0", note = "Dedup gate in ingest() replaces this need. See EngineConfig::dedup_threshold.")]
    pub fn auto_merge(&mut self, threshold: f64) -> Result<MergeLog, Error>;
}
```

## EngineConfig Fields

```rust
pub struct EngineConfig {
    /// Maximum number of nodes before perception gate rejects new observations.
    pub max_nodes: usize,                  // default: 100_000
    /// Minimum novelty score [0, 1] for an observation to enter the graph.
    pub novelty_threshold: f64,            // default: 0.30
    /// Minimum confidence [0, 1] for an observation to enter the graph.
    pub confidence_threshold: f64,         // default: 0.50
    /// Similarity threshold above which ingest reinforces an existing node instead of creating one.
    pub dedup_threshold: f64,              // default: 0.92
    /// Whether ingest should detect duplicate embeddings and reinforce existing nodes.
    pub dedup_enabled: bool,               // default: true
    /// Decay model for salience computation. Exponential (default) or PowerLaw (ACT-R).
    pub decay_model: DecayModel,           // default: Exponential
    /// Energy model for final score computation in spreading activation.
    pub energy_model: EnergyModel,         // default: WeightedSum
    /// Spreading activation model for query traversal.
    pub spreading_model: SpreadingModel,   // default: PriorityQueueBfs
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
- ✅ Scoped knowledge (session/project/universal via Origin.scope)
- ✅ Structured query output (ContextPackage with identity/knowledge/memories/tensions)
- ✅ Multi-resolution content (L0 name / L1 summary / L2 full content)
- ✅ Identity management (agent personas as graph nodes with dynamics)
- ✅ Episodic preservation (original text + extracted knowledge linked)
- ✅ Contradiction detection (Contradicts edges surfaced during queries)
- ✅ All query modes (Associative, TypeFiltered, Neighborhood, Temporal, List)
- ✅ Unified search (`search()` — text + vector + graph traversal)
- ✅ Post-session consolidation (`crystallize()` — ConsolidatedFrom edges, salience promotion)
- ✅ Cross-agent entity linking (`reflect_batch()` — entity tag matching, Entity edges)
- ✅ Debug lifecycle (DebugSession, Hypothesis, Evidence nodes with typed edges)
- ✅ Clone-based snapshots (`snapshot()`, `restore()`, `list_snapshots()`)
- ✅ Bitemporal queries (`fact_at()` — valid_from/valid_until filtering)
- ✅ Optional embedding provider (`EmbeddingProvider` trait; `FastEmbedProvider` behind `feature = "embed"`)
- ⚠️ `merge_candidates()` / `auto_merge()` — deprecated; use `EngineConfig::dedup_threshold` instead

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
    IdentityCore,    // Immutable core trait, no decay ("I am a code architect")
    IdentityLearned, // Experience-formed trait, very slow decay ("prefers factory pattern")
    IdentityState,   // Current state, normal decay ("refactoring auth module")

    // Knowledge (Planet — medium mass, moderate decay)
    Semantic,        // Extracted fact from conversation or document
    Procedural,      // How-to or execution pattern
    Entity,          // Named concept, module, person, or service
    Convention,      // Project rule or convention
    Decision,        // Decision with rationale
    Gotcha,          // Pitfall or warning
    Hypothesis,      // Debugging hypothesis (inert: lambda=0, floor=1.0)
    Evidence,        // Evidence logged against a hypothesis (inert: lambda=0, floor=1.0)
    DebugSession,    // Root node for a debugging session (inert: lambda=0, floor=1.0)

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
    Supports,            // Evidence supports a hypothesis. kappa = 1.10
    Refutes,             // Evidence refutes a hypothesis. kappa = 0.30
    BelongsTo,           // Hypothesis belongs to a debug session. kappa = 0.95

    // Inhibitory
    Contradicts,         // Conflicting assertions. Excluded from propagation; applies repulsion.

    Custom(String),      // Consumer-defined edge type
}

/// Tracks which agent produced a knowledge fragment.
struct Origin {
    agent_id: String,
    session_id: String,
    scope: ScopePath, // ScopePath::universal() = universal
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
    Associative { seed: NodeId, budget: usize },             // Full spreading activation pipeline
    TypeFiltered { node_type: KnowledgeType, limit: usize }, // Nodes of a type, ordered by salience
    Neighborhood { entity: NodeId, depth: usize },            // k-hop BFS subgraph
    Temporal { since: Timestamp, node_types: Option<Vec<KnowledgeType>>, limit: usize }, // Recent nodes
    List { min_salience: f64, limit: usize },                 // All nodes above salience threshold
}
```

## EmbeddingProvider

```rust
/// Trait for embedding text into vectors.
///
/// Implementations must be synchronous and thread-safe (Send + Sync).
/// The core engine uses f64 embeddings; providers typically return f32.
/// Use embed_f64() or widen() to convert.
pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error>;
    fn dimensions(&self) -> usize;
    fn model_name(&self) -> &str;

    // Default convenience methods
    fn embed_single(&self, text: &str) -> Result<Vec<f32>, Error>;
    fn embed_f64(&self, texts: &[&str]) -> Result<Vec<Vec<f64>>, Error>;
}

/// Convert f32 embedding slice to Vec<f64>.
pub fn widen(v: &[f32]) -> Vec<f64>;
```

The optional `FastEmbedProvider` (BAAI/bge-base-en-v1.5, 768 dims) is available behind `feature = "embed"`:

```toml
[dependencies]
anamnesis = { version = "0.4", features = ["embed"] }
```

Note: `FastEmbedProvider::new()` downloads the model on first call (~100-500 MB). This is a runtime side effect, not a core behavior.

## StorageAdapter Interface

21 required methods across six groups, plus 7 default methods (6 helper + flush):

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

    // Default helpers (O(N) scan; override for O(1) index lookup)
    fn nodes_by_entity_tag(&self, tag: &str) -> Vec<NodeId>;
    fn nodes_by_type(&self, kt: &KnowledgeType) -> Vec<NodeId>;
    fn nodes_by_agent(&self, agent_id: &str) -> Vec<NodeId>;
    fn nodes_by_scope(&self, scope: &ScopePath) -> Vec<NodeId>;
    fn node_ids_descending(&self) -> Vec<NodeId>;
    fn text_search(&self, query: &str, limit: usize) -> Vec<(NodeId, f64)>;

    // Flush — default no-op; override for write-behind backends
    // Called by Engine::tick() and Engine::snapshot() to commit pending writes.
    fn flush(&mut self) -> Result<(), Error> { Ok(()) }
}
```

Ships with `SqliteStorage` (bundled SQLite via rusqlite, FTS5 full-text search, write-behind dirty tracking for hot fields). `Engine::new()` opens an in-memory SQLite database — zero config, no files. For persistence, use `SqliteStorage::open(path)`. Implement the trait for PostgreSQL, Neo4j, or any other backend.

## Direction

Planned features for future releases — none of these are implemented yet:

- **Social reinforcement scoring** — multi-agent salience bonus when multiple agents independently observe the same fragment
- **`crystallize(session_id: &str)`** — session-level auto-consolidation (current `crystallize()` is consumer-driven; session-level auto-detection is not yet implemented)
