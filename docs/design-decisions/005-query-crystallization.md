# ADR-005: Query Crystallization

**Status**: Proposed

## Context

Anamnesis's knowledge graph grows through external observations (`ingest()`). Observations enter as raw fragments — episodic conversation turns, extracted facts, entity mentions. Over time, the query pipeline (`query()`) discovers patterns by traversing these fragments via spreading activation.

But query results are ephemeral. Each `query()` call re-derives the same associations from scratch. When an agent queries "what do I know about the auth module?" and synthesizes an insight from 8 related fragments, that insight exists only in the agent's context window. The next session starts over.

Karpathy's LLM Wiki pattern (April 2026) crystallized this problem: **"Good answers can be filed back into the wiki. This way your explorations compound in the knowledge base just like ingested sources do."** The wiki grows not only from external input but from internal synthesis.

Current options for consumers who want this feedback loop:

1. Manually call `ingest()` with the synthesis → loses provenance (which fragments were the basis?)
2. Call `ingest()` + N×`link()` for ConsolidatedFrom edges + N×`touch()` for source reinforcement → tedious, error-prone, no dedup protection
3. Do nothing → knowledge doesn't compound

None of these are satisfactory. The engine should provide a first-class operation for this loop.

### What Exists Today

- `EdgeType::ConsolidatedFrom` is defined (kappa = 1.0) but never created by any Engine method
- `ingest()` handles external observations with perception gating + attraction auto-linking
- `touch()` reinforces nodes on access (eq 5)
- `query()` returns `ContextPackage` with `Fragment` entries that include `node_id` and `relevance`
- Vision.md states: "Summaries are emergent — they arise when the Curator consolidates repeated patterns into higher-level semantic nodes"
- ADR-002 states: "Consolidation is additive, not destructive"

### What's Missing

An Engine-level operation that:
1. Creates a new node from synthesized content
2. Links it to its source fragments via `ConsolidatedFrom`
3. Reinforces source fragments (they contributed to synthesis)
4. Computes appropriate initial salience (synthesis > raw fragment)
5. Protects against duplicate crystallizations
6. Preserves full provenance chain

## Decision

Add `Engine::crystallize()` — a dedicated method for re-ingesting query-derived insights into the graph.

### New Types

```rust
/// Request to crystallize query results into a new knowledge node
pub struct CrystallizeRequest {
    /// The synthesized content (consumer provides — typically LLM-generated)
    pub name: String,                        // L0: one-liner
    pub summary: Option<String>,             // L1: optional structural overview
    pub content: String,                     // L2: full synthesis

    /// Embedding of the synthesis (consumer provides)
    pub embedding: Option<Vec<f64>>,

    /// Source fragment node IDs (from ContextPackage)
    pub source_ids: Vec<NodeId>,

    /// Relevance scores for each source fragment [0, 1] (optional; from spreading activation)
    pub source_relevances: Option<Vec<f64>>,

    /// Type of the crystallized node
    pub node_type: KnowledgeType,            // typically Semantic or Decision

    /// Confidence in the synthesis [0, 1]
    pub confidence: f64,

    /// Origin of the synthesis
    pub origin: Origin,

    /// Entity tags for the synthesis (enables future cross-linking)
    pub entity_tags: Vec<String>,

    /// Timestamp of crystallization
    pub timestamp: Timestamp,
}

/// Result of crystallization
pub struct CrystallizeResult {
    /// The newly created synthesis node
    pub node_id: NodeId,

    /// ConsolidatedFrom edges created (new node → each source)
    pub consolidation_edges: Vec<EdgeId>,

    /// Attraction edges created (new node → similar non-source nodes)
    pub attraction_edges: Vec<EdgeId>,

    /// Source nodes that were reinforced via touch()
    pub nodes_reinforced: usize,

    /// Initial salience assigned to the new node
    pub initial_salience: f64,
}
```

### New Equations

**Equation (15): Crystallization initial salience**

```
s_crystal = clamp(
    w_avg * s̄_sources + w_conf * confidence + w_bonus * β,
    0, 1
)

where:
    s̄_sources = Σ(s_i * R_i) / Σ(R_i)   — relevance-weighted mean of source saliences
    confidence                              — consumer-provided synthesis confidence
    β = 0.10                                — synthesis bonus (crystallized > raw)
    w_avg = 0.60, w_conf = 0.25, w_bonus = 0.15
```

Rationale for weights:
- Source salience dominates (60%) — crystallization of low-salience fragments shouldn't produce high-salience nodes
- Confidence matters (25%) — high-confidence synthesis deserves higher starting salience
- Fixed bonus (15%) — synthesis is inherently more valuable than raw fragments; the bonus ensures it starts above the source average

**Equation (16): Crystallization deduplication**

```
dup_score = max_{j ∈ CrystallizedNodes} cosine_similarity(e_crystal, e_j)

if dup_score > θ_dup:
    reject crystallization (return Error::DuplicateCrystallization)

where:
    θ_dup = 0.92    — dedup threshold (stricter than perception novelty)
    CrystallizedNodes = { n | ∃ edge(n, _, ConsolidatedFrom) }
```

The dedup threshold (0.92) is intentionally stricter than the perception novelty threshold (default 0.30 novelty = 0.70 similarity). Crystallized nodes represent pre-synthesized knowledge — duplicating them wastes graph capacity without adding information.

### Engine Method

```rust
impl<S: StorageAdapter> Engine<S> {
    /// Crystallize query results into a new knowledge node.
    ///
    /// This closes the compounding loop:
    ///   ingest → graph grows → query finds patterns →
    ///   crystallize → graph gains higher-level knowledge →
    ///   next query benefits from pre-synthesized nodes
    ///
    /// # Flow
    ///
    /// 1. Validate: all source_ids exist, at least 2 sources
    /// 2. Dedup gate (eq 16): check embedding similarity against existing crystallized nodes
    /// 3. Compute initial salience (eq 15): relevance-weighted source average + bonus
    /// 4. Create node: synthesis content, embedding, computed salience
    /// 5. Create ConsolidatedFrom edges: new node → each source (weight = source relevance)
    /// 6. Attraction pipeline: auto-link to similar non-source nodes (reuses ingest pipeline)
    /// 7. Reinforce sources: touch() each source node (eq 4 → eq 5)
    /// 8. Return CrystallizeResult
    ///
    /// # Errors
    ///
    /// - `Error::InvalidNodeId` — source_id does not exist
    /// - `Error::DuplicateCrystallization` — dedup gate failed (too similar to existing crystallized node)
    /// - `Error::InvalidInput` — fewer than 2 source_ids
    pub fn crystallize(
        &mut self,
        request: CrystallizeRequest,
    ) -> Result<CrystallizeResult, Error>;
}
```

### Implementation Flow (Detailed)

```
crystallize(request)
│
├─ Phase 1: Validation
│   ├─ source_ids.len() >= 2?                          → Error::InvalidInput
│   ├─ all source_ids exist in graph?                   → Error::InvalidNodeId
│   └─ collect source nodes: (salience, embedding, relevance, node_type)
│
├─ Phase 2: Dedup Gate (eq 16)
│   ├─ if request.embedding is Some:
│   │   ├─ find all nodes with outgoing ConsolidatedFrom edges
│   │   ├─ for each: cosine_similarity(request.embedding, node.embedding)
│   │   └─ if max_similarity > 0.92 → Error::Rejected("duplicate crystallization")
│   └─ if request.embedding is None: skip dedup
│
├─ Phase 3: Salience Computation (eq 15)
│   ├─ for each source: weight = relevance (from last query) or 1.0 if unknown
│   │   (relevance not stored on Node — consumer passes it or engine uses uniform)
│   ├─ s̄ = weighted_mean(source_saliences, weights)
│   ├─ s_crystal = 0.60 * s̄ + 0.25 * request.confidence + 0.15 * 0.10
│   └─ clamp to [0.0, 1.0]
│
├─ Phase 4: Node Creation
│   ├─ node_id = graph.next_node_id()
│   ├─ Node {
│   │     id: node_id,
│   │     node_type: request.node_type,
│   │     name: request.name,               // L0
│   │     summary: request.summary,          // L1
│   │     content: request.content,          // L2
│   │     embedding: request.embedding,
│   │     created_at: request.timestamp,
│   │     updated_at: request.timestamp,
│   │     accessed_at: request.timestamp,
│   │     valid_from: None,
│   │     valid_until: None,
│   │     salience: s_crystal,              // eq 15 (NOT 1.0 like ingest)
│   │     access_count: 0,
│   │     origin: request.origin,
│   │     entity_tags: request.entity_tags,
│   │     metadata: HashMap::new(),
│   │ }
│   └─ graph.add_node(node)
│
├─ Phase 5: ConsolidatedFrom Edges
│   ├─ for each source_id in request.source_ids:
│   │     edge_id = graph.next_edge_id()
│   │     Edge {
│   │         source: node_id,              // crystal → source (crystal consolidates FROM sources)
│   │         target: source_id,
│   │         edge_type: EdgeType::ConsolidatedFrom,
│   │         weight: source_salience.clamp(0.1, 1.0),  // weight = source's current salience
│   │         ...
│   │     }
│   └─ collect edge_ids into consolidation_edges
│
├─ Phase 6: Attraction Pipeline (reuse from ingest)
│   ├─ if request.embedding is Some:
│   │   ├─ candidate pool: last 256 nodes + entity-tag matches − source_ids − node_id
│   │   ├─ for each candidate:
│   │   │     sim = cosine_similarity(request.embedding, candidate.embedding)
│   │   │     tau = tau_type(request.node_type, candidate.node_type)
│   │   │     mass = compute_mass(candidate.salience, candidate.access_count, candidate.node_type)
│   │   │     score = attraction_score(sim, tau, mass)        (eq 3)
│   │   │     if should_create_edge(score, ...): include
│   │   ├─ top 4 by score: create Semantic edges or strengthen existing
│   │   └─ collect edge_ids into attraction_edges
│   └─ if no embedding: skip attraction
│
├─ Phase 7: Source Reinforcement
│   ├─ for each source_id in request.source_ids:
│   │     touch(source_id, request.timestamp)?              (eq 4 → eq 5)
│   └─ nodes_reinforced = source_ids.len()
│
└─ Return CrystallizeResult {
        node_id,
        consolidation_edges,
        attraction_edges,
        nodes_reinforced,
        initial_salience: s_crystal,
    }
```

### How Crystallized Nodes Behave in the Graph

Once created, crystallized nodes are **ordinary graph nodes** subject to all existing mechanics:

| Mechanic | Behavior |
|:---------|:---------|
| **Decay** (eq 4) | Normal decay at `lambda_for_type(request.node_type)`. If Semantic → λ=0.020 |
| **Reinforcement** (eq 5) | Normal touch() on access |
| **Spreading Activation** (eq 11) | ConsolidatedFrom edges have kappa=1.0 (full propagation). Activation flows freely between crystal and sources |
| **Gravity** (eq 1) | Mass computed normally. Over time, frequently-accessed crystals gain high mass → attract more connections |
| **Attraction** (eq 3) | Auto-linked to similar nodes on creation. Future ingest() calls can create edges to crystals |
| **Query** (eq 13) | Scored like any other node. High-quality crystals surface because they start with elevated salience and accumulate connections |

**Emergent behavior**: Over time, source fragments decay (especially Episodic, λ=0.050) while crystals decay slower (Semantic, λ=0.020). The crystal naturally becomes the primary access point for that knowledge cluster. But the source fragments remain reachable via ConsolidatedFrom edges — "fragments, not summaries" is preserved.

### Difference from `ingest()`

| Aspect | `ingest()` | `crystallize()` |
|:-------|:-----------|:-----------------|
| **Input source** | External observation | Internal synthesis from existing nodes |
| **Initial salience** | 1.0 (max) | eq 15: weighted by source quality |
| **Edge creation** | Only attraction (Semantic edges) | ConsolidatedFrom + attraction |
| **Source tracking** | None (origin only) | Explicit ConsolidatedFrom edges to source nodes |
| **Source reinforcement** | None | touch() on all source nodes |
| **Dedup check** | Perception novelty gate | Dedicated crystallization dedup (eq 16, stricter) |
| **Minimum sources** | 1 observation | ≥ 2 source nodes required |

### Difference from `auto_merge()`

| Aspect | `crystallize()` | `auto_merge()` |
|:-------|:-----------------|:---------------|
| **Trigger** | Consumer calls explicitly with synthesis content | Engine detects merge candidates automatically |
| **Content** | Consumer provides (LLM-generated synthesis) | Engine combines existing node content |
| **Source preservation** | Sources kept intact, linked via ConsolidatedFrom | One source absorbed into the other |
| **Use case** | "I learned something new from querying" | "These two nodes say the same thing" |
| **Reversibility** | Additive (new node + edges, nothing deleted) | Destructive (absorbed node removed) |

## Rationale

1. **Separate from ingest() because semantics differ.** Ingest handles external input with perception gating. Crystallize handles internal synthesis with provenance tracking. Conflating them would require ingest() to handle two fundamentally different flows.

2. **Minimum 2 sources** because crystallization of a single node is just duplication. The value is in synthesis — combining multiple fragments into something greater.

3. **Relevance-weighted salience (eq 15)** because not all source fragments contribute equally. A highly-relevant fragment should influence the crystal's salience more than a tangentially-related one.

4. **Stricter dedup (eq 16, θ=0.92)** because crystallized nodes are pre-synthesized. Allowing near-duplicates would create redundant summary nodes that clutter the graph without adding information.

5. **Source reinforcement via touch()** because fragments that contribute to synthesis have proven their value. They should be harder to forget.

6. **ConsolidatedFrom edge weight = source salience** because this naturally encodes the source's importance. High-salience sources create stronger consolidation links, making them more reachable via spreading activation from the crystal.

7. **Reusing attraction pipeline** from ingest() because the crystal should connect to the broader graph, not just its sources. This enables discovery paths: query → crystal → attraction edges → related non-source nodes.

8. **Additive, not destructive** (ADR-002 principle). Source fragments are never modified or deleted. The crystal is a new node layered on top. If the crystal's salience decays to zero, the sources remain.

## Alternatives Considered

### A. Extend `ingest()` with a `consolidation_sources` field

```rust
pub struct Observation {
    // ... existing fields ...
    pub consolidation_sources: Option<Vec<NodeId>>,  // if set, treat as crystallization
}
```

**Rejected** because:
- Conflates two distinct semantics (external input vs. internal synthesis)
- Ingest starts at salience 1.0; crystals should start lower (eq 15)
- Ingest dedup uses perception novelty; crystals need stricter dedup (eq 16)
- Makes Observation's API surface confusing — optional fields that change method behavior

### B. Consumer-side crystallization (no engine support)

```rust
// Consumer does:
let ids = engine.ingest(synthesis_observation)?;
for source_id in sources {
    engine.link(ids[0], source_id, EdgeType::ConsolidatedFrom, 0.8)?;
    engine.touch(source_id, now)?;
}
```

**Rejected** because:
- No dedup protection — consumer must implement their own
- Salience starts at 1.0 (wrong for synthesis)
- Attraction pipeline runs but source nodes aren't excluded from candidates
- Every consumer reimplements the same N-step workflow
- Error handling is consumer's burden (partial failure leaves inconsistent state)

### C. Make crystallize() part of auto_merge()

**Rejected** because:
- auto_merge() is destructive (absorbs one node into another)
- crystallize() is additive (creates new node, keeps sources)
- auto_merge() is engine-initiated; crystallize() is consumer-initiated
- Different triggers, different semantics, different outcomes

## Consequences

### API Changes
- New method: `Engine::crystallize(CrystallizeRequest) -> Result<CrystallizeResult, Error>`
- New types: `CrystallizeRequest`, `CrystallizeResult`
- New error variant: `Error::DuplicateCrystallization { existing_node: NodeId, similarity: f64 }`

### Equation System
- New: eq 15 (crystallization initial salience)
- New: eq 16 (crystallization deduplication)

### Graph Structure Impact
- `ConsolidatedFrom` edges will now be created (currently defined but never used)
- Spreading activation already traverses ConsolidatedFrom (kappa = 1.0) — no query changes needed
- Over time, crystallized nodes form a higher-level knowledge layer above raw fragments

### Consumer Contract
- Consumer is responsible for:
  - Running `query()` to discover patterns
  - Generating synthesis content (via LLM or other means)
  - Calling `crystallize()` with the synthesis + source node IDs
  - Providing embedding for the synthesis (for dedup and attraction)
- Consumer is NOT responsible for:
  - Edge creation (engine handles ConsolidatedFrom + attraction)
  - Salience computation (engine handles eq 15)
  - Dedup checking (engine handles eq 16)
  - Source reinforcement (engine handles touch())

### Interaction with Other Planned Features

| Feature | Interaction |
|:--------|:------------|
| `auto_merge()` (Phase 3) | Complementary. auto_merge removes duplicates; crystallize creates syntheses. Both reduce redundancy but through different mechanisms. |
| `reflect_batch()` (Phase 3) | Independent. reflect_batch creates Entity edges across agents; crystallize creates ConsolidatedFrom edges within an agent's knowledge. |
| Graph Events (#9 insight) | crystallize() should emit `NodeCreated` + `EdgeCreated` events. A future `NodeCrystallized` event variant could carry source_ids for consumer observation. |
| Drift Detection (#11 insight) | High crystallization rate in a time window may indicate rapid knowledge evolution. `measure_drift()` could track crystallization frequency. |
| Compression Hints (#6 insight) | Crystallized nodes are candidates for `protected` status in `compression_hints()` — they represent pre-synthesized knowledge that is expensive to re-derive. |

### Testing Strategy

```
tests/crystallize.rs:
    test_crystallize_basic                  — 3 source nodes → 1 crystal, verify node + edges
    test_crystallize_salience_computation   — verify eq 15 with known inputs
    test_crystallize_dedup_rejects          — create crystal, try near-duplicate → Error::DuplicateCrystallization
    test_crystallize_dedup_allows_different — create crystal, create different crystal → Ok
    test_crystallize_source_reinforcement   — verify source saliences increase after crystallize
    test_crystallize_attraction_edges       — verify crystal links to non-source similar nodes
    test_crystallize_minimum_sources        — 0 or 1 source → Error::InvalidInput
    test_crystallize_invalid_source_id      — nonexistent source → Error::InvalidNodeId
    test_crystallize_decay_over_time        — crystal decays normally via tick()
    test_crystallize_query_traversal        — query from seed → crystal reachable via ConsolidatedFrom
    test_crystallize_source_still_queryable — after crystal created, sources still appear in queries
    test_crystallize_compounds              — crystallize → query → crystallize again → verify layer
```

### Benchmark Strategy

```
benches/crystallize_bench.rs:
    crystallize_3_sources    — baseline: 3 source fragments
    crystallize_10_sources   — scaling: 10 source fragments
    crystallize_dedup_scan   — dedup cost: scan N existing crystals
    crystallize_with_attraction — full pipeline including attraction edges
```

## References

- [Karpathy LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f) — "Good answers can be filed back into the wiki"
- [ADR-002: Fragment Memory with Natural Decay](./002-fragment-memory-with-natural-decay.md) — "Consolidation is additive, not destructive"
- [Vision: Fragments, Not Summaries](../vision.md) — "Summaries are emergent via consolidation"
- [Insight #13: Query Crystallization](../insights/insights-karpathy-wiki.local.md) — Initial concept
- [OMEGA memory consolidation](https://github.com/omega-memory/omega-memory) — Access-based reinforcement + consolidation (closest external analog)
