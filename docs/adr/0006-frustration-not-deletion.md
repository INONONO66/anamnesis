# 0006. Contradiction Becomes Frustration, Not Deletion

- Status: Accepted
- Date: 2026-06-05
- Related: [frustration](../04-cognitive-dynamics/frustration.md), [graph-model](../02-knowledge-model/graph-model.md)

## Context

Agent memory must preserve conflicts. If the engine deletes or silently merges contradictory facts, it loses provenance and can hide real uncertainty.

## Decision

Represent contradiction with `Contradicts` constraint edges. During retrieval, these edges do not propagate activation. They generate query-local frustration stress when both endpoints are active and valid together.

```text
sigma_ij =
    contradiction_weight_ij
  * min(a_i, a_j)
  * scope_overlap
  * temporal_overlap
```

Each factor is a bounded gate. `contradiction_weight_ij` is the non-negative constraint strength carried by the `Contradicts` edge, `min(a_i, a_j)` is the bounded query-local activation, and `scope_overlap`, `temporal_overlap` each lie in `[0, 1]` (`0` = no overlap, `1` = full overlap). So `sigma_ij >= 0`, and if any gate is `0` the stress is `0`. See [frustration.md](../04-cognitive-dynamics/frustration.md).

The result is returned as tension. No side is automatically judged true.

## Consequences

Benefits:

- Preserves both claims and their origins.
- Makes conflicts visible in context.
- Keeps propagation evidence clean.
- Allows later review, supersession, or crystallization.

Tradeoffs:

- Context may contain unresolved tensions.
- Consumers must decide how to present or act on them.
- Additional trace and packaging rules are required.

## Alternatives Considered

### Delete the lower-confidence side

Rejected. It destroys evidence and makes trust calibration brittle.

### Merge contradictions into one summary

Rejected. It erases provenance and time validity.

### Treat contradiction as negative activation

Rejected. It can silently suppress one side and turns retrieval into truth judgment.

### Express conflict only through visualization layout

Rejected. Layout does not affect retrieval semantics.
