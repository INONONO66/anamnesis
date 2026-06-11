<p align="center">
  <h1 align="center">Anamnesis</h1>
</p>

<p align="center">
  <strong>Cognitive memory engine for LLMs</strong><br>
  A conductive network with associative recall, power-law forgetting, and contradiction held as tension.
</p>

<p align="center">
  <a href="https://github.com/INONONO66/anamnesis/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/INONONO66/anamnesis/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-2024_edition-orange?style=flat-square&logo=rust" alt="Rust 2024"></a>
  <a href="https://crates.io/crates/anamnesis"><img src="https://img.shields.io/crates/v/anamnesis?style=flat-square" alt="crates.io"></a>
  <a href="https://codecov.io/gh/INONONO66/anamnesis"><img src="https://img.shields.io/codecov/c/github/INONONO66/anamnesis?style=flat-square&label=coverage" alt="Coverage"></a>
  <a href="https://docs.rs/anamnesis"><img src="https://img.shields.io/docsrs/anamnesis?style=flat-square" alt="docs.rs"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> · <a href="docs/README.md">Docs</a> · <a href="docs/00-foundation/vision.md">Vision</a> · <a href="docs/01-system-architecture/overview.md">Architecture</a>
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

**Not a database.** A conductive network with cognitive dynamics (the formal spec lives in [`docs/`](docs/README.md)):

| Mechanic | What It Does |
|:---------|:-------------|
| **Associative recall** | Additive directed **random-walk-with-restart (RWR)** spreads activation from query seeds along typed edges; converging evidence sums (never max). |
| **Conductance** | Edges hold an associative-strength reservoir (a log-likelihood-ratio); committed co-use strengthens links via an Oja-bounded Hebbian update. |
| **Forgetting** | Memory strength `A_i = B_i + P_i`: `B_i` is the ACT-R **base-level** activation over the access-trace history, where each trace decays at an **activation-dependent rate** (Pavlik & Anderson 2005) — so spaced repetition outlasts massed (the **spacing effect**). `P_i` is a decay-exempt **evidence prior** (surprise, feedback, peer trust). Use raises `B_i`; disuse fades it — never deleted. |
| **Perception** | **Surprise-gated** input: an observation charges memory in proportion to prediction error, then novelty/confidence/budget decide whether it allocates a new site or routes to the nearest one. |
| **Frustration** | Contradictions are **excluded from propagation** and surfaced as tension (`sigma_ij`), never overwritten — both sides keep their provenance. |

One cue activates related fragments, which activate further fragments — reconstructing understanding from partial cues the way human recall works. Keyword and embedding search find the first cue; the conductive network decides what comes back with it.

> **Reservoirs vs projections** ([ADR-0002](docs/adr/0002-reservoir-projection-state.md), [ADR-0008](docs/adr/0008-powerlaw-dissipation.md)): per node, the persistent state is the bounded access-trace history (which drives the base level `B_i`, recomputed on demand and never stored) plus a decay-exempt evidence prior `P_i`; per edge, `conductance` is an unbounded log-LR reservoir. The public `salience = logistic(B_i + P_i)` / `weight` in `[0, 1]` are bounded `logistic` projections, refreshed by the write paths (`ingest`, `link`, `touch`, `commit`, `crystallize`, `tick`). The invariant is that **read-only retrieval (`query` / `search` / `fact_at`) never mutates persistent state** — it changes only through explicit writes and time.

> **See it work** → [Cognitive-fidelity results](docs/07-quality-gates/fidelity-results.md): charts of power-law forgetting, the spacing effect (with its retention-interval crossover), and the fan effect — produced by the engine itself, from the same paradigms the CI gate asserts.

### How It Compares

| | Storage Unit | Retrieval | Decay | Relationships | Reasoning |
|:--|:--|:--|:--|:--|:--|
| Mem0 | Extracted facts | Embedding similarity | None | None | Facts only |
| Letta | Conversation history | Text search | Archive tier | Basic | Session recall |
| Stanford ACE | Monolithic playbook | Full load | Curator rewrite | None | Strategy-level |
| **Anamnesis** | **Fragments** | **Graph traversal** | **Decay + revival** | **Typed edges** | **Full chains** |

### Positioning

**vs RAG pipelines** — Anamnesis makes zero LLM calls in its core. Retrieval is deterministic graph traversal, not embedding similarity lookup. No embedding drift, no inference cost on every query.

**vs LLM context documents** — Context docs require manual compilation, suffer brevity bias on every rewrite, and have no mechanism for forgetting or contradiction detection. Anamnesis handles all three automatically: power-law dissipation removes stale knowledge, spreading activation surfaces relevant fragments, and `Contradicts` edges surface tensions in query results.

**vs vector-only stores** — Embedding similarity finds *similar* fragments. Spreading activation finds *related* fragments — following typed reasoning chains (causes, contradictions, decisions, confirmations) that embed alone cannot represent. Embeddings are useful cues, but they are not the memory itself.

### Engine vs Consumer

Anamnesis is a **library — a memory kernel**, not a service. It owns the *physics of memory* (storage, spreading activation, dissipation, reinforcement, frustration, temporal validity) and deliberately leaves the *sensory/motor* layer to you. Unlike hosted memory APIs (Mem0, Zep, Supermemory) that bundle extraction + embeddings + serving, Anamnesis stays a deterministic, local-first, embeddable core that you drive.

| The engine provides | You implement (or wrap) |
|:--|:--|
| Graph + reservoirs, RWR retrieval, readout, packaging | **Encoding**: raw input → `Observation` (type, entity tags, origin, timestamp) — usually via an LLM |
| Power-law dissipation, commit-gated reinforcement | **Embeddings**: the `EmbeddingProvider` (node *and* query vectors are caller-supplied) |
| Frustration, `fact_at` bitemporal validity | **Edge strategy**: provide embeddings (auto-coupling) or call `link()` |
| Snapshots, SQLite storage, health/invariants | **Queries & commit**: when to query, and when use is *committed* (reinforcement) |
| Pure mechanics, no LLM calls, no background tasks | **`tick(now)` scheduling**, **LLM answering**, **serving** (e.g. an MCP bridge) |

## Benchmarks

Long-term conversational memory benchmarks, **retrieval-only dry runs**: no LLM
anywhere — ingest is raw turns + embeddings (`bge-base-en-v1.5`), retrieval is
the engine's deterministic pipeline. Reproducible via
`cargo bench --features embed --bench real_memory` (see
[calibration records](docs/07-quality-gates/calibration-records.md) for full
provenance, ablations, and negative results).

| Benchmark | Gold granularity | Recall@20 | MRR | NDCG@20 | p50 |
|:--|:--|--:|--:|--:|--:|
| **LongMemEval-S** (stratified 30/type, all 6 types, 180 q) | session-level | **94.7%** | **0.861** | 0.824 | ~18 ms |
| **LoCoMo** (full non-adversarial, 1540 q) | turn-level strict | **77.6%** | **0.291** | 0.386 | ~30 ms |

Read these numbers for what they are:

- **Retrieval metrics, not answer accuracy.** Published memory-system scores
  (Mem0, Zep, LangMem, …) are LLM-as-judge *answer* scores — a different
  measurement. These numbers bound what an answer stage could see in context
  (LoCoMo hit@20 = 84.6%, LongMemEval hit@1 = 81.1%).
- **No usage learning is measured here.** Runs are cold-start (no `commit`
  warmup), so the readout calibration intentionally zeroes the salience
  coefficient (`w_s = 0`) — on unused memory, salience carries only
  encoding-time noise. Deployments that accumulate real usage should refit it
  per [ADR-0010](docs/adr/0010-calibrated-priors-not-laws.md); the
  reinforcement dynamics themselves are validated by the
  [cognitive-fidelity gates](docs/07-quality-gates/fidelity-results.md), not
  by these benchmarks.
- Readout coefficients were fit on the even-sample half of LoCoMo and
  validated on the held-out half (Recall@20 77.8% / MRR 0.287 on unseen
  conversations); LongMemEval numbers use the same weights with zero
  dataset-specific tuning.

## Quick Start

> **Anamnesis is in active development.** The core engine is functional — mechanics, query pipeline, debug lifecycle, snapshots, and unified search are all operational. See [Status](#status) for the full breakdown.

Add to your `Cargo.toml`:

```toml
[dependencies]
anamnesis = "0.5"

# Optional: local embedding provider (downloads model on first use, ~100-500 MB)
# anamnesis = { version = "0.5", features = ["embed"] }
```

```rust
use anamnesis::Engine;
use anamnesis::api::{IngestResult, Observation};
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::peer::{SourceKind, TrustLevel};

let mut engine = Engine::new();

// Register a peer (human or agent) before ingesting
let agent_id = engine.register_peer("my-agent", TrustLevel::Agent).unwrap();

// Ingest knowledge fragments
let result = engine.ingest(Observation {
    name: "auth uses factory pattern".into(),
    summary: Some("Confirmed across multiple sessions".into()),
    content: "The auth module uses factory pattern for handler creation".into(),
    embedding: Some(vec![0.8, 0.2, 0.1]),
    confidence: 0.9,
    node_type: KnowledgeType::Semantic,
    entity_tags: vec!["auth".into(), "factory-pattern".into()],
    origin: Origin {
        peer_id: agent_id,
        source_kind: SourceKind::AgentObservation,
        session_id: "session-1".into(),
        scope: ScopePath::new("my-project").expect("valid scope"),
        confidence: 0.9,
    },
    timestamp: Timestamp::now(),
    valid_from: None,
    valid_until: None,
}).unwrap();

let ids = match result {
    IngestResult::Created(ids) => ids,
    IngestResult::Reinforced { existing_id, .. } => vec![existing_id],
};

let result2 = engine.ingest(Observation {
    name: "race condition in auth middleware".into(),
    summary: None,
    content: "Found a race condition in the auth middleware during session #5".into(),
    embedding: Some(vec![0.75, 0.3, 0.15]),
    confidence: 0.85,
    node_type: KnowledgeType::Episodic,
    entity_tags: vec!["auth".into()],
    origin: Origin {
        peer_id: agent_id,
        source_kind: SourceKind::AgentObservation,
        session_id: "session-5".into(),
        scope: ScopePath::new("my-project").expect("valid scope"),
        confidence: 0.85,
    },
    timestamp: Timestamp::now(),
    valid_from: None,
    valid_until: None,
}).unwrap();

let ids2 = match result2 {
    IngestResult::Created(ids) => ids,
    IngestResult::Reinforced { existing_id, .. } => vec![existing_id],
};

// Connect related knowledge (conductance is seeded from the cold-start coupling
// prior — edge weight is a projection, never passed in)
engine.link(ids[0], ids2[0], EdgeType::Semantic).unwrap();

// Reinforce on access (appends an access trace, raising the base level B_i)
engine.touch(ids[0], Timestamp::now()).unwrap();
```

## Core Concepts

<details>
<summary><strong>Indexes Trigger; Graph Remembers</strong></summary>

<br>

Anamnesis separates retrieval cues from memory representation. Keyword search, BM25-style full-text search, entity tags, temporal filters, and optional embeddings are **trigger indexes**: they find candidate `NodeId`s that may start recall.

The actual memory is the graph: nodes, typed edges, salience, timestamps, validity windows, and origin metadata. Once a cue finds a seed, spreading activation reconstructs the surrounding context: what happened, who produced it, when it was valid, what it supports or contradicts, and why a decision was made.

```text
query
  -> keyword / BM25 / embedding / entity / time triggers
  -> candidate seed nodes
  -> graph spreading activation
  -> identity + knowledge + memories + tensions
```

This means indexes can be rebuilt or replaced without changing memory. The graph remains the source of truth.

</details>

<details>
<summary><strong>Fragments, Not Summaries</strong></summary>

<br>

Existing systems summarize conversations into compact facts — lossy by design. The reasoning, context, and rejected alternatives are discarded.

Anamnesis preserves **individual conversation turns as nodes**. Each retains original content, temporal position, entity references, and origin metadata. Summaries are emergent — they arise when repeated patterns consolidate into higher-level semantic nodes. The raw fragments remain.

</details>

<details>
<summary><strong>Identity-Conditioned Recall</strong></summary>

<br>

Identity is not a runtime behavior prompt. It is represented by high-salience identity nodes inside the same graph and acts as a retrieval prior:

| Type | Role | Dynamics |
|:-----|:-----|:---------|
| `IdentityCore` | Stable retrieval anchors and operating principles | No decay; high salience |
| `IdentityLearned` | Experience-formed preferences and conventions | Very slow decay; reinforced by repeated success |
| `IdentityState` | Current task or stance | Normal decay; scope-sensitive |

Identity nodes bias recall, ranking, and tension detection. They do **not** hide contradictory facts, enforce behavior, or replace a system prompt; the consumer decides how retrieved identity fragments are exposed to an LLM.

</details>

<details>
<summary><strong>Scoped Knowledge</strong></summary>

<br>

Every node carries `Origin` metadata: `agent_id`, `session_id`, a hierarchical scope path, and `confidence`.

- `work/company-a` means the memory is scoped to a work domain or workspace.
- `personal/daily-life` means the memory is scoped to a personal domain.
- `personal-projects/anamnesis` means the memory is scoped to a specific personal project.
- `universal` means the memory can participate across scopes.
- Exact-scope memories receive the strongest query weight.
- Ancestor/domain memories receive a medium boost.
- Universal memories remain available across scopes.
- Sibling or unrelated scopes are downweighted unless explicitly requested or strongly connected by entities.

Scoped memories can be crystallized upward: session evidence can become project knowledge, project knowledge can become domain knowledge, and domain knowledge can become universal principles. The original scoped memories remain as evidence via `ConsolidatedFrom` edges; promotion is additive, not destructive.

Examples of scope paths:

```text
universal
work/company-a/backend-platform
personal/daily-life
personal-projects/anamnesis/search
```

</details>

<details>
<summary><strong>Forgetting Is a Feature</strong></summary>

<br>

Salience is `logistic(B_i + P_i)`. As time passes without access, the base level `B_i` falls (the access traces age), so salience drops on `tick()`. A committed access via `touch()` appends a fresh trace, raising `B_i` (and hence salience) back up; the decay-exempt evidence prior `P_i` is left untouched.

```
March:     Node created, salience 0.7
June:      No access — B_i has aged, salience → 0.08 (below threshold, invisible)
September: Direct mention → touch() appends a fresh trace → B_i (and salience) recover
           Connected nodes reactivate via spreading activation
```

A node at salience 0.03 is invisible to queries but **still exists** in the graph. The base level decayed, not the memory itself.

</details>

<details>
<summary><strong>Emergent Memory Tiers</strong></summary>

<br>

Tiers are **salience ranges**, not separate stores. Reinforcement and dissipation naturally distribute nodes:

| Tier | Salience | Role |
|:-----|:---------|:-----|
| Core Memory | > 0.8 | Project conventions, active decisions. Kept high by repeated committed use. |
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
| `Reason` | Why a decision was made |
| `RejectedAlternative` | Option considered and discarded |
| `Supersedes` | Replaces outdated knowledge (sets validity windows) |
| `ReinforcedBy` | Confirmed by repeated experience |
| `ConsolidatedFrom` | Derived from multiple fragments |
| `Contradicts` | Conflict — excluded from propagation, surfaced as frustration |

When a new agent session starts, it inherits not rules but *judgment*.

</details>

## Architecture

```
src/
├── graph/          Node, Edge, Origin, scope, time, types — data + reservoirs
├── mechanics/      Pure cognitive functions, no side effects
│   ├── perception     Surprise gating — novelty, confidence, budget
│   ├── attraction     Cosine/entity coupling for cold-start edge creation
│   ├── interactions   Dissipation, Rescorla-Wagner, Oja-bounded Hebbian updates
│   ├── frustration    Contradiction stress (sigma_ij), surfaced not deleted
│   ├── energy         Query-local energy objective E(S | Q)
│   ├── projection     Reservoir ↔ bounded projection (logistic / logit)
│   └── priors         Calibrated irreducible priors (d, L, N, k, …)
├── query/          Additive directed RWR, potential field, 7-term readout, search
├── storage/        StorageAdapter trait + SqliteStorage
├── embedding/      EmbeddingProvider trait + optional FastEmbedProvider
├── snapshot/       Clone-based snapshot storage
└── api/            Engine — public interface (ingest, query, commit, tick, …)
```

<details>
<summary><strong>Data Flow</strong></summary>

<br>

```
Observation
  │  surprise-gated perception (novelty / confidence / budget)
  ▼
Ingest ── allocate new site OR route to nearest ──► Graph (reservoirs)
  │  cold-start coupling may seed a Semantic edge (embedding/entity above threshold)
  ▼
Query ── additive directed RWR from seeds ──► 7-term readout ──► budget-bounded ContextPackage
  │       (read-only: reservoirs unchanged; Contradicts excluded, surfaced as frustration)
  ▼
Commit ── write-back for used memories ──►
          append access traces (B_i) + evidence-prior update (P_i)
          + Oja-bounded Hebbian edge strengthening
          (touch()/touch_batch() append a trace directly; tick() advances time)

         ┌────────────────────────────────────────┐
         │  tick(now) — periodic                  │
         │  recompute salience from B_i(now)      │
         │  + edge leakage; flush storage         │
         └────────────────────────────────────────┘

         ┌────────────────────────────────────────┐
         │  crystallize() / reflect_batch()       │
         │  synthesis + cross-agent Entity links  │
         └────────────────────────────────────────┘
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
    pub fn with_storage<S: StorageAdapter + Clone>(config: EngineConfig, storage: S) -> Self;

    // Snapshots
    pub fn snapshot(&mut self, label: &str) -> SnapshotId;
    pub fn restore(&mut self, id: &SnapshotId) -> Result<(), Error>;
    pub fn list_snapshots(&self) -> Vec<(SnapshotId, String, Timestamp)>;

    // Core operations
    pub fn ingest(&mut self, observation: Observation) -> Result<IngestResult, Error>;
    pub fn crystallize(&mut self, request: CrystallizeRequest) -> Result<CrystallizeResult, Error>;
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType) -> Result<EdgeId, Error>;
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error>;
    pub fn set_tier(&mut self, node_id: NodeId, tier: MemoryTier) -> Result<(), Error>;
    pub fn get_tier(&self, node_id: NodeId) -> Result<MemoryTier, Error>;
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    // Query — returns structured context for LLM consumption
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error>;
    pub fn search(&self, input: SearchInput) -> Result<SearchResult, Error>;
    pub fn fact_at(&self, query: &Query, as_of: Timestamp) -> Result<ContextPackage, Error>;

    // Debug lifecycle
    pub fn start_debug(&mut self, problem: &str, origin: Origin, timestamp: Timestamp) -> Result<NodeId, Error>;
    pub fn log_hypothesis(&mut self, session: NodeId, text: &str, origin: Origin, timestamp: Timestamp) -> Result<NodeId, Error>;
    pub fn log_evidence(&mut self, hypothesis: NodeId, text: &str, result: EvidenceResult, origin: Origin, timestamp: Timestamp) -> Result<NodeId, Error>;
    pub fn reject_hypothesis(&mut self, hypothesis: NodeId, reason: &str, timestamp: Timestamp) -> Result<(), Error>;
    pub fn confirm_hypothesis(&mut self, hypothesis: NodeId, conclusion: &str, timestamp: Timestamp) -> Result<(), Error>;
    pub fn end_debug(&mut self, session: NodeId, outcome: DebugOutcome, timestamp: Timestamp) -> Result<(), Error>;
    pub fn search_rejected_hypotheses(&self, query: &str, limit: usize) -> Result<Vec<NodeId>, Error>;

    // Cross-agent
    pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;

    // Commit — write-back for the retrieval loop: reinforces the memories actually
    // used and strengthens co-used edges (commit-gated Hebbian). Read-only query
    // changes nothing; touch()/tick() also mutate reservoirs by other paths.
    pub fn commit(&mut self, package: ContextPackage, feedback: Option<ConfidenceLevel>)
        -> Result<(ContextPackage, CommitReport), Error>;

    // Peers
    pub fn register_peer(&mut self, name: impl Into<String>, trust_level: TrustLevel)
        -> Result<PeerId, Error>;
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

</details>

## Design Principles

- **rusqlite (bundled SQLite) is the sole external dependency for core** — optional `feature = "embed"` adds FastEmbed
- **Pure functions** for all mechanics — testable, benchmarkable, no side effects
- **Pluggable storage** via `StorageAdapter` trait
- **No async in core** — consumers wrap with async if needed
- **No LLM calls** — engine provides primitives; extraction is the consumer's job
- **No global state** — all state in `Engine` instances
- **Salience as shared signal** — all mechanics read/write salience; tiers emerge naturally from salience ranges
- **Indexes trigger; graph remembers** — keyword, BM25, embedding, and temporal indexes find entry points; graph nodes and edges remain the source of truth

<details>
<summary><strong>Three-Role Processing (Consumer Pattern)</strong></summary>

<br>

A recommended — but not enforced — processing pattern adapted from Stanford ACE:

| Role | When | Engine Primitives |
|:-----|:-----|:------------------|
| **Generator** | During ingestion | `ingest()`, `link()` |
| **Reflector** | Session completion | `link()`, `touch()` |
| **Curator** | Periodic batch | `tick()`, `crystallize()` |

The Generator extracts nodes from conversation turns. The Reflector reviews and creates cross-session reasoning edges. The Curator applies decay and consolidates patterns. These roles run **in the consumer** — the engine provides graph primitives only.

</details>

## Development

```bash
cargo build                    # Build (default features, no FastEmbed)
cargo build --features embed   # Build with optional FastEmbed provider
cargo test                     # Run tests
cargo fmt --check              # Formatting
cargo clippy --all-targets --all-features -- -D warnings  # Lint (zero warnings required)
cargo test --all-targets --all-features --no-run          # Compile tests and benches without running long benchmarks
cargo doc --open               # Docs
cargo bench                    # Run benchmarks
```

### Release gate

Before publishing or tagging a release, run the same hard gates as CI:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo test --doc --all-features
cargo test --all-targets --all-features --no-run
```

CI installs `cargo-nextest` before running the test gate. If `cargo-nextest` is not available locally, use `cargo test --all-features` as the local functional-test equivalent.

CI also runs the MSRV check (`cargo check --all-targets --all-features` on Rust 1.85), `cargo deny`, and PR semver checks. Run those locally when the corresponding tools are installed, especially before publishing a release.

`cargo test --all-targets` intentionally is not a release gate because this crate has `harness = false` benchmark binaries that execute long-running benchmarks when invoked as test targets. Use `cargo bench` or the manual benchmark workflow for performance runs.

## Status

**v0.5.0** — migrated to the **conductive-network** model: additive directed RWR, log-odds reservoirs with bounded projections, power-law dissipation, commit-gated Hebbian learning, and frustration. Breaking redesign vs 0.4 (force/gravity/BFS/Hopfield models removed); the [techspec](docs/README.md) is the source of truth.

Node strength is now decomposed as `A_i = B_i + P_i` ([ADR-0008](docs/adr/0008-powerlaw-dissipation.md)): the ACT-R base level `B_i` is recomputed on demand from the access-trace history (forgetting and use-driven reinforcement live here), and the persistent evidence prior `P_i` (encoding surprise, feedback, peer trust) is decay-exempt. Each access trace carries its own **activation-dependent decay rate** `d_j = m_type·(c·e^{m_j}+α)` (Pavlik & Anderson 2005), so the multi-trace base level genuinely reproduces the **spacing effect** (the human *testing* effect is not claimed). The edge `conductance` log-LR reservoir is unchanged.

| Layer | Status | Notes |
|:------|:-------|:------|
| Graph (Node, Edge, CRUD) | ✅ | SQLite-backed storage with SoA hot fields and write-behind dirty tracking |
| SQLite storage | ✅ | `SqliteStorage` with FTS5 full-text search, adjacency index, ID recycling, secondary indexes |
| Engine API | ✅ | All method signatures finalized |
| Cold-start coupling | ✅ | Embedding/entity/scope/type-weighted seed creates `Semantic` edges in `ingest()` |
| Conductance learning | ✅ | Commit-gated Oja-bounded Hebbian edge strengthening |
| Perception | ✅ | Surprise-gated, wired into `ingest()` — novelty, confidence, budget |
| Forgetting (dissipation) | ✅ | ACT-R base level `B_i` recomputed from access traces with **per-trace activation-dependent decay** (Pavlik-Anderson; reproduces the spacing effect); `tick()` recomputes salience as `B_i(now)` falls; `touch()` appends an access trace (no scalar decay). Evidence prior `P_i` is decay-exempt |
| Activation flow | ✅ | Additive directed random-walk-with-restart (RWR); BFS/force models removed |
| Frustration | ✅ | `Contradicts` excluded from propagation, surfaced as tension (`sigma_ij`) |
| Identity prior | ✅ | Top-3 identity nodes bias query activation |
| Scope weighting | ✅ | Hierarchical scope-path scoring with entity overlap bonus |
| ContextPackage | ✅ | Structured output: identity/knowledge/memories/tensions |
| Agent tension | ✅ | Contradiction tension measurement in query results |
| Multi-resolution content (L0/L1/L2) | ✅ | Token budget controls fragment detail level |
| Typed edges | ✅ | Edge types with directional type factors (`Contradicts` excluded from flow) |
| Embedding persistence | ✅ | Stored on Node, used for similarity operations |
| Origin attribution | ✅ | agent_id, session_id, scope, confidence |
| Non-Associative query modes | ✅ | TypeFiltered, Neighborhood, Temporal, List — all implemented |
| `search()` unified text + graph | ✅ | Text search + vector similarity + spreading activation |
| `crystallize()` post-session consolidation | ✅ | ConsolidatedFrom edges, salience promotion from sources |
| `reflect_batch()` cross-agent linking | ✅ | Entity edges via entity tag matching, no LLM calls |
| Debug lifecycle | ✅ | DebugSession, Hypothesis, Evidence nodes; start/log/reject/confirm/end APIs |
| `search_rejected_hypotheses()` | ✅ | Case-insensitive search across rejected hypothesis nodes |
| Clone-based snapshots | ✅ | `snapshot()`, `restore()`, `list_snapshots()` |
| Bitemporal queries | ✅ | `fact_at()` — valid_from/valid_until filtering |
| EmbeddingProvider trait | ✅ | Synchronous, Send + Sync; `embed()`, `dimensions()`, `model_name()`, `widen()` |
| FastEmbedProvider | ✅ | Behind `feature = "embed"`; BAAI/bge-base-en-v1.5, 768 dims |
| Memory tier control | ✅ | `set_tier()` / `get_tier()` — Core tier protected from decay |
| Energy objective | ✅ | Query-local `E(S \| Q)` with symmetric-coupling caveat (Hopfield/force models removed) |
| Commit pipeline | ✅ | `commit()` — write-back for query usage: access-trace append (`B_i`) + evidence-prior update (`P_i`) + Hebbian edge learning (read-only retrieval mutates nothing) |
| Social reinforcement scoring | ✅ | Multi-agent corroboration scoring and consumer feedback reservoir updates |

## References

- Collins & Loftus — *A Spreading-Activation Theory of Semantic Processing* (1975)
- Tulving — *Episodic and Semantic Memory* (1972)
- Stanford ACE — *Agentic Context Engineering* (ICLR 2026)
- Anthropic — *Effective Context Engineering for AI Agents* (2025)

## License

[MIT](LICENSE)
