<p align="center">
  <h1 align="center">Anamnesis</h1>
</p>

<p align="center">
  <strong>Cognitive graph engine for LLMs</strong><br>
  Knowledge with attraction, gravity, perception, and forgetting.
</p>

<p align="center">
  <a href="https://github.com/INONONO66/anamnesis/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/INONONO66/anamnesis/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-2024_edition-orange?style=flat-square&logo=rust" alt="Rust 2024"></a>
  <a href="https://crates.io/crates/anamnesis"><img src="https://img.shields.io/badge/crates.io-v0.2.0-e6b44c?style=flat-square" alt="crates.io"></a>
  <a href="https://codecov.io/gh/INONONO66/anamnesis"><img src="https://img.shields.io/codecov/c/github/INONONO66/anamnesis?style=flat-square&label=coverage" alt="Coverage"></a>
  <a href="https://docs.rs/anamnesis"><img src="https://img.shields.io/docsrs/anamnesis?style=flat-square" alt="docs.rs"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> · <a href="docs/vision.md">Vision</a> · <a href="docs/architecture.md">Architecture</a> · <a href="docs/design-decisions/">Design Decisions</a>
</p>

---

> Named after Plato's theory of **anamnesis** (ἀνάμνησις) — the soul already possesses knowledge; learning is recollection triggered by the right cue.

## Why

Every LLM agent session starts from zero. Agents repeat mistakes, rediscover conventions, and lose the reasoning behind past decisions. The industry has converged on solutions that don't solve this:

- **Vector stores** answer *"what was said"* but not *"why it was decided"*
- **Tiered memory** archives conversations but loses cross-session connections
- **Evolving playbooks** improve over time but suffer brevity bias — detail erodes with each rewrite

None provide what a long-running agent actually needs: **fragment-level knowledge with associative retrieval, natural decay, and reasoning preservation.**

## What

Anamnesis gives LLM agents **associative memory**. It builds a graph of knowledge fragments connected by typed relationships — not summaries, not embeddings, not flat text.

**Not a database.** A graph engine with cognitive dynamics:

| Mechanic | What It Does |
|:---------|:-------------|
| **Attraction** | Related fragments cluster together (embedding similarity, entity overlap) |
| **Gravity** | Important nodes (high centrality) attract new knowledge naturally |
| **Perception** | Input gating — not every observation enters the graph (novelty, confidence, budget) |
| **Forgetting** | Salience decays over time; access reinforces. Unused knowledge fades |
| **Spreading Activation** | Query from a seed, activation spreads through edges with decay, returns subgraph within token budget |

One cue activates related fragments, which activate further fragments — reconstructing understanding from partial cues the way human recall works.

### How It Compares

| | Storage Unit | Retrieval | Decay | Relationships | Reasoning |
|:--|:--|:--|:--|:--|:--|
| Mem0 | Extracted facts | Embedding similarity | None | None | Facts only |
| Letta | Conversation history | Text search | Archive tier | Basic | Session recall |
| Stanford ACE | Monolithic playbook | Full load | Curator rewrite | None | Strategy-level |
| **Anamnesis** | **Fragments** | **Graph traversal** | **Decay + revival** | **Typed edges** | **Full chains** |

### Positioning

**vs RAG pipelines** — Anamnesis makes zero LLM calls in its core. Retrieval is deterministic graph traversal, not embedding similarity lookup. No embedding drift, no inference cost on every query.

**vs LLM context documents** — Context docs require manual compilation, suffer brevity bias on every rewrite, and have no mechanism for forgetting or contradiction detection. Anamnesis handles all three automatically: salience decay removes stale knowledge, spreading activation surfaces relevant fragments, and `Contradicts` edges flag tensions in query results.

**vs vector-only stores** — Embedding similarity finds *similar* fragments. Spreading activation finds *related* fragments — following typed reasoning chains (causes, contradictions, decisions, confirmations) that embed alone cannot represent.

## Quick Start

> **Anamnesis is in early development.** The core engine is functional — mechanics are wired and the associative query pipeline is operational. Several features are stubs that return placeholder results. See [Status](#status) for the full breakdown.

Add to your `Cargo.toml`:

```toml
[dependencies]
anamnesis = "0.2"
```

```rust
use anamnesis::{Engine, EngineConfig};
use anamnesis::api::Observation;
use anamnesis::graph::{KnowledgeType, EdgeType, Timestamp};
use anamnesis::graph::node::Origin;

let mut engine = Engine::new();

// Ingest knowledge fragments
let ids = engine.ingest(Observation {
    name: "auth uses factory pattern".into(),
    summary: Some("Confirmed across multiple sessions".into()),
    content: "The auth module uses factory pattern for handler creation".into(),
    embedding: Some(vec![0.8, 0.2, 0.1]),
    confidence: 0.9,
    node_type: KnowledgeType::Semantic,
    entity_tags: vec!["auth".into(), "factory-pattern".into()],
    origin: Origin {
        agent_id: "agent-1".into(),
        session_id: "session-1".into(),
        project_id: Some("my-project".into()),
        confidence: 0.9,
    },
    timestamp: Timestamp::now(),
}).unwrap();

let ids2 = engine.ingest(Observation {
    name: "race condition in auth middleware".into(),
    summary: None,
    content: "Found a race condition in the auth middleware during session #5".into(),
    embedding: Some(vec![0.75, 0.3, 0.15]),
    confidence: 0.85,
    node_type: KnowledgeType::Episodic,
    entity_tags: vec!["auth".into()],
    origin: Origin {
        agent_id: "agent-1".into(),
        session_id: "session-5".into(),
        project_id: Some("my-project".into()),
        confidence: 0.85,
    },
    timestamp: Timestamp::now(),
}).unwrap();

// Connect related knowledge
engine.link(ids[0], ids2[0], EdgeType::Semantic, 0.78).unwrap();

// Reinforce on access (lazy decay + salience boost)
engine.touch(ids[0], Timestamp::now()).unwrap();
```

> **Working:** `ingest` (with perception gating + auto-linking), `link`, `touch` (lazy decay + reinforcement), `tick` (batch decay), `query` (Associative mode with spreading activation, repulsion, identity prior, scope weighting, ContextPackage assembly).
> **Placeholder:** `merge_candidates()`/`auto_merge()` return empty results, `reflect_batch()` returns empty report. Non-Associative query modes (TypeFiltered, Neighborhood, Temporal, List) return empty ContextPackage.

## Core Concepts

<details>
<summary><strong>Fragments, Not Summaries</strong></summary>

<br>

Existing systems summarize conversations into compact facts — lossy by design. The reasoning, context, and rejected alternatives are discarded.

Anamnesis preserves **individual conversation turns as nodes**. Each retains original content, temporal position, entity references, and origin metadata. Summaries are emergent — they arise when repeated patterns consolidate into higher-level semantic nodes. The raw fragments remain.

</details>

<details>
<summary><strong>Forgetting Is a Feature</strong></summary>

<br>

Salience decays over time via `tick()`. Knowledge that matters gets reinforced through `touch()` on access; knowledge that doesn't fades naturally.

```
March:     Node created, salience 0.7
June:      No access, decay → 0.08 (below threshold, invisible)
September: Direct mention → touch() → salience spikes back
           Connected nodes reactivate via spreading activation
```

A node at salience 0.03 is invisible to queries but **still exists** in the graph. The access path weakened, not the memory itself.

</details>

<details>
<summary><strong>Emergent Memory Tiers</strong></summary>

<br>

Tiers are **salience ranges**, not separate stores. Gravity and forgetting naturally distribute nodes:

| Tier | Salience | Role |
|:-----|:---------|:-----|
| Core Memory | > 0.8 | Project conventions, active decisions. Maintained by gravity. |
| Working Knowledge | 0.4 – 0.8 | Current task learnings, session-scoped observations. |
| Accumulated Wisdom | 0.1 – 0.4 | Cross-session knowledge. Surfaced by spreading activation. |
| Archive | < 0.1 | Decayed nodes. Invisible, but reactivatable via `touch()`. |

</details>

<details>
<summary><strong>Reasoning Edges</strong></summary>

<br>

Beyond structural edges (semantic, temporal, causal), Anamnesis preserves decision context:

| Edge Type | Purpose |
|:----------|:--------|
| `REASON` | Why a decision was made |
| `REJECTED_ALTERNATIVE` | Option considered and discarded |
| `SUPERSEDES` | Replaces outdated knowledge |
| `REINFORCED_BY` | Confirmed by repeated experience |
| `CONSOLIDATED_FROM` | Derived from multiple fragments |

When a new agent session starts, it inherits not rules but *judgment*.

</details>

## Architecture

```
src/
├── graph/          Node, Edge, Graph — core data structures
├── mechanics/      Pure scoring functions, no side effects
│   ├── attraction     Cosine similarity, merge candidate detection
│   ├── gravity        PageRank-like centrality scoring
│   ├── perception     Novelty, confidence, and budget gating
│   └── forgetting     Exponential/polynomial decay + reinforcement
├── query/          Spreading activation, k-hop neighborhood
├── storage/        StorageAdapter trait + InMemoryStorage
└── api/            Engine — public interface
```

<details>
<summary><strong>Data Flow</strong></summary>

<br>

```
Observation
  │
  ▼
Perception ── novelty / confidence / budget ──► reject or accept
  │
  ▼
Ingestion ── create node ──► Graph
  │
  ▼
Attraction ── similarity scoring ──► edge creation / merge candidates
  │
  ▼
Gravity ── centrality scoring ──► hub node identification

         ┌────────────────────────────────────┐
         │  tick() — periodic                  │
         │  decay saliences, prune if needed   │
         └────────────────────────────────────┘

Query ── spreading activation ──► budget-constrained ContextPackage
  │
  ▼
Touch ── reinforce on access ──► salience spike + reactivation

         ┌────────────────────────────────────┐
         │  ⬚ merge_candidates() / auto_merge()│
         │  consolidate near-duplicate nodes   │
         └────────────────────────────────────┘
```

</details>

<details>
<summary><strong>API Surface</strong></summary>

<br>

> Methods marked with ⬚ are defined but return placeholder results.

```rust
impl Engine {
    // Construction
    pub fn new() -> Self;
    pub fn with_config(config: EngineConfig) -> Self;
    pub fn with_storage<S: StorageAdapter>(config: EngineConfig, storage: S) -> Self;

    // Core operations
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, Error>;
    pub fn link(&mut self, from: NodeId, to: NodeId, t: EdgeType, w: f64) -> Result<EdgeId, Error>;
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error>;
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    // Query — returns structured context for LLM consumption
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error>;

    // Stubs — return placeholder results
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<MergePair>, Error>;  // ⬚ returns empty
    pub fn auto_merge(&mut self, threshold: f64) -> Result<MergeLog, Error>;          // ⬚ returns empty
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;  // ⬚ returns empty
}
```

</details>

<details>
<summary><strong>Storage Abstraction</strong></summary>

<br>

```rust
pub trait StorageAdapter: Send + Sync {
    // ID allocation (reuses freed IDs)
    fn next_node_id(&mut self) -> NodeId;
    fn next_edge_id(&mut self) -> EdgeId;

    // Node CRUD
    fn set_node(&mut self, node: Node) -> Result<(), Error>;
    fn get_node(&self, id: NodeId) -> Result<&Node, Error>;
    fn get_node_mut(&mut self, id: NodeId) -> Result<&mut Node, Error>;
    fn delete_node(&mut self, id: NodeId) -> Result<(), Error>;

    // Edge CRUD
    fn set_edge(&mut self, edge: Edge) -> Result<(), Error>;
    fn get_edge(&self, id: EdgeId) -> Result<&Edge, Error>;
    fn get_edge_mut(&mut self, id: EdgeId) -> Result<&mut Edge, Error>;
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

</details>

## Design Principles

- **Zero external dependencies** — core uses only `std`
- **Pure functions** for all mechanics — testable, benchmarkable, no side effects
- **Pluggable storage** via `StorageAdapter` trait
- **No async in core** — consumers wrap with async if needed
- **No LLM calls** — engine provides primitives; extraction is the consumer's job
- **No global state** — all state in `Engine` instances
- **Salience as shared signal** — all mechanics read/write salience; tiers emerge naturally from salience ranges

<details>
<summary><strong>Three-Role Processing (Consumer Pattern)</strong></summary>

<br>

A recommended — but not enforced — processing pattern adapted from Stanford ACE:

| Role | When | Engine Primitives |
|:-----|:-----|:------------------|
| **Generator** | During ingestion | `ingest()`, `link()` |
| **Reflector** | Session completion | `link()`, `touch()` |
| **Curator** | Periodic batch | `tick()`, `auto_merge()` |

The Generator extracts nodes from conversation turns. The Reflector reviews and creates cross-session reasoning edges. The Curator applies decay and consolidates patterns. These roles run **in the consumer** — the engine provides graph primitives only.

</details>

## Development

```bash
cargo build          # Build
cargo test           # Run tests
cargo clippy         # Lint
cargo fmt --check    # Formatting
cargo doc --open     # Docs
```

## Status

**v0.2.0** — Cognitive engine complete. All core mechanics wired; query pipeline fully operational.

| Layer | Status | Notes |
|:------|:-------|:------|
| Graph (Node, Edge, CRUD) | ✅ | Arena-based storage with SoA hot fields |
| In-memory storage | ✅ | `InMemoryStorage` with adjacency index, ID recycling |
| Engine API | ✅ | All method signatures finalized |
| Attraction | ✅ | Wired into `ingest()` — auto-linking with type affinity |
| Gravity | ✅ | Mass computation wired into `ingest()` and `query()` |
| Perception | ✅ | Gating wired into `ingest()` — novelty, confidence, budget |
| Forgetting | ✅ | Lazy decay in `touch()`, batch decay in `tick()` |
| Spreading activation | ✅ | Priority-queue BFS with hop decay, salience gate, gravity boost |
| Repulsion | ✅ | Contradicts edges apply damping during query |
| Identity prior | ✅ | Top-3 identity nodes bias query activation |
| Scope weighting | ✅ | Project-aware scoring with entity overlap bonus |
| ContextPackage | ✅ | Structured output: identity/knowledge/memories/tensions |
| Agent tension | ✅ | Contradiction tension measurement in query results |
| Multi-resolution content (L0/L1/L2) | ✅ | Token budget controls fragment detail level |
| Reasoning edge types | ✅ | 12 edge types with directional kappa multipliers |
| Embedding persistence | ✅ | Stored on Node, used for similarity operations |
| Origin attribution | ✅ | agent_id, session_id, project_id, confidence |
| Non-Associative query modes | ⬚ | TypeFiltered, Neighborhood, Temporal, List — return empty ContextPackage |
| `merge_candidates()` / `auto_merge()` | ⬚ | Defined; return empty results |
| `reflect_batch()` | ⬚ | Defined; returns empty report |
| `search()` unified text + graph | 🔜 | FTS + salience combined retrieval |
| `crystallize()` post-session consolidation | 🔜 | Pattern detection, ConsolidatedFrom edges |
| SQLite storage adapter | 🔜 | Persistent storage with FTS5 support |
| Benchmarks (cognitive engine) | 🔜 | CRUD benchmarks exist; mechanics benchmarks needed |

## References

- Collins & Loftus — *A Spreading-Activation Theory of Semantic Processing* (1975)
- Tulving — *Episodic and Semantic Memory* (1972)
- Stanford ACE — *Agentic Context Engineering* (ICLR 2026)
- Anthropic — *Effective Context Engineering for AI Agents* (2025)

## License

[MIT](LICENSE)
