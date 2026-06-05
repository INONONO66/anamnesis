# 0004. Query As Field, Retrieval As Read-Only, Commit As Work

- Status: Accepted
- Date: 2026-06-05
- Related: [pipeline](../05-context-retrieval/pipeline.md), [interactions](../04-cognitive-dynamics/interactions.md)

## Context

Retrieval should answer what is useful now, but merely looking at a graph should not rewrite memory. At the same time, actual use must improve future recall.

## Decision

Treat a query as a potential field. It creates transient activation and current, then returns a readout and trace. This phase is read-only.

State changes require an explicit commit. Commit consumes retrieval traces and records actual work:

- selected sites were used,
- paths contributed to the answer,
- sites were read out together,
- feedback was given,
- tensions were presented.

```text
query -> field -> activation flow -> readout -> ContextPackage
                                              -> optional commit -> reservoir update
```

## Consequences

Benefits:

- Retrieval is retry-safe.
- State changes are attributable to committed use.
- Access-locality behavior emerges without hidden mutation.
- Traces can explain every persistent delta.

Tradeoffs:

- Callers must distinguish preview from use.
- Commit needs trace validation.
- API surface is slightly larger than a mutating `query`.

## Alternatives Considered

### Mutate on every query

Rejected. It makes debugging and repeated search unsafe, and it reinforces accidental retrieval.

### Never update from retrieval

Rejected. The memory would not learn which paths are actually useful.

### Update only nodes, not paths

Rejected. It misses the core associative behavior: paths that repeatedly carry useful current should become easier to traverse.
