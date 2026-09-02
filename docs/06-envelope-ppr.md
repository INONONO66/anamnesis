# 06 — Envelope and Local PPR

The graph a recall actually sees is a **bounded envelope** two hops around the
seeds. The envelope is pulled in one Neo4j transaction, and PPR runs on it in
TypeScript. Everything outside the envelope is treated as non-existent, except
that mass which would have left the envelope is **leaked back uniformly**, to
reduce boundary distortion.

## 1. Limits

| Item | Value | Derivation |
|---|---|---|
| hops | 2 | fixed |
| seeds | ≤ 128 | docs/05 §3 |
| hop-1 node budget | 640 | |
| hop-2 node budget | 1,232 | 2,000 − 128 − 640 |
| total nodes | ≤ 2,000 | |
| total links | ≤ 20,000 | truncated on the induced subgraph |
| fanout₁ | `clamp(⌊640 / |S|⌋, 4, 32)` | inversely proportional to seed count |
| fanout₂ | `clamp(⌊1232 / |H₁|⌋, 2, 16)` | inversely proportional to hop-1 count |
| hub threshold | deg ≥ 256 | conducting-role degree |
| hub shortlist | ≤ 32 | produced by dreaming (docs/02 §6) |
| inspection bound | 128·256 + 640·256 = 196,608 | non-hub nodes have < 256 neighbors |
| envelope tx deadline | 100 ms (config) | exceeded → whole PPR channel dropped |

Budgets are hard limits. Fanout is derived from them: with 3 seeds and with
128 seeds the same fixed fanout would leave the envelope either nearly empty
or over budget. Because of the clamp minimum, the union at one hop can exceed
its budget by a small margin (e.g. |H₁| = 630 → fanout₂ = 2 → up to 1,260 >
1,232); when that happens, the hop is truncated to its budget by
`m_cache DESC, id DESC`.

## 2. Expansion

```text
  S  = 128 seeds                                           (hop 0)
  H₁ = ∪_{v∈S}  expand(v, fanout₁)  \ S                    (hop 1, ≤ 640)
  H₂ = ∪_{v∈H₁} expand(v, fanout₂)  \ (S ∪ H₁)             (hop 2, ≤ 1,232)
  V  = S ∪ H₁ ∪ H₂                                         (≤ 2,000)
  E  = conducting links of the induced subgraph on V×V, visible_gen(link),  ≤ 20,000
```

### expand(v, f)

```text
  if deg(v) >= 256:                            # hub. O(1) via COUNT{}
      return v.shortlist[0:f]  ∩ visible(T)    # no shortlist → ∅ — not expanded
  else:
      neighbors m over conducting roles, visible(m, T) ∧ visible_gen(m) ∧ visible_gen(link)
      ORDER BY link.weight DESC, m.m_cache DESC, m.id DESC
      LIMIT f
```

The third sort key is **`id DESC`** for a reason: UUIDv7 is time-ordered, so
an `id ASC` tie-break would systematically drop recent memories at every
truncation. If a bias is unavoidable, we choose the one that keeps the recent.
The first two keys (weight, m_cache) decide most cases; id only breaks ties.

`m_cache` is the mass snapshot SET daily by dreaming (docs/02 §6). It exists so
that exact m(now) is not computed for thousands of neighbors during expansion,
and it is used only for truncation order — the final score uses exact m(now)
(docs/05 §6).

### Cypher shape

```cypher
// hop-1. $frontier = seed ids, $f = fanout₁, $T, $g_e, $g_c = active[extraction|community]
UNWIND $frontier AS fid
MATCH (v:Element {id: fid})
CALL (v) {
  WITH v
  WHERE COUNT { (v)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-() } < 256
  MATCH (v)-[l:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-(m:Element)
  WHERE /* visible(m, T) ∧ visible_gen(m) ∧ visible_gen(l) — docs/03 §7 */
  RETURN m.id AS mid
  ORDER BY l.weight DESC, m.m_cache DESC, m.id DESC
  LIMIT $f
  UNION
  WITH v
  WHERE COUNT { (v)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-() } >= 256
  UNWIND v.shortlist[0..$f] AS mid
  MATCH (m:Element {id: mid}) WHERE /* visible(m, T) ∧ visible_gen(m) */
  RETURN m.id AS mid
}
RETURN DISTINCT mid
```

hop-2 has the same shape. Then one query fetches the induced-subgraph links and
each node's **true degree**:

```cypher
UNWIND $ids AS id  MATCH (a:Element {id: id})
RETURN a.id,
       COUNT { (a)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-() } AS deg
```

```cypher
MATCH (a:Element)-[l:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]->(b:Element)
WHERE a.id IN $ids AND b.id IN $ids
  AND l.gen_from <= $g AND (l.gen_to IS NULL OR l.gen_to > $g)
RETURN a.id, b.id, type(l) AS role, l.id, l.weight
ORDER BY l.weight DESC, l.id DESC
LIMIT 20000
```

The four queries form one read transaction. If it exceeds 100 ms, the
transaction is abandoned and we proceed with `ppr_used = false`.

The true degree is the conducting degree **ignoring visibility**. A
visibility-aware degree would be a subquery per node, not O(1). Ignoring it
overstates the degree → overstates the leak → errs in the conservative
direction (trusting in-envelope mass less), which we accept.

## 3. Hubs

Nodes with degree ≥ 256 are not scanned. The shortlist built by dreaming (top
32 neighbors by `m_cache × weight`, deterministic order) is used instead. If
there is no shortlist (before the first dreaming, or a newly formed hub) the
hub is not expanded — it enters the envelope **as a node** but pulls in no
neighbors.

What a hub does inside the envelope: its true degree D is large, so
`leak = 1 − Σ_j W_ij` is large, and most mass entering the hub leaks
uniformly. A hub is a drain, not a black hole. In a graph where every memory
connects through "me", this is what preserves query specificity.

## 4. Boundary normalization

```text
  W_ij  = w_ij / D_i           D_i = i's true conducting degree (including edges outside the envelope)
  leak_i = 1 − Σ_{j∈V} W_ij    the share of mass that would have left the envelope.  0 ≤ leak_i ≤ 1
```

Since `w_ij ≤ 1` and the number of in-envelope links ≤ D_i, row sums are
guaranteed ≤ 1. A node with no in-envelope neighbors (dangling) has leak = 1.

Normalizing by in-envelope degree would make boundary nodes stronger
conductors than they really are, piling mass up at the envelope's edge.
Normalizing by true degree and redistributing the leak uniformly is what makes
"a larger envelope converges toward full-graph PPR" hold — and that
convergence is what GDS measures ([07-gds-validation](07-gds-validation.md) §3).

## 5. PPR

```text
  p⁰ = s                                       (seed distribution, Σ s = 1)
  p^{k+1} = (1−α) s + α ( Wᵀ p^k + (Σ_i leak_i p^k_i) · q )      q = 1/|V| uniform
  stop when ‖p^{k+1} − p^k‖₁ < τ   or   k = maxIter

  α = 0.85,  τ = 1e-4,  maxIter = 64
```

- Σ p = 1 at every iteration (mass conservation: conducted + leaked = 1).
- **Error bound**: the operator is an α-contraction in L1, so
  `‖p^k − p*‖₁ ≤ α/(1−α) · ‖p^k − p^{k−1}‖₁ ≤ 0.85/0.15 · 1e-4 ≈ 6.7e-4`.
  The GDS validation tolerance (L1 ≤ 7e-4) comes from here.
- **maxIter 64**: `‖Δ^k‖₁ ≤ α^k · ‖Δ^0‖₁ ≤ 2 · 0.85^k` and `2 · 0.85^61 < 1e-4`,
  so 64 is a cap that guarantees τ is reached. In practice 20–30 iterations.
- **No deadline.** The size is bounded: 64 iterations × 40,000 entries ≈ 2.6M
  multiply-adds, a few ms. A deadline would make the same input produce
  different output, so there is none.

### Conduction rules

| Role | Conducts | Direction |
|---|---|---|
| NEXT_EPISODE, MENTIONS, RELATES_TO, HAS_MEMBER, DERIVED_FROM | yes | **both ways** — the stored direction is semantic |
| INVALIDATES, CONTRASTS | no | negation and contradiction are not relevance conductors |

Per-role weights start at **1.0 for all**. `config.jsonc` has a role → weight
table and it is a calibration target. A link's `weight` property multiplies the
role weight.

### Data structures

```text
  nodes     V sorted by UUID bytes ASC → index 0..|V|−1
  CSR       rowPtr Int32Array(|V|+1), colIdx Int32Array(2|E|), val Float64Array(2|E|)
  vectors   p, pNext, s, leak: Float64Array(|V|)
  deg       Int32Array(|V|)  — true degree
```

Link accumulation order: `(role bytes ASC, link UUID ASC)`. When several links
connect the same (i, j), their val is summed in that order — floating-point
sums depend on order, so the order is fixed. Both (i→j) and (j→i) are inserted
since conduction is bidirectional.

## 6. Cost

| Stage | Size | Estimate |
|---|---|---|
| hop-1 query | 128 nodes, ≤ 32,768 inspected | 5–15 ms |
| hop-2 query | ≤ 640 nodes, ≤ 163,840 inspected | 10–40 ms |
| degree + link query | 2,000 nodes, 20,000 links | 5–20 ms |
| CSR build | 40,000 entries | < 1 ms |
| PPR, 64 iterations | 2.6M multiply-adds | 1–3 ms |

The envelope tx total is what the 100 ms deadline applies to. Re-measured at
1M elements / 10M links (docs/07 §4).

## 7. Determinism — ordering conventions

| Place | Order |
|---|---|
| vector / BM25 candidates | score DESC, id ASC |
| session candidates | time_utc DESC, id ASC |
| seed selection | seed DESC, id ASC |
| expansion fanout | link.weight DESC, m.m_cache DESC, **m.id DESC** |
| hop budget truncation | m_cache DESC, id DESC |
| link truncation (20,000) | weight DESC, link.id DESC |
| CSR node order | UUID bytes ASC |
| CSR link accumulation | role bytes ASC, link.id ASC |
| PPR list | p DESC, id ASC |
| final results | score DESC, relevance DESC, mass DESC, id ASC |

PPR itself uses only `+ × ÷`, so with a fixed order it is bit-reproducible
under IEEE-754. `Math.exp` and `Math.pow` appear only in mass (docs/04) and
RRF and may differ in the last ulp across V8 versions — tests give those values
a tolerance of 1e-12.

## 8. Why not GDS online

- GDS PPR runs on an in-memory graph **projection**. The projection must be
  rebuilt whenever the graph changes, and that cost lands in recall latency.
- A procedure call is a JVM round trip plus result streaming; what we want is a
  2 ms computation on 2,000 nodes.
- Determinism, deadlines and leak normalization are behaviors we need to
  control ourselves.

GDS is used to measure **how wrong** this local PPR is — that is the subject
of 07.
