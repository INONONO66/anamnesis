# Anamnesis Technical Specification

Anamnesis is a Rust library that preserves LLM-agent conversations and work experience as a graph, then retrieves relevant knowledge as context when a cue is provided. This `docs` directory is the final-design SSOT for future implementation work.

These documents are not a code status table or an audit log. Each chapter describes the system we intend to build. Implementers should align types, storage, algorithms, and public APIs to this specification.

## One-Line Definition

> A cognitive memory engine based on spreading activation (ACT-R): cues activate related memories and spread through them, with that activation flow expressed through the intuition of a path-dependent conductive network.

## Design Summary

- **Fragment first.** Conversation turns and document fragments enter as memory sites without losing the original text.
- **The theory is spreading activation.** Cues activate related memories and spread through associations. Conductance is associative strength (`log-LR`); the conductive-network frame is the representation, not a separate theory.
- **Retrieval is perturbation.** A query imposes a semantic potential field on the graph; retrieval is the resulting flow and readout.
- **Use leaves work behind.** Only fragments and paths actually consumed as context are integrated as committed work, lowering impedance for future retrieval.
- **Forgetting is leakage.** Unused sites are not deleted. Their retained action falls through leakage and dissipation.
- **Contradiction is frustration.** Conflicting knowledge is preserved as constraint stress and surfaced in context instead of being hidden or erased.
- **Scope and origin are required.** Every site carries agent, session, scope, and confidence.
- **Storage is swappable.** The public engine runs over a storage trait. SQLite is the default adapter.

## Stack At A Glance

| Area | Choice | Reason |
|---|---|---|
| Language | Rust 2024 | Strong library boundary, type safety, synchronous core API |
| Default storage | SQLite | Embedded use, durable file mode, easy testing |
| Optional embeddings | External provider trait | Keeps model choice and download policy outside the engine |
| Public API | Synchronous `Engine` surface | Easy for LLM runtimes and CLIs to wrap |
| Graph model | Typed site / typed edge | Fixes retrieval, dissipation, contradiction, and scope policy in data types |
| Querying | `search` + `query` | Combines text, vector, and activation flow into one context package |

## Document Index

### 00 - Foundation
- [vision.md](00-foundation/vision.md) - product purpose, audience, design direction
- [goals-nongoals.md](00-foundation/goals-nongoals.md) - scope, non-scope, completion criteria
- [glossary.md](00-foundation/glossary.md) - terms and symbols

### 01 - Architecture
- [overview.md](01-system-architecture/overview.md) - system boundary, main flows, public surface

### 02 - Knowledge Model
- [graph-model.md](02-knowledge-model/graph-model.md) - nodes, edges, types, tiers
- [temporal-model.md](02-knowledge-model/temporal-model.md) - record time, fact time, access history
- [peer-identity.md](02-knowledge-model/peer-identity.md) - peers, origin, trust
- [scoping-promotion.md](02-knowledge-model/scoping-promotion.md) - scope and promotion

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
- [social.md](04-cognitive-dynamics/social.md) - peer trust and cross-agent reinforcement
- [readout-scoring.md](04-cognitive-dynamics/readout-scoring.md) - selecting sites that can be read out

### 05 - Context Retrieval
- [activation-flow.md](05-context-retrieval/activation-flow.md) - activation current under a query field
- [pipeline.md](05-context-retrieval/pipeline.md) - candidate collection, flow, packaging

### 07 - Quality Gates
- [observability.md](07-quality-gates/observability.md) - health, trace, invariant telemetry
- [benchmarks.md](07-quality-gates/benchmarks.md) - performance budgets and measurement

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

## Reading Order

1. New readers should start with [vision.md](00-foundation/vision.md) and [overview.md](01-system-architecture/overview.md).
2. Implementers should first fix [graph-model.md](02-knowledge-model/graph-model.md), [storage.md](03-persistence/storage.md), and [pipeline.md](05-context-retrieval/pipeline.md).
3. Algorithm changes should read [interactions.md](04-cognitive-dynamics/interactions.md), the [cognitive dynamics overview](04-cognitive-dynamics/overview.md), and [activation-flow.md](05-context-retrieval/activation-flow.md) together.
4. Release and quality work should read [observability.md](07-quality-gates/observability.md) and [benchmarks.md](07-quality-gates/benchmarks.md).
5. For design rationale, read [adr/](adr/), especially [0003 Bayesian magnitudes](adr/0003-bayesian-magnitudes.md).

## SSOT Rules

- This directory contains final design only.
- Do not add historical status, audit notes, comparison drafts, work steps, or reference-repository operating rules.
- Use relative links between files.
- Prose must be concrete enough to guide implementation.
- Algorithm documents must include inputs, outputs, invariants, and failure conditions.
