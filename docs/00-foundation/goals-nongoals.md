# Goals And Non-Goals

This chapter fixes what Anamnesis is responsible for and what it deliberately leaves outside the core. Goals are written at implementation-contract level; non-goals prevent boundary creep.

## Goals

| ID | Goal | Completion Criterion |
|---|---|---|
| G1 | Typed conductive graph | Sites and edges represent knowledge type, origin, time, scope, and conductance |
| G2 | Perception gate | Observations branch into new site allocation, duplicate-work integration, or rejection |
| G3 | Retained-action dynamics | Readout and time interactions act on the multi-trace base level `B_i` (committed access appends a trace whose per-trace decay rate `d_j = m_type·(c·e^{m_j} + α)` is fixed at creation from current activation; aging is power-law over the trace history). Activation-dependent decay reproduces the spacing effect — spaced re-presentation at low activation earns a low `d_j` and durable strength — subject to the documented retention-interval crossover (spaced wins only at sufficiently delayed tests). Feedback and social reinforcement update a separate non-decaying evidence prior `P_i` |
| G4 | Associative retrieval | Text, vector, and activation-flow results merge into one context package |
| G5 | Contradiction visibility | Conflicting relations are returned as frustration/tension instead of hidden |
| G6 | Scoped memory | Session, project, and universal scopes affect retrieval and promotion |
| G7 | Multi-agent provenance | Every fragment preserves producing agent and session |
| G8 | Pluggable storage | Storage implementations can be swapped behind one trait |
| G9 | Local-first default | The engine starts without a server, using in-memory or file-backed SQLite |
| G10 | Snapshot and restore | Graph state can be captured and restored for experiments and debugging |

## Non-Goals

- The core does not call LLMs, own extraction prompts, or decide embedding download policy.
- The core does not orchestrate sessions or expose an HTTP server.
- The core does not replace vector database products.
- The core does not automatically decide that all knowledge is true.
- The core does not overwrite original fragments with summaries.
- The core does not treat visualization coordinates as semantic distance.
- The core does not include remote sync or an authorization server.
- The core does not claim to reproduce the human testing effect. Activation-dependent decay treats a committed retrieval as a presentation regardless of whether it was a test or a restudy, so the test-vs-restudy dissociation at equal timing is out of scope; the engine instead models a separate commitment principle (a committed retrieval appends a durable trace; a read-only retrieval mutates nothing).

## MVP Scope

The MVP is a local library:

1. Observation ingest, duplicate routing, and initial conductance proposals.
2. `tick`-based dissipation and committed `Accessed` interaction integration.
3. `search` and `query` producing `ContextPackage` values and reflecting committed retrieval.
4. SQLite storage and clone-based snapshots.
5. Origin, scope, and bitemporal fields.
6. Contradiction constraints and tension return.

## Extension Scope

The following can be built on top of the core but should not be placed inside it:

- HTTP or RPC server APIs.
- Model-specific extraction pipelines.
- Hosted synchronization.
- UI and graph visualization.
- Multi-tenant authorization.
- LLM-based adjudication of conflicts.

## Design Completion Criteria

The design is complete when every public behavior documents its inputs, outputs, state changes, and failure conditions. Algorithm documents must include data structures, execution order, computational cost, and observable metrics instead of only formulas.
