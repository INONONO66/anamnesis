# 06 — Envelope and Local PPR

Two things live here. The first is the bounded physical graph read behind the
`graph.envelope` RPC: a diagnostic surface over the ConductingArc cache. The
second is the local PPR solver that recall runs in TypeScript. They are
separate. Recall doesn't call `graph.envelope`; its own graph read is the
seed-adjacent query in [05-recall](05-recall.md) §4–5. The two-hop,
mass-ordered online envelope that would join them is a
[09-roadmap](09-roadmap.md) target.

## 1. Limits

| Surface | Item | Value |
|---|---|---|
| `graph.envelope` | seeds per request | 1..128 |
| `graph.envelope` | ordered probe per source | first 256 ConductingArc rows |
| `graph.envelope` | nodes returned | ≤ 2,000 |
| `graph.envelope` | arcs returned | ≤ 20,000 |
| recall | seed-adjacent ConductingArc rows | `LIMIT 1024` |
| solver | `α`, tolerance, iteration cap | 0.85, 1e-4, 64 |

The `graph.envelope` bounds are limits of that diagnostic read. They say
nothing about what a recall sees.

## 2. Expansion

### Canonical DegreeProbe(n)

DegreeProbe(n) counts the **first 256 ConductingArc rows** for
`source_id = n.id` in `link_id ASC` order. The cache has exactly one endpoint
row per retained physical NEXT_EPISODE, MENTIONS, RELATES_TO, HAS_MEMBER or
DERIVED_FROM link (docs/01 §5). Parallel links count separately. Under
complete, consistent coverage, values 0..255 are the exact physical conducting
degree. 256 is saturated: it classifies a hub and is never an exact count.

The unique composite `(source_id, link_id)` RANGE index performs the source
equality and the ordered link iteration across all five roles. `LIMIT 256`
precedes collection, counting, any role, time, generation or policy filter and
any non-index sort. Rows and physical links change atomically on creation,
deletion and topology rewire. `Meta.conducting_arc_ready` is the single
completeness gate. Missing or incomplete coverage fails with
`degree_probe_unavailable`, and an unavailable ordered access path fails with
`ordered_probe_unavailable`. Neither is ever read as degree zero, and there is
**no native adjacency fallback**. The cache is nonsemantic: it isn't
authority, a candidate, a conductor or an output, and it is rebuilt from
retained physical links, not from a policy-filtered view.

Rows of unselected generations and denied relationships can saturate a probe.
They still can't conduct. A stale row occupies its probe slot, is discarded
with no refill and invalidates coverage until repair.

### graph.envelope

`graph.envelope {seed_ids[1..128], T?}` is authenticated and read-only. For
each seed it runs the probe above and reports `{source_id, count, saturated,
coverage: "complete"}`. An unsaturated source contributes its probe rows as
arcs and their peers as nodes. A saturated source contributes none. The read
stops at 2,000 nodes or 20,000 arcs and then sets `truncated` and the
`overflow {nodes, arcs, saturated_sources}` counters. The result carries its
`pin {policy_revision, generation_id, coverage_revision, covered_ingest_seq,
T}`. The read takes the same Meta fence as policy and generation writes, so it
never sees a torn policy or selector.

## 3. Hubs

A saturated probe never authorizes traversal over sampled rows or native
adjacency. A saturated source has no outgoing rows in the envelope; other
sources may still reach it. No hub shortlist exists: the HubArc cache was
removed in D60.

## 4. Retained-row normalization

```text
  A_i  = ordered multiset of retained arcs (i, link_id, destination)
  Z_i  = Σ_{a∈A_i} w_role(a)
  W_ij = Σ_{a∈A_i : destination(a)=j} w_role(a) / Z_i    when Z_i > 0
  d_i  = 1 if Z_i = 0, otherwise 0                       dangling indicator
```

Every non-dangling row sums to one over the graph the solver actually
received. Relationships that weren't read are absent; they don't contribute to
a denominator computed from another graph. This gives the TypeScript solver
and GDS the same transition matrix (docs/07).

Each `w_role` must be finite and strictly positive, or the solver throws. A
role without a configured weight uses 1, and recall configures none. The
solver rejects an arc that carries its own weight. Parallel physical links are
separate arcs in `A_i` and contribute separately to `Z_i` and to their matrix
cell.

## 5. PPR

```text
  p⁰ = s                                       (seed distribution, Σ s = 1)
  p^{k+1} = (1−α) s + α ( Wᵀ p^k + (Σ_i d_i p^k_i) · q )      q = 1/|V| uniform
  for n = 1..maxIter: compute p^n from p^{n−1}
  δ_n = ‖p^n − p^{n−1}‖₁
  stop after the new iterate when δ_n < τ; otherwise return p^maxIter

  α = 0.85,  τ = 1e-4,  maxIter = 64
```

- Σ p = 1 in exact arithmetic at every iteration, because each row is
  normalized or dangling. Seeds must be finite and nonnegative with a positive
  total. Malformed nodes, arcs or weights are rejected at the solver boundary.
- **Local error bound**: the update is an α-contraction in L1, so for the
  returned p^n, `‖p^n − p*‖₁ <= α/(1−α) · δ_n`. At δ_n < 1e-4 this is less
  than `0.85/0.15 · 1e-4 = 0.000566666667`. The solver returns
  `iterateDeltaL1`, `residualL1` and `errorBoundL1`. Neither bound measures
  what the graph read left out, or retrieval usefulness.
- **maxIter 64**: with p⁰=s, `δ_n <= 2α^n`, and `2·0.85^61 ≈ 0.000098988436`
  is already below τ, so the defaults reach the threshold within 61 updates in
  exact arithmetic. The count is a bound, not a typical iteration promise.
- **No deadline.** In recall the arc work is bounded by 64 iterations over at
  most 1,024 rows. A deadline would make the same input produce different
  output, so there is none.

### Conduction rules

| Role | Conducts | Direction |
|---|---|---|
| NEXT_EPISODE, MENTIONS, RELATES_TO, HAS_MEMBER, DERIVED_FROM | yes | both ways; the stored direction is semantic |
| INVALIDATES, CONTRASTS, REPLIES_TO | no | negation, contradiction and reply addressing are not relevance conductors |

Role weights are 1 for all roles. Recall's seed-adjacent query filters on the
selected generation, so NEXT_EPISODE rows, which carry no generation, don't
enter the recall graph.

### Data structures

`exportFixedCsr` in `packages/core/src/dynamics/ppr.ts` sorts nodes ascending
and arcs by a fixed arc order, then builds `{nodes, offsets, targets, roles}`.
Production solving and external qualification use this same export, so no
result depends on caller order. Vectors are `Float64Array`. When several
physical links produce the same directed `(i, j)`, their values are summed in
that fixed order.

## 6. Cost

No timing in this set is a measurement. The sizes are bounded: a recall solve
touches at most 1,024 arcs for at most 64 iterations, and a `graph.envelope`
read inspects at most 128 × 256 probe rows.

## 7. Determinism — ordering conventions

| Place | Order |
|---|---|
| vector / BM25 candidates | score DESC, id ASC |
| session candidates | `time_utc DESC, ingest_seq DESC, id ASC` |
| degree probe | ConductingArc `(source_id, link_id)` RANGE seek, `link_id ASC`, LIMIT 256 before any filter |
| CSR node and arc order | ascending, fixed by `exportFixedCsr` |
| PPR list | p DESC, id ASC |
| primary ranking | score DESC, relevance DESC, mass DESC, id ASC |
| conflict companions | peer id ASC, LIMIT 4 (docs/05 §6) |
| delivered primaries | greedy complete-bundle packing in primary order; rank 0..n−1 afterward |
| RRF ranks | 1-based in each channel; an absent item contributes zero |

The numeric PPR kernel uses fixed-order binary64 arithmetic. With identical
CSR, seeds, runtime and options it is bit-reproducible. That isn't a guarantee
that rerunning an ANN query or reading a concurrently updated cache produces
the same input. Native transcendental calls in recall-time retention and mass
scoring may differ in the last ulp across runtimes; cross-runtime comparisons
give these values a tolerance of 1e-12. That tolerance never applies to
persisted Hit caches: the adoption kernel behind docs/04 §7 uses fixed-order
arithmetic for producer and replay, with exact cache comparison. RRF is
rank-based addition and division, not an exp/pow formula.

## 8. Why not GDS online

- GDS PPR runs on an in-memory graph **projection**. The projection must be
  rebuilt whenever the graph changes, and that cost would land in recall
  latency.
- A procedure call is a JVM round trip plus result streaming. Recall needs a
  small computation over a bounded row set.
- Determinism, retained-row normalization and dangling behavior are things
  the engine has to control itself.

GDS 2.13.12 is the pinned offline baseline for same-operator solver error
([07-gds-validation](07-gds-validation.md)). It isn't a truth oracle, and it
isn't evidence that PageRank improves retrieval; that needs separate utility
and ablation evaluation (D47).
