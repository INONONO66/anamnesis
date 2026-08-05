# Anamnesis Technical Specification

Anamnesis is a Rust library that preserves LLM-agent conversations and work
experience as a graph, then retrieves relevant knowledge as context when a cue
is provided. This directory is the public technical specification.

Architecture chapters describe the current contract. Proposed changes are
identified explicitly and live in an ADR until accepted. Quality-gate records
document protocols, results, and evidence availability rather than product
design. If a document
disagrees with code, code is authoritative and the document must be corrected.

## One-Line Definition

> A cognitive memory engine based on spreading activation (ACT-R): cues activate related memories and spread through them, with that activation flow expressed through the intuition of a path-dependent conductive network.

## Design Summary

- **Fragment first.** Successfully ingested conversation turns and document
  fragments remain available as source content; derived records do not replace
  them.
- **The theory is spreading activation.** Cues activate related memories and spread through associations. Conductance is associative strength (`log-LR`); the conductive-network frame is the representation, not a separate theory.
- **Retrieval is perturbation.** A query imposes a semantic potential field on the graph; retrieval is the resulting flow and readout.
- **Use leaves work behind.** Only fragments and paths actually consumed as context are integrated as committed work, lowering impedance for future retrieval.
- **Forgetting is activation-dependent multi-trace decay (Pavlik & Anderson 2005).** Unused sites are not deleted. Their base-level activation `B_i = ln( Σ_j (now − t_j)^(−d_j) )` falls as access traces age (power-law), where each trace's decay rate `d_j = m_type·(c·e^{m_j} + α)` is computed once from current activation at trace creation. This makes the spacing effect emerge: spaced re-presentation lands at low activation → low `d_j` → durable strength, while massed re-presentation lands at high activation → high `d_j` → fast decay. A committed access appends a fresh trace and lifts `B_i` back up.
- **Contradiction is frustration.** Conflicting knowledge is preserved as constraint stress and surfaced in context instead of being hidden or erased.
- **Scope and origin are required.** Every site carries producer, session,
  scope, and confidence metadata. Scope influences applicability and ranking;
  authorization remains a consumer responsibility.
- **Storage is swappable.** The public engine runs over a storage trait. SQLite is the default adapter.

## Stack At A Glance

| Area | Choice | Reason |
|---|---|---|
| Language | Rust 2024 | Strong library boundary, type safety, synchronous core API |
| Default storage | SQLite | Embedded use, durable file mode, easy testing |
| Optional embeddings | External provider trait | Keeps model choice and download policy outside the engine |
| Public API | Synchronous `Memory` framework and `Engine` kernel surfaces | Easy for LLM runtimes and CLIs to wrap |
| Graph model | Typed site / typed edge | Fixes retrieval, dissipation, contradiction, and scope policy in data types |
| Querying | `search` + `query` | Combines text, vector, and activation flow into one context package |

## Document Index

### 00 - Foundation

- [vision.md](00-foundation/vision.md) - product purpose, audience, design direction
- [goals-nongoals.md](00-foundation/goals-nongoals.md) - scope, non-scope, completion criteria
- [glossary.md](00-foundation/glossary.md) - terms and symbols

### 01 - Architecture

- [overview.md](01-system-architecture/overview.md) - system boundary, main flows, public surface
- [framework-layer.md](01-system-architecture/framework-layer.md) - Framework API (`Memory`): recipe, buffering semantics, and shared direct/MCP/plugin recall surface
- [ingestion-layers.md](01-system-architecture/ingestion-layers.md) - storage mechanism vs formation policy; ingestion granularity is a consumer-layer policy, not an engine property

### 02 - Knowledge Model

- [graph-model.md](02-knowledge-model/graph-model.md) - nodes, edges, types, tiers
- [evidence-model.md](02-knowledge-model/evidence-model.md) - proposed source authority, canonical facts, relations, observations, and evidence references
- [temporal-model.md](02-knowledge-model/temporal-model.md) - record time, fact time, access history
- [peer-identity.md](02-knowledge-model/peer-identity.md) - producer provenance and the neutral trust boundary
- [scoping-promotion.md](02-knowledge-model/scoping-promotion.md) - current scope ranking and proposed promotion rules

### 03 - Persistence

- [storage.md](03-persistence/storage.md) - storage trait, SQLite schema, snapshots

### 04 - Cognitive Dynamics

- [overview.md](04-cognitive-dynamics/overview.md) - conductive-network model and shared invariants
- [conductance.md](04-cognitive-dynamics/conductance.md) - relation conductance and path-dependent plasticity
- [potential-landscape.md](04-cognitive-dynamics/potential-landscape.md) - query potential and memory basins
- [frustration.md](04-cognitive-dynamics/frustration.md) - contradiction constraints and stress handling
- [dissipation.md](04-cognitive-dynamics/dissipation.md) - leakage, dissipation, retained action
- [interactions.md](04-cognitive-dynamics/interactions.md) - event model that leaves readout work behind
- [energy.md](04-cognitive-dynamics/energy.md) - subsystem stabilization objective
- [perception.md](04-cognitive-dynamics/perception.md) - input gating and initial site coupling
- [social.md](04-cognitive-dynamics/social.md) - producer provenance and explicit feedback dynamics
- [readout-scoring.md](04-cognitive-dynamics/readout-scoring.md) - selecting sites that can be read out

### 05 - Context Retrieval

- [activation-flow.md](05-context-retrieval/activation-flow.md) - activation current under a query field
- [pipeline.md](05-context-retrieval/pipeline.md) - candidate collection, flow, packaging
- [hook-triggering.md](05-context-retrieval/hook-triggering.md) - making the agent use memory: activation-gated hook strategy

### 06 - Operations

- [operations.md](06-operations/operations.md) - client lifecycle, capture and extraction, failure recovery, telemetry, and daemon operation

### 07 - Quality Gates

- [observability.md](07-quality-gates/observability.md) - health, trace, invariant telemetry
- [benchmarks.md](07-quality-gates/benchmarks.md) - performance budgets and measurement
- [calibration-records.md](07-quality-gates/calibration-records.md) - active defaults, disclosed measurements, and evidence availability

### ADR - Design Decisions

- [0001-conductive-network-substrate.md](adr/0001-conductive-network-substrate.md) - spreading activation is the theory; conductive networks are the representation
- [0002-reservoir-projection-state.md](adr/0002-reservoir-projection-state.md) - retained action and conductance are authoritative; salience and weight are bounded projections
- [0003-bayesian-magnitudes.md](adr/0003-bayesian-magnitudes.md) - magnitudes come from Bayes: `A = log need-odds`, `C = log-LR`
- [0004-query-as-field-and-commit.md](adr/0004-query-as-field-and-commit.md) - query as potential field; read-only retrieval vs committed work
- [0005-additive-activation-flow.md](adr/0005-additive-activation-flow.md) - additive activation flow (RWR), never max-path only
- [0006-frustration-not-deletion.md](adr/0006-frustration-not-deletion.md) - contradictions become frustration, not deletion or automatic judgment
- [0007-energy-objective-symmetric-caveat.md](adr/0007-energy-objective-symmetric-caveat.md) - energy is an objective; minimization is strict only under symmetric coupling
- [0008-powerlaw-dissipation.md](adr/0008-powerlaw-dissipation.md) - forgetting is power-law base-level dissipation
- [0009-surprise-gated-perception.md](adr/0009-surprise-gated-perception.md) - ingest magnitude is Bayesian surprise
- [0010-calibrated-priors-not-laws.md](adr/0010-calibrated-priors-not-laws.md) - constants are calibrated priors, not physical laws
- [0011-activation-gated-triggering.md](adr/0011-activation-gated-triggering.md) - activation-gated hook triggering, not flat-profile injection
- [0012-daemon-core-mcp-plugin-clients.md](adr/0012-daemon-core-mcp-plugin-clients.md) - daemon is the shared core; MCP and plugin are distinct clients of it
- [0013-reasoning-capture-pipeline.md](adr/0013-reasoning-capture-pipeline.md) - passive raw ingest + agent-side batch extraction
- [0014-shrink-to-product.md](adr/0014-shrink-to-product.md) - delete the consumer-less surface; shrink the API to what the product walks
- [0015-evidence-grounded-formation-and-chain-retrieval.md](adr/0015-evidence-grounded-formation-and-chain-retrieval.md) - proposed source-grounded catalog, bounded evidence chains, and adaptive packaging

## Reading Order

1. New readers should start with [vision.md](00-foundation/vision.md) and [overview.md](01-system-architecture/overview.md).
2. Implementers of the current engine should read
   [graph-model.md](02-knowledge-model/graph-model.md),
   [storage.md](03-persistence/storage.md), and
   [pipeline.md](05-context-retrieval/pipeline.md) together. The
   [evidence model](02-knowledge-model/evidence-model.md) is the proposed
   ADR-0015 extension, not the current API.
3. Algorithm changes should read [interactions.md](04-cognitive-dynamics/interactions.md), the [cognitive dynamics overview](04-cognitive-dynamics/overview.md), and [activation-flow.md](05-context-retrieval/activation-flow.md) together.
4. Release and quality work should read [observability.md](07-quality-gates/observability.md) and [benchmarks.md](07-quality-gates/benchmarks.md).
5. For design rationale, read [adr/](adr/), especially [0003 Bayesian magnitudes](adr/0003-bayesian-magnitudes.md).

## SSOT Rules

- Architecture chapters contain current contracts. Any summary of an
  unimplemented decision is isolated under a heading labeled `Proposed` and
  links to its proposed ADR.
- ADRs preserve durable decisions and consequences. Quality-gate records state
  active profiles, metric boundaries, and reproducible evidence.
- Use relative links between files.
- Prose must be concrete enough to guide implementation.
- Algorithm documents must include inputs, outputs, invariants, and failure conditions.
