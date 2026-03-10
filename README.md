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
  <a href="https://crates.io/crates/anamnesis"><img src="https://img.shields.io/badge/crates.io-v0.1.0-e6b44c?style=flat-square" alt="crates.io"></a>
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

**Not a database.** A cognitive engine with physics-like dynamics:

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

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
anamnesis = "0.1"
```

```rust
use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::graph::edge::EdgeType;

let mut engine = Engine::new();

// Ingest knowledge fragments
let ids = engine.ingest(Observation {
    content: "auth module uses factory pattern".into(),
    embedding: vec![0.8, 0.2, 0.1],
    confidence: 0.9,
    node_type: "semantic".into(),
}).unwrap();

let ids2 = engine.ingest(Observation {
    content: "race condition in auth middleware".into(),
    embedding: vec![0.75, 0.3, 0.15],
    confidence: 0.85,
    node_type: "episodic".into(),
}).unwrap();

// Connect related knowledge
engine.link(ids[0], ids2[0], EdgeType::Semantic, 0.78).unwrap();

// Reinforce on access
engine.touch(ids[0]).unwrap();

// Query — spreading activation within token budget
let results = engine.query(ids[0], 50).unwrap();
```

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

Query ── spreading activation ──► budget-constrained subgraph
  │
  ▼
Touch ── reinforce on access ──► salience spike + reactivation
```

</details>

<details>
<summary><strong>API Surface</strong></summary>

<br>

```rust
impl Engine {
    // Construction
    pub fn new() -> Self;
    pub fn with_config(config: EngineConfig) -> Self;
    pub fn with_storage(config: EngineConfig, storage: Box<dyn StorageAdapter>) -> Self;

    // Core
    pub fn ingest(&mut self, observation: Observation) -> Result<Vec<NodeId>, String>;
    pub fn link(&mut self, from: NodeId, to: NodeId, t: EdgeType, w: f64) -> Result<EdgeId, String>;
    pub fn tick(&mut self, now: u64) -> Result<(), String>;
    pub fn query(&self, seed: NodeId, budget: usize) -> Result<Vec<NodeId>, String>;
    pub fn touch(&mut self, node_id: NodeId) -> Result<(), String>;

    // Consolidation
    pub fn merge_candidates(&self, threshold: f64) -> Result<Vec<(NodeId, NodeId)>, String>;
    pub fn auto_merge(&mut self, threshold: f64) -> Result<usize, String>;
}
```

</details>

<details>
<summary><strong>Storage Abstraction</strong></summary>

<br>

```rust
pub trait StorageAdapter: Send + Sync {
    fn get_node(&self, id: NodeId) -> StorageResult<Node>;
    fn set_node(&mut self, id: NodeId, node: Node) -> StorageResult<()>;
    fn delete_node(&mut self, id: NodeId) -> StorageResult<()>;
    fn get_edge(&self, id: u64) -> StorageResult<Edge>;
    fn set_edge(&mut self, id: u64, edge: Edge) -> StorageResult<()>;
    fn delete_edge(&mut self, id: u64) -> StorageResult<()>;
    fn list_nodes(&self) -> StorageResult<Vec<NodeId>>;
    fn list_edges(&self) -> StorageResult<Vec<u64>>;
}
```

Ships with `InMemoryStorage`. Implement the trait for SQLite, PostgreSQL, Neo4j, or anything else.

</details>

## Design Principles

- **Zero external dependencies** — core uses only `std`
- **Pure functions** for all mechanics — testable, benchmarkable, no side effects
- **Pluggable storage** via `StorageAdapter` trait
- **No async in core** — consumers wrap with async if needed
- **No LLM calls** — engine provides primitives; extraction is the consumer's job
- **No global state** — all state in `Engine` instances
- **Salience as universal currency** — all mechanics read/write salience; tiers emerge naturally

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

**v0.1.0** — Core mechanics implemented, API surface defined.

| Layer | Status |
|:------|:-------|
| Graph (Node, Edge, CRUD) | ✅ |
| Attraction (cosine similarity, merge detection) | ✅ |
| Gravity (PageRank, degree centrality) | ✅ |
| Perception (novelty, confidence, budget gating) | ✅ |
| Forgetting (exponential + polynomial decay) | ✅ |
| Spreading activation (k-hop, budget-constrained) | ✅ |
| In-memory storage | ✅ |
| Engine API | ✅ |
| Reasoning edge types | 🔜 |
| `tick()` ↔ forgetting integration | 🔜 |
| `query()` ↔ spreading activation integration | 🔜 |
| `auto_merge()` with undo log | 🔜 |
| SQLite storage adapter | 🔜 |
| Benchmarks | 🔜 |

## References

- Collins & Loftus — *A Spreading-Activation Theory of Semantic Processing* (1975)
- Tulving — *Episodic and Semantic Memory* (1972)
- Stanford ACE — *Agentic Context Engineering* (ICLR 2026)
- Anthropic — *Effective Context Engineering for AI Agents* (2025)

## License

[MIT](LICENSE)
