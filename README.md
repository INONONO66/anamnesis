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
  <a href="#use-in-claude-code--codex">Claude Code &amp; Codex</a> · <a href="#see-it-reason">See it reason</a> · <a href="#quick-start">Quick Start</a> · <a href="docs/README.md">Docs</a> · <a href="docs/00-foundation/vision.md">Vision</a>
</p>

---

> Named after Plato's theory of **anamnesis** (ἀνάμνησις) — the soul already possesses knowledge; learning is recollection triggered by the right cue.

## Why

Every LLM agent session starts from zero. Agents repeat mistakes, rediscover conventions, and lose the reasoning behind past decisions. The common answers each drop something:

- **Vector stores** answer *"what was said"* but not *"why it was decided"* — the structure between facts is gone.
- **Tiered memory** archives conversations but loses cross-session connections.
- **Evolving playbooks** improve over time but suffer brevity bias — detail erodes with each rewrite.

Anamnesis stores memory as a **graph of fragments connected by typed edges** — so a decision keeps the reason it was made, and a reversal keeps the decision it overturned. Retrieval is a hybrid: alignment scoring (keyword / embedding) finds the entry points, and the graph surfaces the *structure* around them — reasoning chains and contradictions a flat list can't express — while power-law forgetting keeps the store from growing without bound.

## What

Anamnesis is a **Rust library — a memory kernel** — plus a ready-made **Claude Code / Codex plugin** that drives it for a coding agent. It is not a service: the core owns storage, retrieval, forgetting, and contradiction handling, and leaves extraction and serving to the consumer.

| Mechanic | What it does |
|:---------|:-------------|
| **Associative recall** | Additive directed **random-walk-with-restart (RWR)** spreads activation from query seeds along typed edges; converging evidence sums (never max), so a fragment reachable by several paths ranks above one reachable by one. |
| **Conductance** | Edges hold an associative-strength reservoir (a log-likelihood-ratio); committed co-use strengthens links via an Oja-bounded Hebbian update. |
| **Forgetting** | Node strength `A_i = B_i + P_i`: `B_i` is the ACT-R **base-level** activation over the access-trace history, where each trace decays at an **activation-dependent rate** (Pavlik & Anderson 2005) — so spaced repetition outlasts massed (the **spacing effect**). `P_i` is a decay-exempt **evidence prior** (encoding surprise, feedback). Use raises `B_i`; disuse fades it — never deleted. |
| **Perception** | **Surprise-gated** input: an observation charges memory in proportion to prediction error, then novelty / confidence / budget decide whether it allocates a new site or routes to the nearest one. |
| **Frustration** | Contradictions are **excluded from propagation** and surfaced as tension (`sigma_ij`), never overwritten — both sides keep their provenance. |

The earned claim is narrow and real: **typed reasoning edges plus contradiction-as-tension expose structure a flat store cannot** — see [See it reason](#see-it-reason) below. Ranking itself is dominated by alignment scoring; the graph's contributions are structure surfacing and principled forgetting, not magic relevance.

> **Reservoirs vs projections** ([ADR-0002](docs/adr/0002-reservoir-projection-state.md), [ADR-0008](docs/adr/0008-powerlaw-dissipation.md)): per node, the persistent state is the bounded access-trace history (which drives the base level `B_i`, recomputed on demand and never stored) plus a decay-exempt evidence prior `P_i`; per edge, `conductance` is an unbounded log-LR reservoir. The public `salience = logistic(B_i + P_i)` / `weight` in `[0, 1]` are bounded `logistic` projections, refreshed by the write paths (`ingest`, `link`, `touch`, `commit`, `crystallize`, `tick`). The invariant is that **read-only retrieval (`query` / `search`) never mutates persistent state** — it changes only through explicit writes and time.

> **See it work** → [cognitive-fidelity results](docs/07-quality-gates/fidelity-results.md): charts of power-law forgetting, the spacing effect (with its retention-interval crossover), and the fan effect — produced by the engine itself, from the same paradigms the CI gate asserts.

## What it is not

- **Not a vector database.** Retrieval uses hybrid alignment (text + embedding + entity + temporal) plus graph-surfaced structure; if you only need top-k similarity, use a vector store — it will be simpler and faster.
- **Not a cloud memory API.** Local-first single binary; your memories live in a SQLite file you own. There is no hosted service and none is planned.
- **Not a QA system.** The benchmark numbers are retrieval recall over the LongMemEval / LoCoMo corpora, not answer accuracy; anamnesis returns memories and structure, the agent does the answering.
- **Not multi-tenant / multi-agent (yet).** One graph per namespace, one writer; peer provenance and trust weighting are roadmap (see [ADR-0014](docs/adr/0014-shrink-to-product.md)).
- **Not a replacement for project files.** Conventions and specs that belong in your repo (CLAUDE.md, docs) should stay there; anamnesis holds what emerges from conversations — decisions, contradictions, lessons, context.

## Success criteria

What "working" means for a memory engine, in observable terms — check yours with the `stats` tool (usage section):

1. **Recall earns its injection.** Session-start and per-prompt recalls surface prior decisions the agent actually builds on (the τ gate keeps irrelevant memory out; a reinforcing `recall` / `relate` after use is the signal it helped).
2. **Capture keeps up.** The extraction backlog drains within a few sessions (`extraction backlog` low relative to `captured total`); raw turns are never lost (fail-open, redelivery).
3. **The graph stays structured.** Contradictions surface as tensions instead of silently coexisting; `why`-chains are traceable (`relate` edges accumulate alongside captured turns).
4. **Forgetting works.** Stale ratio stays bounded as the graph grows — old, unused memories sink (archival) instead of drowning recall.

## Use in Claude Code & Codex

The most common way to run Anamnesis: **persistent associative memory for a
coding agent.** The plugin wires Anamnesis into Claude Code (and Codex) as
**activation-gated recall** — `SessionStart` seeds a few high-salience project
memories, and every `UserPromptSubmit` injects a read-only spreading-activation
recall **only when the top activation clears a threshold**, so an off-topic
prompt injects nothing. It is **install-and-go**: the plugin carries both the
hooks and the agent MCP tools and fetches the matching native binary from the
GitHub Release on first use — no `cargo`, no `npm`, no separate binary step.

**Claude Code** — add the marketplace, install, reload:

```text
/plugin marketplace add INONONO66/anamnesis
/plugin install anamnesis@anamnesis-plugins
/reload-plugins
```

That is the whole setup. You get proactive recall (5 hooks) and all 11 agent
MCP tools:

| Surface | What ships |
|:--|:--|
| **Hooks** | `SessionStart` (seed recall + extraction nudge), `UserPromptSubmit` (gated recall), `Stop` / `PreCompact` / `SessionEnd` (passive turn capture) |
| **MCP tools** | The 11 tools in the inventory below. |

### MCP tool inventory

This is the authoritative inventory of tools registered by the MCP server:

| Tool | Purpose |
|:--|:--|
| `recall` | Search memory for relevant prior knowledge. |
| `remember` | Store a distilled insight, decision, or lesson. |
| `ingest_conversation` | Ingest an ordered conversation transcript. |
| `relate` | Link two remembered nodes with a typed reasoning relation. |
| `stats` | Report read-only graph health and size statistics. |
| `extract_pending` | Retrieve un-extracted conversation turns for reasoning extraction. |
| `update` | Edit an existing memory's content. |
| `forget` | Soft-delete or permanently erase a memory. |
| `supersede` | Mark a newer memory as superseding an older one. |
| `list` | List memories by salience with optional filters. |
| `get` | Read one memory's full detail by node ID. |

**Automatic capture.** Beyond on-demand `remember`, the plugin captures
the session on its own in two stages. **Stage 1** is passive: `Stop`,
`PreCompact`, and `SessionEnd` hooks stream each turn to Anamnesis as raw
`Episodic` memories — fire-and-forget, content-hash-deduped, and it never blocks
a prompt. **Stage 2** is agent-driven extraction: once the un-extracted queue
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

## See it reason

Vector search returns *a list*. The thing a flat store cannot represent is the
*structure between* the results — that turn A was **reversed** by turn B, and
**why** each was chosen. The [`reasoning_demo`](crates/anamnesis/examples/reasoning_demo.rs)
example makes that concrete: a short conversation decides on Postgres (recording
the reason with a `Reason` edge), then reverses to SQLite (a `Contradicts` edge
back to the decision). One query — *"why did we switch databases?"* — is then
answered two ways over the **same nodes**.

```text
cargo run -p anamnesis-engine --example reasoning_demo
```

Graph recall surfaces the contradiction as a **tension** and walks the reasoning
chain by typed edge:

```text
=== graph recall (structure: tensions + reasons) ===

tensions (contradictions surfaced, never suppressed):
  #5 ⟂ #11  (stress 0.03)
    ↳ assistant: Decision: we go with Postgres.
    ↳ assistant: We are reverting to SQLite — the ops overhead is too high ...

why-chain from the reversal (typed edges):
  reversal --because--> assistant: SQLite keeps the single-node deploy simple ...
  reversal --contradicts--> assistant: Decision: we go with Postgres.
```

Ranking the same turns by raw cosine to the query gives a bare list — the
conflict and the why-chain are gone:

```text
=== flat vector ranking (cosine to the query) ===

a list with no structure — the contradiction and the why-chain are invisible:

  1.000  assistant: Postgres because we need JSONB and row-level security.
  1.000  assistant: We are reverting to SQLite — the ops overhead is too high ...
  0.999  assistant: SQLite keeps the single-node deploy simple ...
```

The claim is narrow: **typed reasoning edges plus contradiction-as-tension
expose structure a flat store cannot.** The demo runs offline with a
deterministic stub embedder (no model download); the same behaviour is asserted
end-to-end in [`tests/reasoning_advantage.rs`](crates/anamnesis/tests/reasoning_advantage.rs).

## Benchmarks

Long-term conversational memory benchmarks, **retrieval-only dry runs**: no LLM
anywhere — ingest is raw turns + embeddings (`bge-base-en-v1.5`), retrieval is
the engine's deterministic pipeline. Reproducible via
`cargo bench --features embed --bench real_memory` (see
[calibration records](docs/07-quality-gates/calibration-records.md) for full
provenance, ablations, and negative results). These numbers are measured through
the `Memory` framework API exactly as shipped — the benchmark harness builds and
queries via `Memory`.

| Benchmark | Gold granularity | Recall@20 | MRR | NDCG@20 | p50 |
|:--|:--|--:|--:|--:|--:|
| **LongMemEval-S** (full official split, 500 q, all 6 types) | session-level | **93.8%** | **0.872** | 0.808 | ~17 ms |
| **LoCoMo** (full non-adversarial, 1540 q) | turn-level strict | **77.6%** | **0.291** | 0.386 | ~21 ms |

Read these numbers for what they are — and, importantly, for what they are **not**. On these cold-start dry runs the ranking is carried by alignment scoring (keyword + embedding); the graph's spreading activation is a modest re-rank, not the source of the score. What the graph earns here is not measured by these tables — it is the [structure surfacing](#see-it-reason) and the [forgetting dynamics](docs/07-quality-gates/fidelity-results.md). No baselines or comparison tables are added; these are the shipped harness's own numbers.

- **Retrieval metrics, not answer accuracy.** Published memory-system scores
  (Mem0, Zep, LangMem, …) are LLM-as-judge *answer* scores — a different
  measurement. These numbers bound what an answer stage could see in context
  (LoCoMo hit@20 = 84.6%; LongMemEval hit@1 = 82.6%, hit@20 = 98.0%).
- **No usage learning is measured here.** Runs are cold-start (no `commit`
  warmup), so the readout calibration intentionally zeroes the salience
  coefficient (`w_s = 0`) — on unused memory, salience carries only
  encoding-time noise. Deployments that accumulate real usage should refit it
  per [ADR-0010](docs/adr/0010-calibrated-priors-not-laws.md); the
  reinforcement dynamics themselves are validated by the
  [cognitive-fidelity gates](docs/07-quality-gates/fidelity-results.md), not
  by these benchmarks.
- Readout coefficients were fit on the even-sample half of LoCoMo and
  validated on the held-out half (dev-half, never seen by the fit: Recall@20 /
  MRR 0.778 / 0.287 on unseen conversations); LongMemEval numbers use the same
  weights with zero dataset-specific tuning.

## Quick Start

> **Anamnesis is in active development.** The core engine is functional — ingest, query pipeline, forgetting, snapshots, and unified search are all operational.

Add to your `Cargo.toml`:

```toml
[dependencies]
# Published as `anamnesis-engine` — the crates.io name `anamnesis` belongs to an
# unrelated crate. The library is still imported as `anamnesis` (`use anamnesis::…`).
# Optional: local embedding provider (downloads model on first use, ~100-500 MB)
anamnesis-engine = { version = "0.20", features = ["embed"] }
```

```rust,no_run
use anamnesis::Memory;
use anamnesis::engine::Timestamp;

// 1. Open a persistent Memory (feature = "embed" wires in bge-base-en-v1.5)
let mut mem = Memory::open("my-memory.db").unwrap();

// 2. Add conversational turns — the bench recipe runs automatically
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

**Use `Memory`** — it is the validated, bench-proven consumer recipe. Drop to
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

Existing systems summarize conversations into compact facts — lossy by design. The reasoning, context, and rejected alternatives are discarded.

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

`ScopePath` is an opaque string with a `universal` flag; scope scoring is a two-branch weight (universal vs. non-matching). A richer scope hierarchy is on the [roadmap](#roadmap), not in the shipped engine.

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

When a new agent session starts, it inherits not rules but *judgment*.

</details>

### How It Compares

| | Storage Unit | Retrieval | Decay | Relationships | Reasoning | Management |
|:--|:--|:--|:--|:--|:--|:--|
| Mem0 | Extracted facts | Embedding similarity | None | None | Facts only | Add/update/delete (LLM-mediated) |
| Letta | Conversation history | Text search | Archive tier | Basic | Session recall | — |
| Stanford ACE | Monolithic playbook | Full load | Curator rewrite | None | Strategy-level | — |
| **Anamnesis** | **[Fragments](docs/01-system-architecture/ingestion-layers.md)** | **Alignment + graph** | **[Decay + revival](docs/07-quality-gates/fidelity-results.md)** | **[Typed edges](crates/anamnesis/examples/reasoning_demo.rs)** | **Full chains** | **[update/forget/supersede/list/get](CHANGELOG.md#0120---2026-07-04)** |

### Positioning

**vs mem0** — mem0 is add-only: memories accumulate with no time-based decay or salience mechanism, so a long-running store only grows. Anamnesis ages every node through power-law dissipation with reinforcement-driven revival — access resurrects a decayed node instead of it staying stale forever; see the [cognitive-fidelity results](docs/07-quality-gates/fidelity-results.md), produced by the engine itself. Retrieval and extraction are the second structural gap: Anamnesis makes no per-operation LLM calls in its core (see [Design Principles](#design-principles), "No LLM calls") — no API key, no per-query inference cost, no cloud round-trip; when extraction happens, it piggybacks on the *consumer agent's own* in-loop LLM call rather than a separate paid one (see [Use in Claude Code & Codex](#use-in-claude-code--codex), Stage 2). Conflicting facts are held as tension, not silently overwritten: the [`reasoning_demo`](crates/anamnesis/examples/reasoning_demo.rs) example and [ADR-0006](docs/adr/0006-frustration-not-deletion.md) show a reversed decision surfaced as a `Contradicts` edge, both sides keeping their provenance. Storage is local-first — a SQLite file you own, no hosted service (see [What it is not](#what-it-is-not)). Raw fragments are preserved rather than collapsed into an LLM summary at ingest — the storage mechanism is lossless and formation (what to distill, and when) is a swappable consumer-layer policy, not a step baked into the core (see [ingestion layers](docs/01-system-architecture/ingestion-layers.md)). **New in 0.12.0**: a full agent-facing memory-management surface — `update`, `forget` (soft-retract or hard delete), `supersede`, `list`, `get` — plus per-namespace extraction-queue isolation so captured turns from one project no longer leak into another's backlog (see [CHANGELOG 0.12.0](CHANGELOG.md#0120---2026-07-04)).

**vs RAG pipelines** — Anamnesis makes zero LLM calls in its core. Retrieval is deterministic (alignment scoring plus graph traversal), not an inference call on every query. No embedding drift on the graph itself; embeddings are cues, not the memory.

**vs LLM context documents** — Context docs require manual compilation, suffer brevity bias on every rewrite, and have no mechanism for forgetting or contradiction detection. Anamnesis handles all three: power-law dissipation ages out stale knowledge, spreading activation surfaces related fragments, and `Contradicts` edges surface tensions in query results.

**vs vector-only stores** — Embedding similarity finds *similar* fragments and does the heavy lifting on ranking. Anamnesis adds what similarity alone cannot represent: the typed reasoning chains (causes, contradictions, decisions, confirmations) *between* fragments, surfaced as structure in the result.

## Architecture

Anamnesis exposes two API surfaces: the **Framework API** ([`anamnesis::memory::Memory`](https://docs.rs/anamnesis-engine/latest/anamnesis/memory/struct.Memory.html)) and the **Kernel API** ([`anamnesis::engine`](https://docs.rs/anamnesis-engine/latest/anamnesis/engine/index.html)). `Memory` is the official consumer-layer default, built entirely on `Engine`'s public API. The crate root re-exports exactly three symbols — `Memory`, `Engine`, and `Error` — and nothing else.

- [Operations](docs/06-operations/operations.md) — tool usage contract, failure/recovery semantics, daemon lifecycle, all env knobs.

```
src/
├── memory/         Memory — the Framework API (bench-proven recipe: add/search/used/tick)
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

> The top-level module tree above (`api`, `graph`, `mechanics`, `query`, `storage`, …) is the **real implementation tree** — the crate compiles against it and it carries hundreds of internal references. What changed at the two-door boundary ([v0.7](#status)) is only what is *re-exported at the root*: exactly `Memory`, `Engine`, `Error`. The module paths remain internal, not a public API.

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
<summary><strong>API Surface</strong></summary>

<br>

```rust
// ── Framework API (anamnesis::memory) — the front door ──────────────────────
impl Memory {
    // Construction
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error>;            // feature = "embed"
    pub fn in_memory() -> Result<Self, Error>;                             // feature = "embed"
    pub fn with_provider(path: impl AsRef<Path>, provider: Arc<dyn EmbeddingProvider>) -> Result<Self, Error>;
    pub fn in_memory_with_provider(provider: Arc<dyn EmbeddingProvider>) -> Result<Self, Error>;

    // Ingest (bench recipe: episodic turn + windowed semantic view)
    pub fn add(&mut self, session: &str, speaker: &str, text: &str, at: Timestamp) -> Result<AddReceipt, Error>;
    pub fn add_note(&mut self, text: &str, at: Timestamp) -> Result<AddReceipt, Error>;
    pub fn flush_session(&mut self, session: &str) -> Result<Option<NodeId>, Error>;
    pub fn flush_all(&mut self) -> Result<(), Error>;

    // Retrieval (readout surface — what the benchmarks measure)
    pub fn search(&mut self, query: &str, limit: usize) -> Result<Recall, Error>;
    pub fn search_at(&mut self, query: &str, limit: usize, now: Timestamp) -> Result<Recall, Error>;
    pub fn search_result_at_with(&mut self, query: &str, limit: usize, now: Timestamp, tuning: &SearchTuning) -> Result<SearchResult, Error>;

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

## Roadmap

These are **not yet implemented**. They are recorded here so the map matches the territory; several were deliberately removed in the [v0.10.0 shrink](docs/adr/0014-shrink-to-product.md) because they had no consumer, and will return only behind a real one:

- **Multi-peer provenance & trust** — the `PeerId` / `SourceKind` fields on `Origin` persist, but the peer registry, trust levels, and the readout trust term (now a neutral `1.0`) were removed. A multi-agent deployment that actually attributes and weights sources by trust is the re-add condition.
- **Identity tiers** — the collapsed `KnowledgeType` keeps a single `Identity` variant; the `IdentityCore` / `IdentityLearned` / `IdentityState` split (with per-tier decay policy) is future work.
- **Scope hierarchies** — `ScopePath` is currently an opaque string with a `universal` flag. Ancestor/sibling scope scoring and upward crystallization across a real hierarchy are roadmap.
- **Debug / hypothesis lifecycle** — the start-debug / log-hypothesis / rejected-hypothesis machinery was removed as consumer-less; a first-class reasoning-session capture may return through the [capture pipeline](docs/adr/0013-reasoning-capture-pipeline.md).

See [ADR-0014](docs/adr/0014-shrink-to-product.md) for the full shrink record — what was removed, why, and the condition for each to return.

## Status

**v0.10.x** — external-review fixes (0.10.1: doc drift, v8 bare-type normalization, tension-endpoint trimming exemption, corpus-independent demo baseline) and ops hardening (0.10.2: usage metrics in `stats`, [operations contract](docs/06-operations/operations.md), [migration policy](docs/03-persistence/migration-policy.md), flake-class fixes).

**v0.10.0** — **shrink to product** ([ADR-0014](docs/adr/0014-shrink-to-product.md)). An audit found ~85% of the Engine's public surface had zero consumers — the map sold more than the territory walked. This release removes the debug/hypothesis lifecycle, the peer/trust subsystem, a large convenience API, manual memory-tier override, and the scope-relation hierarchy; collapses `KnowledgeType` from 15 variants to 4 (`Episodic` / `Semantic` / `Identity` / `Custom`); and discloses a set of by-design decay/tau coarsenings. `PeerId` storage, tier *display*, and the internal module tree survive. Breaking vs 0.9. Migrations run automatically on open (v5→v6 drops peers; v6→v7 normalizes legacy node types). See the [CHANGELOG](CHANGELOG.md) and ADR-0014.

**v0.9.x** — automatic capture pipeline ([ADR-0013](docs/adr/0013-reasoning-capture-pipeline.md)): `Stop` / `PreCompact` / `SessionEnd` hooks stream turns as raw `Episodic` memories; a Stage-2 nudge asks the agent to distill them via `extract_pending`. Capture hardening (queue durability, nudge ungating, bounded I/O) in 0.9.1.

**v0.8.x** — published to crates.io as **`anamnesis-engine`**; ships the Claude Code & Codex plugin (activation-gated recall) and the MCP-free internal transport ([ADR-0012](docs/adr/0012-daemon-core-mcp-plugin-clients.md)). Codex MCP-launch fixes in 0.8.1 / 0.8.2.

**v0.7.0** — two-door public API surface: root re-exports exactly `Memory`, `Engine`, `Error`; `anamnesis::engine::*` is the full kernel namespace; `anamnesis::memory::*` is the framework namespace. The top-level modules (`api`, `graph`, `mechanics`, `query`, `snapshot`, `storage`, `embedding`, `error`) are the real internal tree, doc-hidden at the root boundary.

**v0.6.0** — retrieval overhaul: alignment-only readout potential, ADR-0010 calibrated readout coefficients, `SearchTrace.readout` diagnostics, temporal query cues, and `Balanced` packaging.

**v0.5.0** — migrated to the **conductive-network** model: additive directed RWR, log-odds reservoirs with bounded projections, power-law dissipation, commit-gated Hebbian learning, and frustration. Node strength decomposed as `A_i = B_i + P_i` ([ADR-0008](docs/adr/0008-powerlaw-dissipation.md)): the ACT-R base level `B_i` is recomputed on demand from the access-trace history, and the persistent evidence prior `P_i` is decay-exempt.

## References

- Pavlik & Anderson — *Practice and Forgetting Effects on Vocabulary Memory: An Activation-Based Model of the Spacing Effect* (2005)
- Anderson & Schooler — *Reflections of the Environment in Memory* (1991)
- Collins & Loftus — *A Spreading-Activation Theory of Semantic Processing* (1975)
- Tulving — *Episodic and Semantic Memory* (1972)
- Stanford ACE — *Agentic Context Engineering* (ICLR 2026)
- Anthropic — *Effective Context Engineering for AI Agents* (2025)

## License

[MIT](LICENSE)
