# Goals And Non-Goals

This chapter fixes what Anamnesis is responsible for and what it deliberately leaves outside the core. Goals are written at implementation-contract level; non-goals prevent boundary creep.

## Goals

| ID | Goal | Completion Criterion |
|---|---|---|
| G1 | Typed conductive graph | Sites and edges represent knowledge type, origin, time, scope, and conductance |
| G2 | Perception gate | Observations branch into new site allocation, duplicate-work integration, or rejection |
| G3 | Retained-action dynamics | Readout work and time interactions operate on the same reservoir |
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

## MVP Scope

The MVP is a local library:

1. Observation ingest, duplicate routing, and initial conductance proposals.
2. `tick`-based dissipation and `Accessed` interaction integration.
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
