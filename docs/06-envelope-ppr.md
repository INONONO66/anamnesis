# 06 — Envelope and Local PPR

The graph a recall actually sees is a **bounded envelope** two hops around the
seeds. The envelope is pulled in one Neo4j transaction, and PPR runs on it in
TypeScript. Everything outside the envelope is treated as non-existent. Each
row is normalized over the retained visible links; a row with no retained link
uses the fixed uniform dangling distribution.

## 1. Limits

| Item | Value | Derivation |
|---|---|---|
| hops | 2 | fixed |
| seeds | ≤ 128 | docs/05 §3 |
| hop-1 node budget | 640 | |
| hop-2 node budget | 1,232 | 2,000 − 128 − 640 |
| total nodes | ≤ 2,000 | |
| outgoing retained arcs per row | ≤ 10 (`L`) | selected independently per source row |
| total directed CSR arcs | ≤ 20,000 | 2,000 × L |
| fanout₁ | `clamp(⌊640 / \|S\|⌋, 4, 32)` | inversely proportional to seed count |
| fanout₂ | `clamp(⌊1232 / \|H₁\|⌋, 2, 16)` | inversely proportional to hop-1 count |
| hub threshold | deg ≥ 256 | conducting-role degree |
| hub shortlist | ≤ 32 | produced by maintenance (docs/02 §6) |
| inspection bound | 128·256 + 640·256 + 2,000·256 = 708,608 | conservative: non-hub rows inspect < 256; hub rows read ≤ 32 indexed HubArcs |
| envelope tx deadline | 100 ms (config) | exceeded → whole PPR channel dropped |

Budgets are hard limits. Fanout is derived from them: with 3 seeds and with
128 seeds the same fixed fanout would leave the envelope either nearly empty
or over budget. Because of the clamp minimum, the union at one hop can exceed
its budget by a small margin (e.g. |H₁| = 630 → fanout₂ = 2 → up to 1,260 >
1,232); when that happens, the hop is truncated to its budget by
`coalesce(m_cache,m0) DESC, id DESC`.

If `H₁` is empty, `H₂` is defined as empty and `fanout₂` is not evaluated;
there is no division by zero.

Every query in the envelope transaction has a per-row LIMIT, so the **work**
is bounded, not only the result: no query sorts an unbounded set before
limiting. Entity and Community visibility are materialized threshold
comparisons (docs/03 §3), never nested MENTIONS/HAS_MEMBER scans.

## 2. Expansion

```text
  S  = 128 seeds                                           (hop 0)
  H₁ = ∪_{v∈S}  expand(v, fanout₁)  \ S                    (hop 1, ≤ 640)
  H₂ = ∪_{v∈H₁} expand(v, fanout₂)  \ (S ∪ H₁)             (hop 2, ≤ 1,232)
  V  = S ∪ H₁ ∪ H₂                                         (≤ 2,000)
  A  = for each v ∈ V: its top-L retained visible outgoing arcs to V
```

### expand(v, f)

```text
  if deg(v) >= 256:                            # hub. O(1) via COUNT{}
      return the first f eligible visible neighbors from HubArc rank 0..31
                                                 # no shortlist → ∅ — not expanded
  else:
      neighbors m over conducting roles, visible(m, T) ∧ visible_gen(m) ∧ visible_gen(link)
      ORDER BY w_role(link) DESC, coalesce(m.m_cache,m.m0) DESC, m.id DESC
      LIMIT f
```

The third sort key is **`id DESC`** for a reason: UUIDv7 is time-ordered, so
an `id ASC` tie-break would systematically drop recent memories at every
truncation. If a bias is unavoidable, we choose the one that keeps the recent.
The first two keys (role weight, `coalesce(m_cache,m0)`) decide most cases; id only breaks
ties.

`m_cache` is the mass snapshot SET hourly by the maintenance job
(docs/02 §6). It exists so that exact m(now) is not computed for thousands of
neighbors during expansion, and it is used only for truncation order — the
final score uses exact m(now) (docs/05 §6). It is available from the first
version that runs PPR (docs/09). Until a newly created node is maintained,
immutable total `m0` is the fallback; no null reaches a DESC comparator.

### Cypher shape

Role weights are constants, so they are passed as a map and applied with a
CASE; the generation filter picks the selector by role (HAS_MEMBER → community,
others → extraction) and lets originals-layer links (no `generation`) through.

```cypher
// hop-1. $frontier = seed ids, $exclude = seed ids, $hop_budget = 640,
// $f = fanout₁, $T, $g_e, $g_c, $w = {NEXT_EPISODE: 1.0, MENTIONS: 1.0, …}
UNWIND $frontier AS fid
MATCH (v:Element {id: fid})
CALL (v) {
  WITH v
  WHERE COUNT { (v)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-() } < 256
  MATCH (v)-[l:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-(m:Element)
  WHERE (
      (m:Episode AND m.time_utc <= $T)
   OR (m:Fact AND m.generation = $g_e AND m.time_utc <= $T)
   OR (m:Entity AND m.generation = $g_e AND m.visible_from_utc <= $T)
   OR (m:Community AND m.generation = $g_c
       AND m.source_extraction_generation = $g_e
       AND m.visible_from_utc <= $T)
  )
  AND (
       type(l) = 'NEXT_EPISODE'
    OR (type(l) = 'HAS_MEMBER' AND l.generation = $g_c)
    OR (type(l) IN ['MENTIONS','RELATES_TO','DERIVED_FROM']
        AND l.generation = $g_e)
  )
  AND NOT m.id IN $exclude
  RETURN m AS candidate
  ORDER BY $w[type(l)] DESC, coalesce(m.m_cache,m.m0) DESC, m.id DESC, l.id ASC
  LIMIT $f
  UNION
  WITH v
  WHERE COUNT { (v)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-() } >= 256
  MATCH (arc:HubArc {hub_id: v.id})
  WHERE arc.rank >= 0 AND arc.rank < 32
  MATCH (m:Element {id: arc.neighbor_id})
  WHERE (
      (m:Episode AND m.time_utc <= $T)
   OR (m:Fact AND m.generation = $g_e AND m.time_utc <= $T)
   OR (m:Entity AND m.generation = $g_e AND m.visible_from_utc <= $T)
   OR (m:Community AND m.generation = $g_c
       AND m.source_extraction_generation = $g_e
       AND m.visible_from_utc <= $T)
  )
  AND (
       arc.stream = 'cache'
    OR (arc.stream = 'extraction' AND arc.generation = $g_e)
    OR (arc.stream = 'community' AND arc.generation = $g_c
        AND arc.source_extraction_generation = $g_e)
  )
  AND NOT m.id IN $exclude
  RETURN m AS candidate
  ORDER BY arc.rank ASC
  LIMIT $f
}
WITH DISTINCT candidate
RETURN candidate.id AS mid, coalesce(candidate.m_cache, candidate.m0) AS ordering_mass
ORDER BY ordering_mass DESC, mid DESC
LIMIT $hop_budget
```

Call with `$hop_budget=640` for hop 1 and `1232` for hop 2; this is the
normative global union truncation. hop-2 otherwise has the same shape. Before fetching links, TypeScript allocates every
node in `V` with an empty adjacency list. The final query returns only retained
links; a node with no returned row therefore remains present and becomes
dangling rather than disappearing through a zero-row subquery.

```cypher
UNWIND $ids AS id
MATCH (a:Element {id: id})
CALL (a) {
  WITH a
  // A non-hub has fewer than 256 physical conducting relationships,
  // so this branch has a hard inspection bound before ORDER BY.
  WHERE COUNT { (a)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-() } < 256
  MATCH (a)-[l:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-(b:Element)
  WHERE b.id IN $ids
    AND (
        (b:Episode AND b.time_utc <= $T)
     OR (b:Fact AND b.generation = $g_e AND b.time_utc <= $T)
     OR (b:Entity AND b.generation = $g_e AND b.visible_from_utc <= $T)
     OR (b:Community AND b.generation = $g_c
         AND b.source_extraction_generation = $g_e
         AND b.visible_from_utc <= $T)
    )
    AND (
         type(l) = 'NEXT_EPISODE'
      OR (type(l) = 'HAS_MEMBER' AND l.generation = $g_c)
      OR (type(l) IN ['MENTIONS','RELATES_TO','DERIVED_FROM']
          AND l.generation = $g_e)
    )
  RETURN b.id AS bid, type(l) AS role, l.id AS lid
  ORDER BY $w[type(l)] DESC, coalesce(b.m_cache,b.m0) DESC, b.id DESC, l.id ASC
  LIMIT $L
  UNION
  WITH a
  // A hub is never expanded through MATCH. HubArc is indexed by hub/rank.
  WHERE COUNT { (a)-[:NEXT_EPISODE|MENTIONS|RELATES_TO|HAS_MEMBER|DERIVED_FROM]-() } >= 256
  MATCH (arc:HubArc {hub_id: a.id})
  WHERE arc.rank >= 0 AND arc.rank < 32
  MATCH (b:Element {id: arc.neighbor_id})
  WHERE b.id IN $ids
  AND (
      (b:Episode AND b.time_utc <= $T)
   OR (b:Fact AND b.generation = $g_e AND b.time_utc <= $T)
   OR (b:Entity AND b.generation = $g_e AND b.visible_from_utc <= $T)
   OR (b:Community AND b.generation = $g_c
       AND b.source_extraction_generation = $g_e
       AND b.visible_from_utc <= $T)
  )
  AND (
       arc.stream = 'cache'
    OR (arc.stream = 'extraction' AND arc.generation = $g_e)
    OR (arc.stream = 'community' AND arc.generation = $g_c
        AND arc.source_extraction_generation = $g_e)
  )
  RETURN b.id AS bid, arc.role AS role, arc.link_id AS lid
  ORDER BY arc.rank ASC
  LIMIT $L
}
RETURN a.id, bid, role, lid
```

The non-hub branch inspects fewer than 256 relationships. The hub branch reads
at most 32 cached tuples, filters eligibility, returns at most `L`, and
performs indexed Element lookups; it never scans hub adjacency. Each returned
row is one directed retained arc `a→b`. A
physical conducting relationship is eligible from both endpoints, but each
endpoint selects and caps its row independently; the local graph may therefore
be asymmetric after truncation. Arc identity is `(a.id,lid)`, with no global
link deduplication or forced reverse insertion. The total CSR arc count is at
most `|V|·L = 20,000`.

The three queries form one read transaction. If it exceeds 100 ms, the
transaction is abandoned and we proceed with `ppr_used = false`.

## 3. Hubs

Nodes with degree ≥ 256 are not scanned. The shortlist built by the
maintenance job for **every Element** with conducting degree ≥ 256 consists
of at most 32 indexed `HubArc` cache nodes. Their rank order is the same order
used by expansion and final arc selection. If there is no shortlist (a hub
formed since the last maintenance run), the hub selects no outgoing arc and
its row follows the uniform dangling policy. Other rows may still select arcs
into that hub; incoming arcs do not change its dangling outgoing row.

## 4. Retained-row normalization

```text
  A_i  = ordered multiset of retained arcs (i, link_id, destination)
  Z_i  = Σ_{a∈A_i} w_role(a)
  W_ij = Σ_{a∈A_i : destination(a)=j} w_role(a) / Z_i    when Z_i > 0
  d_i  = 1 if Z_i = 0, otherwise 0                       dangling indicator
```

Every non-dangling row sums to exactly one over the graph the solver actually
received. Invisible, retired and truncated relationships are absent rather
than contributing to a denominator computed from another graph. This gives
the TypeScript solver and GDS the same transition matrix. Envelope
normalization can overemphasize a boundary edge; v0.3 measures that quality
cost against full-view GDS instead of hiding it inside a non-equivalent leak
model.

Every configured `w_role` must be finite and strictly positive; configuration
loading fails before the daemon starts otherwise. Parallel physical links are
separate arcs in `A_i` and contribute separately to both `Z_i` and their
aggregated matrix cell.

## 5. PPR

```text
  p⁰ = s                                       (seed distribution, Σ s = 1)
  p^{k+1} = (1−α) s + α ( Wᵀ p^k + (Σ_i d_i p^k_i) · q )      q = 1/|V| uniform
  stop when ‖p^{k+1} − p^k‖₁ < τ   or   k = maxIter

  α = 0.85,  τ = 1e-4,  maxIter = 64
```

- Σ p = 1 at every iteration (each row is normalized or dangling).
- **Error bound**: the operator is an α-contraction in L1, so
  `‖p^k − p*‖₁ ≤ α/(1−α) · ‖p^k − p^{k−1}‖₁ ≤ 0.85/0.15 · 1e-4 ≈ 5.7e-4`.
  The GDS validation tolerance (L1 ≤ 7e-4) comes from here.
- **maxIter 64**: `‖Δ^k‖₁ ≤ α^k · ‖Δ^0‖₁ ≤ 2 · 0.85^k` and `2 · 0.85^61 < 1e-4`,
  so 64 is a cap that guarantees τ is reached. In practice 20–30 iterations.
- **No deadline.** The size is bounded: 64 iterations × 20,000 arcs ≤ 1.28M
  multiply-adds, a few ms. A deadline would make the same input produce
  different output, so there is none.

### Conduction rules

| Role | Conducts | Direction |
|---|---|---|
| NEXT_EPISODE, MENTIONS, RELATES_TO, HAS_MEMBER, DERIVED_FROM | yes | each endpoint is independently eligible to select an outgoing arc; stored direction is semantic |
| INVALIDATES, CONTRASTS | no | negation and contradiction are not relevance conductors |

Role weights `w_role` start at **1.0 for all**. `config.jsonc` has the
role → weight table and it is a calibration target. There is no per-link
weight (docs/01 §5).

### Data structures

```text
  nodes     V sorted by UUID bytes ASC → index 0..|V|−1
  CSR       rowPtr Int32Array(|V|+1), colIdx Int32Array(|A|), val Float64Array(|A|)
  vectors   p, pNext, s: Float64Array(|V|)
  dangling  Uint8Array(|V|)
```

Within each source row, arc accumulation order is `(role bytes ASC, link UUID
ASC, neighbor UUID ASC)`. When several physical links produce the same
directed `(i,j)`, their values are summed in that order. A physical link can
produce `i→j`, `j→i`, both or neither depending on the two independent row
caps.

## 6. Cost

| Stage | Size | Estimate |
|---|---|---|
| hop-1 query | 128 nodes, ≤ 32,768 inspected | 5–15 ms |
| hop-2 query | ≤ 640 nodes, ≤ 163,840 inspected | 10–40 ms |
| bounded link query | non-hub: < 256 inspected; hub: ≤ 32 cached tuples inspected, ≤ 10 returned, per node | 5–25 ms |
| CSR build | ≤ 20,000 directed entries | < 1 ms |
| PPR, 64 iterations | ≤ 1.28M multiply-adds | 1–3 ms |

The envelope tx total is what the 100 ms deadline applies to. Re-measured at
1M elements / 10M links (docs/07 §4).

## 7. Determinism — ordering conventions

| Place | Order |
|---|---|
| vector / BM25 candidates | score DESC, id ASC |
| session candidates | time_utc DESC, id ASC |
| seed selection | affinity DESC, id ASC; normalize selected set |
| expansion fanout | w_role DESC, coalesce(m.m_cache,m.m0) DESC, **m.id DESC**, link.id ASC |
| hop budget truncation | coalesce(m_cache,m0) DESC, id DESC |
| per-row arc selection (L) | w_role DESC, coalesce(neighbor.m_cache,neighbor.m0) DESC, neighbor.id DESC, link.id ASC |
| CSR node order | UUID bytes ASC |
| CSR arc accumulation | source UUID, role bytes, link.id, neighbor UUID — all ASC |
| PPR list | p DESC, id ASC |
| final results | score DESC, relevance DESC, mass DESC, id ASC |

PPR itself uses only `+ × ÷`, so with a fixed order it is bit-reproducible
under IEEE-754. `Math.exp` and `Math.pow` appear only in mass (docs/04) and
RRF and may differ in the last ulp across V8 versions — tests give those values
a tolerance of 1e-12.

A property test creates mixed maintained/unmaintained nodes and asserts that
every truncation comparator uses `coalesce(m_cache,m0)`; raw nullable
`m_cache DESC` is forbidden.

## 8. Why not GDS online

- GDS PPR runs on an in-memory graph **projection**. The projection must be
  rebuilt whenever the graph changes, and that cost lands in recall latency.
- A procedure call is a JVM round trip plus result streaming; what we want is a
  2 ms computation on 2,000 nodes.
- Determinism, deadlines, retained-row normalization and dangling behavior are
  controls we need to
  control ourselves.

GDS is used to measure **how wrong** this local PPR is — that is the subject
of 07.
