<p align="center">
  <h1 align="center">Anamnesis</h1>
</p>

<p align="center">
  <strong>Cognitive memory engine for LLMs</strong><br>
  A graph of knowledge fragments with associative recall, power-law forgetting, and contradiction held as tension.
</p>

<p align="center">
  <a href="https://github.com/INONONO66/anamnesis/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/INONONO66/anamnesis/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-2024_edition-orange?style=flat-square&logo=rust" alt="Rust 2024"></a>
  <a href="https://crates.io/crates/anamnesis-engine"><img src="https://img.shields.io/crates/v/anamnesis-engine?style=flat-square" alt="crates.io"></a>
  <a href="https://codecov.io/gh/INONONO66/anamnesis"><img src="https://img.shields.io/codecov/c/github/INONONO66/anamnesis?style=flat-square&label=coverage" alt="Coverage"></a>
  <a href="https://docs.rs/anamnesis-engine"><img src="https://img.shields.io/docsrs/anamnesis-engine?style=flat-square" alt="docs.rs"></a>
</p>

<p align="center">
  <a href="#use-in-claude-code--codex">Claude Code &amp; Codex</a> · <a href="#typed-relation-contract">Typed relations</a> · <a href="#quick-start">Quick Start</a> · <a href="docs/README.md">Docs</a> · <a href="docs/00-foundation/vision.md">Vision</a>
</p>

---

> Named after Plato's theory of **anamnesis** (ἀνάμνησις) — the soul already possesses knowledge; learning is recollection triggered by the right cue.

## Why

An LLM agent session does not retain prior-session state unless a consumer
supplies it. Persistent memory must preserve source records, provenance,
relationships between observations, temporal validity, and an explicit
lifecycle for unused material.

Anamnesis stores memory as a **graph of fragments connected by typed edges**.
A decision can retain its reason, and a reversal can retain the decision it
superseded. Alignment scoring (keyword, embedding, entity, and temporal cues)
selects entry points; graph activation surfaces related structure; power-law
forgetting changes the priority of unused material without deleting its source.

## What

Anamnesis is a **Rust library — a memory kernel** — plus a ready-made **Claude Code / Codex plugin** that drives it for a coding agent. It is not a service: the core owns storage, retrieval, forgetting, and contradiction handling, and leaves extraction and serving to the consumer.

| Mechanic | What it does |
|:---------|:-------------|
| **Associative recall** | Additive directed **random-walk-with-restart (RWR)** spreads activation from query seeds along typed edges; converging evidence sums (never max), so a fragment reachable by several paths ranks above one reachable by one. |
| **Conductance** | Edges hold an associative-strength reservoir (a log-likelihood-ratio); committed co-use strengthens links via an Oja-bounded Hebbian update. |
| **Forgetting** | Node strength `A_i = B_i + P_i`: `B_i` is the ACT-R **base-level** activation over the access-trace history, where each trace decays at an **activation-dependent rate** (Pavlik & Anderson 2005) — so spaced repetition outlasts massed (the **spacing effect**). `P_i` is a decay-exempt **evidence prior** (encoding surprise, feedback). Use raises `B_i`; disuse fades it — never deleted. |
| **Perception** | **Surprise-gated** input: an observation charges memory in proportion to prediction error, then novelty / confidence / budget decide whether it allocates a new site or routes to the nearest one. |
| **Frustration** | Contradictions are **excluded from propagation** and surfaced as tension (`sigma_ij`), never overwritten — both sides keep their provenance. |

Typed reasoning edges and contradiction-as-tension preserve relationships that
an untyped result list does not represent; see
[Typed relation contract](#typed-relation-contract).
Alignment scoring remains the primary ranking input. The graph layer surfaces
structure and applies the documented forgetting dynamics.

> **Reservoirs vs projections** ([ADR-0002](docs/adr/0002-reservoir-projection-state.md), [ADR-0008](docs/adr/0008-powerlaw-dissipation.md)): per node, the persistent state is the bounded access-trace history (which drives the base level `B_i`, recomputed on demand and never stored) plus a decay-exempt evidence prior `P_i`; per edge, `conductance` is an unbounded log-LR reservoir. The public `salience = logistic(B_i + P_i)` / `weight` in `[0, 1]` are bounded `logistic` projections, refreshed by the write paths (`ingest`, `link`, `touch`, `commit`, `crystallize`, `tick`). The invariant is that **read-only retrieval (`query` / `search`) never mutates persistent state** — it changes only through explicit writes and time.

> **Fidelity evidence** → [cognitive-fidelity results](docs/07-quality-gates/fidelity-results.md): charts of power-law forgetting, the spacing effect (with its retention-interval crossover), and the fan effect, produced by the same engine paths exercised by the CI gate.

## What it is not

- **Not a vector database.** Embeddings are retrieval cues; the public contract
  is memory ingestion, graph-aware recall, lifecycle, and evidence provenance,
  not general-purpose vector CRUD.
- **Not a cloud memory API.** The current product is a local-first single binary;
  memories live in a caller-owned SQLite file.
- **Not a QA system.** Anamnesis returns evidence and structure; a consumer does
  the answering. Retrieval metrics and consumer-model answer diagnostics are
  reported as separate measurement regimes.
- **Not an identity or trust service.** A namespace has one database owner.
  `PeerId` records producer provenance only; the engine does not authenticate
  producers, learn peer reputation, or promote claims by majority.
- **Not a replacement for project files.** Conventions and specs that belong in your repo (CLAUDE.md, docs) should stay there; anamnesis holds what emerges from conversations — decisions, contradictions, lessons, context.

## Success criteria

What "working" means for a memory engine, in observable terms — check yours with the `stats` tool (usage section):

1. **Recall earns its injection.** Session-start and per-prompt hooks surface
   prior decisions without training on exposure alone. Deliberate MCP recall or
   an explicit `Memory::used` call records use under the consumer's policy.
2. **Capture keeps up.** The extraction backlog drains within a few sessions
   (`extraction backlog` low relative to `captured total`). Hook capture is
   best-effort; once a raw turn is persisted, later extraction failure cannot
   remove it.
3. **The graph stays structured.** Contradictions surface as tensions instead of silently coexisting; `why`-chains are traceable (`relate` edges accumulate alongside captured turns).
4. **Forgetting works.** Stale ratio stays bounded as the graph grows — old, unused memories sink (archival) instead of drowning recall.

## Use in Claude Code & Codex

The most common way to run Anamnesis: **persistent associative memory for a
coding agent.** The plugin wires Anamnesis into Claude Code (and Codex) as
**activation-gated recall** — `SessionStart` seeds a few high-salience project
memories, and every `UserPromptSubmit` injects a read-only spreading-activation
recall **only when the top activation clears a threshold**, so an off-topic
prompt injects nothing. The plugin carries both the hooks and the agent MCP
tools and fetches the matching native binary from the
GitHub Release on first use — no `cargo`, no `npm`, no separate binary step.

**Claude Code** — add the marketplace, install, reload:

```text
/plugin marketplace add INONONO66/anamnesis
/plugin install anamnesis@anamnesis-plugins
/reload-plugins
```

This configuration enables five lifecycle hooks and twelve MCP tools:

| Surface | What ships |
|:--|:--|
| **Hooks** | `SessionStart` (seed recall + extraction nudge), `UserPromptSubmit` (gated recall), `Stop` / `PreCompact` / `SessionEnd` (passive turn capture) |
| **MCP tools** | The 12 tools in the inventory below. |

### MCP tool inventory

This is the authoritative inventory of tools registered by the MCP server:

| Tool | Purpose |
|:--|:--|
| `recall` | Search memory for relevant prior knowledge. |
| `remember` | Store a distilled insight, decision, or lesson. |
| `ingest_conversation` | Ingest an ordered conversation transcript. |
| `ingest_attachment_transcript` | Store a consumer-produced textual attachment transcript with attachment and processor provenance. |
| `relate` | Link two remembered nodes with a typed reasoning relation. |
| `stats` | Report read-only graph health and size statistics. |
| `extract_pending` | Retrieve un-extracted conversation turns for reasoning extraction. |
| `update` | Edit an existing memory's content. |
| `forget` | Soft-delete or permanently erase a memory. |
| `supersede` | Mark a newer memory as superseding an older one. |
| `list` | List memories by salience with optional filters. |
| `get` | Read one memory's full detail by node ID. |

**Automatic capture.** Beyond on-demand `remember`, the plugin captures
the session on its own in two stages. **Stage 1** is passive: supported `Stop`,
`PreCompact`, and `SessionEnd` events submit bounded transcript windows as raw
`Episodic` memories. Submission is fail-open and overlapping windows are
content-hash-deduplicated; an unavailable hook, transcript, or daemon can leave
a turn uncaptured. **Stage 2** is agent-driven extraction: once the un-extracted queue
crosses a threshold, the next `SessionStart` injects a one-line nudge asking the
agent to call the `extract_pending` MCP tool, which hands back the raw turns to
distill into reasoning and lessons via `relate` / `remember`. Both stages are
best-effort and configurable; see **[`plugin/README.md`](plugin/README.md)** for
the hook contract, thresholds, and env-var toggles.

**Codex** — same hook contract, same binary:

```text
codex plugin marketplace add INONONO66/anamnesis
codex plugin add anamnesis@anamnesis-plugins
```

Configuration (the `τ` recall gate, top-`k`, timeouts), the guard-wrapper
rationale, and the Codex visibility caveat live in
**[`plugin/README.md`](plugin/README.md)**.

> **Just the MCP server / CLI** (no plugin): the same binary ships on npm as
> [`anamnesis-mcp`](https://www.npmjs.com/package/anamnesis-mcp), exposing the
> `anamnesis` command — run `npx -p anamnesis-mcp anamnesis serve` for a stdio
> MCP server, or `cargo run -p anamnesis-mcp -- serve` from a checkout. See
> [`crates/anamnesis-mcp`](crates/anamnesis-mcp/README.md).

## Typed relation contract

The [`reasoning_demo`](crates/anamnesis/examples/reasoning_demo.rs) example
builds a short decision history, attaches `Reason` and `Contradicts` edges
through the public `Memory` API, and prints the resulting tension and
reasoning chain.

```text
cargo run -p anamnesis-engine --example reasoning_demo
```

It runs offline with a deterministic stub embedder. The same behavior is
verified end-to-end in
[`tests/reasoning_advantage.rs`](crates/anamnesis/tests/reasoning_advantage.rs).

## Benchmarks

The repository includes hermetic regression fixtures and optional dataset
harnesses for retrieval, context rendering, and answer evaluation. Benchmark
labels are available only to the evaluation layer; formation, retrieval, and
product context rendering receive label-free inputs.

Protocols, metric definitions, and reproducibility requirements are documented
in [`docs/07-quality-gates/benchmarks.md`](docs/07-quality-gates/benchmarks.md).
Generated datasets, model weights, caches, and run reports are not committed.

## Quick Start

> The current core implements ingest, query, forgetting, snapshots, and unified
> search. The release gate below defines the supported validation surface.

Add to your `Cargo.toml`:

```toml
[dependencies]
# Published as `anamnesis-engine` — the crates.io name `anamnesis` belongs to an
# unrelated crate. The library is still imported as `anamnesis` (`use anamnesis::…`).
# Optional: local embedding provider (downloads model on first use, ~100-500 MB)
anamnesis-engine = { version = "0.22", features = ["embed"] }
```

```rust,no_run
use anamnesis::Memory;
use anamnesis::engine::Timestamp;

// 1. Open a persistent Memory (feature = "embed" wires in bge-base-en-v1.5)
let mut mem = Memory::open("my-memory.db").unwrap();

// 2. Add conversational turns through the canonical framework recipe
let now = Timestamp::now();
mem.add("session-1", "Alice", "I prefer dark mode", now).unwrap();
mem.add("session-1", "Bob",   "Got it, dark mode it is", now).unwrap();

// 3. Search (auto-flushes pending buffers before querying)
let recall = mem.search("display preferences", 5).unwrap();
for hit in &recall.hits {
    println!("{:.3}  {}", hit.score, hit.text);
}

// 4. Reinforce what was actually used (commit-gated Hebbian strengthening)
mem.used(recall).unwrap();
```

**Use `Memory`** — it is the canonical consumer recipe. Drop to
**`Engine`** (the kernel API) only when you need custom node/edge types, your own
ingest representation, or direct control over `link` / `crystallize` / `tick`.
`Memory` is built entirely on `Engine`'s public API: anything it does, you can do.

```rust,no_run
// Framework API (default)
use anamnesis::Memory;

// Kernel API (custom encoding / raw control)
use anamnesis::engine::{Engine, EngineConfig, Observation, ConfidenceLevel};
```

For direct `Engine` usage see the [API Surface](#api-surface) section and [`docs/`](docs/README.md).

## Core Concepts

<details>
<summary><strong>Indexes Trigger; Graph Remembers</strong></summary>

<br>

Anamnesis separates retrieval cues from memory representation. Keyword search, BM25-style full-text search, entity tags, temporal filters, and optional embeddings are **trigger indexes**: they find candidate `NodeId`s that may start recall.

The actual memory is the graph: nodes, typed edges, salience, timestamps, validity windows, and origin metadata. Once a cue finds a seed, spreading activation reconstructs the surrounding structure: what it supports or contradicts, and why a decision was made.

```text
query
  -> keyword / BM25 / embedding / entity / time triggers
  -> candidate seed nodes
  -> graph spreading activation
  -> knowledge + memories + tensions
```

This means indexes can be rebuilt or replaced without changing memory. The graph remains the source of truth.

</details>

<details>
<summary><strong>Fragments, Not Summaries</strong></summary>

<br>

Replacing source turns with compact facts can omit reasoning, context, and
rejected alternatives. Those omissions cannot be reconstructed from the compact
record alone.

Anamnesis preserves **individual conversation turns as nodes**. Each retains original content, temporal position, entity references, and origin metadata. Summaries are emergent — they arise when repeated patterns consolidate into higher-level semantic nodes. The raw fragments remain.

</details>

<details>
<summary><strong>Knowledge Types</strong></summary>

<br>

Every node carries a `KnowledgeType`. The set is deliberately small — four variants that the retrieval pipeline treats differently:

| Type | Role |
|:-----|:-----|
| `Episodic` | A specific event or conversation turn — timestamped, high-fidelity. |
| `Semantic` | A distilled fact or generalization — the windowed view over episodics, and the target of consolidation. |
| `Identity` | Stable retrieval anchors and operating principles. Routed to a dedicated partition in the context package and used as a retrieval prior. |
| `Custom(String)` | An open escape hatch for consumer-defined categories, rendered by its bare label. |

`Identity` nodes bias recall as a prior but do **not** hide contradictory facts or replace a system prompt; the consumer decides how retrieved identity fragments are exposed to an LLM. (The kernel populates the identity partition only when `Identity`-typed nodes exist; the default `Memory` recipe emits `Episodic` + `Semantic`, so most consumers never write one.)

</details>

<details>
<summary><strong>Scoped Knowledge</strong></summary>

<br>

Every node carries `Origin` metadata: `peer_id`, `session_id`, a `scope` path, and `confidence`.

- A scope path such as `work/company-a` or `personal-projects/anamnesis` marks the domain a memory belongs to.
- `universal` scope means the memory participates across scopes.
- Scoped memories can be crystallized upward: session evidence can become project knowledge, which can become universal principles. The original scoped memories remain as evidence via `ConsolidatedFrom` edges; promotion is additive, not destructive.

`ScopePath` is an opaque string with a `universal` flag; current scope scoring
uses exact/universal compatibility rather than inferring a hierarchy from path
text. Consumers resolve any external authorization hierarchy before calling the
engine.

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

Tiers are **salience ranges**, not separate stores. Reinforcement and dissipation naturally distribute nodes; the tier is a display label derived from salience, not a manual setting:

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

</details>

### Product Shape

| Concern | Anamnesis contract |
|:--|:--|
| Storage unit | Persisted [source fragments](docs/01-system-architecture/ingestion-layers.md) plus source-grounded derived records |
| Retrieval | Lexical/vector alignment, cognitive graph activation, local reranking, and source-aware selection |
| Memory lifecycle | [Power-law decay and use-driven revival](docs/07-quality-gates/fidelity-results.md) |
| Relationships | Typed graph edges with contradiction preserved as tension |
| Evidence authority | Derived records retain exact provenance and never overwrite raw sources |
| Management | `update`, `forget`, `supersede`, `list`, and `get` |
| Core boundary | No LLM calls, networking, session orchestration, or provider-specific answer policy |

### Runtime Boundary

Anamnesis is a local cognitive memory engine rather than an answer generator.
Its core retrieval path is deterministic for fixed graph state and provider
outputs. Embeddings provide cues; the graph retains association, access history,
typed relationships, temporal validity, and contradiction.

Once persisted, raw fragments survive formation so a failed or revised
extraction can be replayed against the same source. Optional extraction and
reflection belong to consumer boundaries. [ADR-0015](docs/adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
specifies a proposed evidence-complete extension based on source-grounded facts,
relations, and post-selection source hydration.

## Architecture

Anamnesis exposes two API surfaces: the **Framework API** ([`anamnesis::memory::Memory`](https://docs.rs/anamnesis-engine/latest/anamnesis/memory/struct.Memory.html)) and the **Kernel API** ([`anamnesis::engine`](https://docs.rs/anamnesis-engine/latest/anamnesis/engine/index.html)). `Memory` is the official consumer-layer default, built entirely on `Engine`'s public API. The crate root re-exports exactly three symbols — `Memory`, `Engine`, and `Error` — and nothing else.

- [Operations](docs/06-operations/operations.md) — tool usage contract, failure/recovery semantics, daemon lifecycle, all env knobs.

```
src/
├── memory/         Memory — the canonical Framework API (add/search/used/tick)
├── engine.rs       anamnesis::engine — the curated Kernel API namespace
│
├── api/            Engine implementation (ingest, query, commit, tick, …)
├── graph/          Node, Edge, Origin, scope, time, types — data + reservoirs
├── mechanics/      Pure cognitive functions, no side effects
│   ├── perception     Surprise gating — novelty, confidence, budget
│   ├── attraction     Cosine/entity coupling for cold-start edge creation
│   ├── interactions   Dissipation, Rescorla-Wagner, Oja-bounded Hebbian updates
│   ├── frustration    Contradiction stress (sigma_ij), surfaced not deleted
│   ├── energy         Query-local energy objective E(S | Q)
│   ├── projection     Reservoir ↔ bounded projection (logistic / logit)
│   └── priors         Calibrated irreducible priors (d, L, N, k, …)
├── query/          Additive directed RWR, potential field, readout, search
├── storage/        StorageAdapter trait + SqliteStorage
├── embedding/      EmbeddingProvider trait + optional FastEmbedProvider
└── snapshot/       Clone-based snapshot storage

Public surface: `anamnesis::{Memory, Engine, Error}` at the root,
`anamnesis::memory` (Framework) and `anamnesis::engine` (Kernel) namespaces.
Everything below the first two lines is implementation reached through them.
```

> The crate root re-exports exactly `Memory`, `Engine`, and `Error`. The
> implementation modules (`api`, `graph`, `mechanics`, `query`, `storage`, …)
> remain internal and are not part of the public API contract.

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
Query ── additive directed RWR from seeds ──► readout ──► budget-bounded ContextPackage
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
         │  crystallize()                         │
         │  synthesis + cross-fragment Entity links│
         └────────────────────────────────────────┘
```

</details>

<details id="api-surface">
<summary><strong>Selected API Surface</strong></summary>

<br>

The signatures below are an abridged guide to common framework and kernel
operations. Rustdoc is authoritative for the complete public API, including the
atomic-fact and reviewed-relation lifecycle.

```rust
// ── Framework API (anamnesis::memory) — the front door ──────────────────────
impl Memory {
    // Construction
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error>;            // feature = "embed"
    pub fn in_memory() -> Result<Self, Error>;                             // feature = "embed"
    pub fn with_provider(path: impl AsRef<Path>, provider: Arc<dyn EmbeddingProvider>) -> Result<Self, Error>;
    pub fn in_memory_with_provider(provider: Arc<dyn EmbeddingProvider>) -> Result<Self, Error>;

    // Ingest (canonical episodic turn + windowed semantic view)
    pub fn add(&mut self, session: &str, speaker: &str, text: &str, at: Timestamp) -> Result<AddReceipt, Error>;
    pub fn add_note(&mut self, text: &str, at: Timestamp) -> Result<AddReceipt, Error>;
    pub fn flush_session(&mut self, session: &str) -> Result<Option<NodeId>, Error>;
    pub fn flush_all(&mut self) -> Result<(), Error>;

    // Retrieval (canonical pre-packaging readout surface)
    pub fn search(&mut self, query: &str, limit: usize) -> Result<Recall, Error>;
    pub fn search_at(&mut self, query: &str, limit: usize, now: Timestamp) -> Result<Recall, Error>;
    pub fn search_result_at_with(&mut self, query: &str, limit: usize, now: Timestamp, tuning: &SearchTuning) -> Result<SearchResult, Error>;
    pub fn search_reranked(&mut self, query: &str, reranker: &dyn RerankingProvider, options: RerankedRecallOptions) -> Result<RerankedRecall, Error>;
    pub fn render_context_for_with(&self, query: &str, recall: &Recall, options: ContextRenderOptions) -> Result<String, Error>;

    // Reinforcement & time
    pub fn used(&mut self, recall: Recall) -> Result<CommitReport, Error>;
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    // Bounded k-hop subgraph export (nodes + induced edges + per-node depth) — dashboard/graph-viz consumers
    pub fn subgraph(&self, seeds: &[NodeId], depth: usize, node_budget: usize) -> Result<Subgraph, Error>;

    // Escape hatch — drop to the kernel on the same store
    pub fn engine(&self) -> &Engine;
    pub fn engine_mut(&mut self) -> &mut Engine;
}

// ── Kernel API (anamnesis::engine) — the raw substrate ──────────────────────
impl Engine {
    // Construction
    pub fn new() -> Self;
    pub fn with_config(config: EngineConfig) -> Self;
    pub fn with_storage<S: StorageAdapter + Clone>(config: EngineConfig, storage: S) -> Self;

    // Snapshots
    pub fn snapshot(&mut self, label: &str) -> Result<SnapshotId, Error>;
    pub fn restore(&mut self, id: &SnapshotId) -> Result<(), Error>;
    pub fn list_snapshots(&self) -> Vec<(SnapshotId, String, Timestamp)>;

    // Core operations
    pub fn ingest(&mut self, observation: Observation) -> Result<IngestResult, Error>;
    pub fn crystallize(&mut self, request: CrystallizeRequest) -> Result<CrystallizeResult, Error>;
    pub fn link(&mut self, from: NodeId, to: NodeId, edge_type: EdgeType) -> Result<EdgeId, Error>;
    pub fn touch(&mut self, node_id: NodeId, now: Timestamp) -> Result<(), Error>;
    pub fn tick(&mut self, now: Timestamp) -> Result<TickReport, Error>;

    // Query — returns structured context for LLM consumption
    pub fn query(&self, query: &Query, config: &QueryConfig) -> Result<ContextPackage, Error>;
    pub fn search(&self, input: SearchInput) -> Result<SearchResult, Error>;

    // Commit — write-back for the retrieval loop: reinforces the memories actually
    // used and strengthens co-used edges (commit-gated Hebbian). Read-only query
    // changes nothing; touch()/tick() also mutate reservoirs by other paths.
    pub fn commit(&mut self, package: ContextPackage, feedback: Option<ConfidenceLevel>)
        -> Result<(ContextPackage, CommitReport), Error>;
}
```

</details>

<details>
<summary><strong>Selected Storage Abstraction</strong></summary>

<br>

This is an abridged view of the graph-oriented methods. The complete
`StorageAdapter` contract, including atomic-fact and reviewed-relation methods,
is documented in rustdoc and the [storage design](docs/03-persistence/storage.md).

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

CI also runs the MSRV check (`cargo check --all-targets --all-features` on Rust 1.88), `cargo deny`, and PR semver checks. Run those locally when the corresponding tools are installed, especially before publishing a release.

`cargo test --all-targets` intentionally is not a release gate because this crate has `harness = false` benchmark binaries that execute long-running benchmarks when invoked as test targets. Use `cargo bench` or the manual benchmark workflow for performance runs.

## Proposed Evidence-Complete Architecture

[ADR-0015](docs/adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
specifies a proposed additive architecture: a source-grounded entity/fact catalog,
typed evidence relations, bounded chain retrieval, post-selection raw-source
hydration, adaptive packaging, and optional consumer-side reflection and
multimodal formation. It is explicitly `Proposed`; the current graph and atomic
fact APIs remain authoritative until every migration, provenance, parity,
regression, scale, and latency gate in that ADR passes.

## Status

**Current main (after v0.22.0)** — canonical production reranked recall, an
isolated grounded atomic-fact sidecar with reviewed typed routing relations,
bounded query and relation expansion for complex queries, and source-aware
coverage selection. The engine remains LLM-free; configured extraction
stays in the daemon/consumer layer and routes only to live, scope-valid raw
sources. See the [CHANGELOG](CHANGELOG.md) for release history and exact
migrations.

## References

- Pavlik & Anderson — *Practice and Forgetting Effects on Vocabulary Memory: An Activation-Based Model of the Spacing Effect* (2005)
- Anderson & Schooler — *Reflections of the Environment in Memory* (1991)
- Collins & Loftus — *A Spreading-Activation Theory of Semantic Processing* (1975)
- Tulving — *Episodic and Semantic Memory* (1972)

## License

[MIT](LICENSE)
