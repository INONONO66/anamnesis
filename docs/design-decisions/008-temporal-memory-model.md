# ADR-008: Temporal Memory Model

**Status**: Accepted

## Context

Knowledge validity is not the same as salience. A decision can be important and well-remembered while no longer being true. A bug can be fixed, an API can be superseded, and a project convention can apply only during a specific time window.

Decay answers "how likely is this to be recalled?" Temporal validity answers "was this true at that point in time?" The engine needs both.

## Decision

Represent temporal validity directly on graph records and provide point-in-time recall.

Nodes carry optional validity bounds:

```rust
pub struct Node {
    pub valid_from: Option<Timestamp>,
    pub valid_until: Option<Timestamp>,
    // ... other fields ...
}
```

Edges also carry optional validity bounds so relationships can expire independently from their endpoints:

```rust
pub struct Edge {
    pub valid_from: Option<Timestamp>,
    pub valid_until: Option<Timestamp>,
    // ... other fields ...
}
```

The engine exposes point-in-time query support:

```rust
pub fn fact_at(&self, query: &Query, as_of: Timestamp) -> Result<ContextPackage, Error>;
```

During bitemporal recall, nodes and edges are included only if their validity windows contain `as_of`. Missing bounds mean "valid indefinitely" on that side.

## Rationale

- **Explicit validity complements decay**: decay controls recall strength; validity controls truth at a time.
- **Supersession remains additive**: old facts do not need to be deleted. They can be closed with `valid_until` and linked to replacements with `Supersedes`.
- **Historical recall becomes possible**: consumers can ask what the graph knew or believed at a specific point in time.
- **Relationships can age independently**: a node may remain true while one specific edge between facts is no longer valid.
- **Backward compatibility is simple**: nodes and edges without bounds behave as always valid.

## Consequences

- Query execution must apply validity filtering before packaging results.
- Spreading activation must not propagate through edges invalid at the query timestamp.
- `fact_at()` should preserve the same output shape as `query()` so consumers do not need a separate result model.
- Consumers are responsible for choosing domain timestamps. The engine stores and filters timestamps; it does not infer historical truth from content.

## Implementation Status

Implemented. `Node` and `Edge` include validity bounds, spreading activation checks edge validity, and `Engine::fact_at()` returns a `ContextPackage` filtered to the requested timestamp.

## Related Decisions

- [ADR-002: Fragment Memory with Natural Decay](./002-fragment-memory-with-natural-decay.md)
- [ADR-006: Knowledge Scoping and Promotion](./006-knowledge-scoping-and-promotion.md)
- [ADR-010: Unified Search Pipeline](./010-search-pipeline.md)
