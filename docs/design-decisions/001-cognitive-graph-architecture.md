# ADR-001: Cognitive Graph as Application-Layer Engine

**Status**: Accepted

## Context

Building a knowledge management engine for LLM agents. The engine needs physics-like dynamics (attraction, gravity, forgetting, perception) applied to a knowledge graph.

Options considered:

1. Build a custom graph database from scratch
2. Use existing graph DB (Neo4j) and add cognitive layer
3. Build cognitive mechanics as application-layer logic on top of pluggable storage

## Decision

Option 3: Application-layer cognitive engine with pluggable storage.

Core cognitive mechanics (attraction, gravity, perception, forgetting) are implemented as pure scoring/propagation functions in Rust. Storage is abstracted behind a `StorageAdapter` trait.

## Rationale

- Cognitive mechanics are fundamentally **ranking/scoring/propagation rules**, not storage operations
- Storage engines are commodity — SQLite, in-memory, Neo4j can all hold nodes and edges
- The differentiator is the dynamics model, which evolves faster than storage needs
- Pluggable storage enables starting simple (in-memory) and scaling later
- Pure functions are trivially testable and benchmarkable

## Consequences

- Storage implementations must satisfy the `StorageAdapter` trait
- Performance-critical paths (similarity search, graph traversal) run in Rust
- LLM integration (embedding generation, knowledge extraction) is NOT in this engine — that's the consumer's job (e.g., an orchestration layer)
- Initial implementation uses in-memory storage; SQLite adapter is the first external storage target
