# ADR-010: Unified Search Pipeline

**Status**: Accepted

## Context

Search is the user-facing recall entry point. It must combine lexical, semantic, entity, temporal, scope, and identity cues without treating any index as memory itself.

## Decision

`search()` follows this pipeline:

```text
SearchInput
  -> trigger candidate generation
  -> per-source ranks and scores
  -> rank-based seed fusion
  -> graph recall by spreading activation
  -> scope/time/identity-aware scoring
  -> ContextPackage assembly
  -> SearchTrace diagnostics
```

Candidate sources may include:

- lexical / BM25-style text search,
- optional embedding similarity,
- entity tags,
- temporal filters,
- scope filters,
- identity cues.

## Candidate and Fusion Model

Each trigger returns candidates with provenance:

```rust
pub struct SearchCandidate {
    pub node_id: NodeId,
    pub source: CandidateSource,
    pub rank: usize,
    pub score: f64,
    pub reason: String,
}
```

Scores from different triggers are not directly comparable. The baseline fusion strategy is rank-based, such as reciprocal rank fusion:

```text
fused_score = Σ 1 / (k + rank)
```

## Trace Requirements

Search traces should record:

- raw trigger candidates,
- fused seeds,
- graph reconstruction paths,
- dropped candidates or fragments,
- packaging budget decisions,
- identity and scope contributions when they affect ranking.

## Engine Boundary

The engine does not call an LLM for query expansion, reranking, summarization, or answer generation. Embedding generation is also outside the core; the consumer may provide query and node embeddings.

## Related Decisions

- [ADR-007: Trigger Indexes vs. Graph Memory](./007-trigger-indexes-vs-graph-memory.md)
- [ADR-008: Temporal Memory Model](./008-temporal-memory-model.md)
- [ADR-009: Identity-Conditioned Recall](./009-identity-conditioned-recall.md)
