# ADR-007: Trigger Indexes vs. Graph Memory

**Status**: Accepted

## Context

Agent memory systems often conflate retrieval indexes with memory. A vector database can find semantically similar text, and a full-text index can find exact terms, but neither can represent why a decision was made, who produced a fact, when it was valid, what contradicted it, or how it connects to other memories.

Anamnesis models memory as graph-structured fragments with cognitive dynamics. The engine still benefits from keyword, BM25-style, vector, entity, and temporal retrieval, but these mechanisms should start recall rather than replace the graph.

## Decision

Treat keyword, BM25, full-text search, embeddings, entity tags, and temporal filters as **trigger indexes**. They return candidate `NodeId`s and scores. The graph remains the source of truth.

```text
query
  -> trigger indexes find candidate NodeIds
  -> seed fusion selects recall entry points
  -> spreading activation reconstructs graph context
  -> packaging returns identity, knowledge, memories, and tensions
```

The guiding rule is:

> Indexes trigger; graph remembers.

## Retrieval Flow

### 1. Candidate generation

Candidate sources may include:

| Trigger | Strength |
|:--------|:---------|
| Keyword / exact match | File paths, identifiers, dates, error strings, names |
| BM25 / full-text search | Rare terms and lexical relevance across node content |
| Embedding similarity | Paraphrases and conceptual similarity |
| Entity tags | Structured names and known concepts |
| Temporal filters | Time windows and validity constraints |

Each source returns `NodeId`, source-specific rank or score, and a match reason. Search keeps these raw candidates in its trace so retrieval can be diagnosed separately from graph recall.

### 2. Seed fusion

Candidate scores from different retrieval systems are not directly comparable. BM25 scores, cosine similarity, exact-match bonuses, and temporal filters have different scales. Rank-based fusion, such as reciprocal rank fusion, is the preferred starting point because it combines agreement across sources without brittle score normalization.

The unified `search()` pipeline should therefore preserve:

```text
raw candidates -> per-source ranks -> fused seeds -> graph paths -> packaging drops
```

### 3. Graph recall

Fused seeds enter the cognitive graph. Spreading activation follows typed edges and applies salience gates, gravity boosts, identity priors, scope weights, and contradiction damping.

This is where memory is reconstructed:

- `Origin` answers who produced the memory and from which session/project.
- timestamps and validity windows answer when it happened or when it was true.
- `Reason`, `Supports`, `Refutes`, `RejectedAlternative`, and `Supersedes` edges answer why.
- `ExtractedFrom` and `ConsolidatedFrom` preserve source evidence.
- `Contradicts` edges surface tensions instead of flattening disagreement.

### 4. Packaging

The final `ContextPackage` is not just top-k nearest text. It is a structured package of identity, knowledge, episodic memories, and tensions, with L0/L1/L2 resolution chosen under a token budget.

## Storage Boundary

`StorageAdapter::text_search()` is the lexical trigger seam. The default implementation may be simple and `std`-only. More capable backends can override it with BM25, FTS, or database-specific indexes.

Embeddings are stored on nodes and may be used for query-time similarity, but the embedding vector is not the memory. It is a rebuildable projection or cue into graph recall.

## What the Engine Does Not Do

- It does not treat a vector database as the source of truth.
- It does not store facts only inside index entries.
- It does not call an LLM for query expansion, reranking, or summarization.
- It does not require an external BM25 or ANN dependency in the core.
- It does not consider a nearest-neighbor hit sufficient evidence without graph reconstruction.

## Rationale

- **Exact and semantic cues are complementary**: BM25 is strong for exact strings; embeddings are strong for paraphrase.
- **Graph context is the memory**: Edges and provenance represent why, who, when, and how memories relate.
- **Indexes must be replaceable**: Tokenizer changes, embedding model changes, or storage backend changes should not migrate memory.
- **Evaluation becomes diagnosable**: Trigger recall, graph reconstruction, packaging, and final answer quality can be measured separately.

## Consequences

- Search traces should record per-source candidates, fused seeds, graph paths, and packaging drops.
- Low-salience memories should still be seedable by precise lexical triggers; exact recall can revive archived nodes via `touch()`.
- `search()` should expose a raw candidate layer before `ContextPackage` assembly.
- The public API should keep `search()` as a user-facing entry point and `query()` as structured graph retrieval.

## Related Decisions

- [ADR-001: Cognitive Graph as Application-Layer Engine](./001-cognitive-graph-architecture.md)
- [ADR-002: Fragment Memory with Natural Decay](./002-fragment-memory-with-natural-decay.md)
- [ADR-005: Query Crystallization](./005-query-crystallization.md)
- [ADR-006: Knowledge Scoping and Promotion](./006-knowledge-scoping-and-promotion.md)
