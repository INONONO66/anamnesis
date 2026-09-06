# 06 — Envelope and Local PPR

The graph a recall actually sees is a **bounded envelope** two hops around the
seeds. The envelope is pulled in one Neo4j transaction, and PPR runs on it in
TypeScript. Everything outside the envelope is treated as non-existent. Each
row is normalized over the retained visible links; a row with no retained link
uses the fixed uniform dangling distribution. This normative future operator
is distinct from retrieval utility and from the current originals/fulltext
implementation (docs/09). D43 and D47 in [10-decision-log](10-decision-log.md)
define current-policy eligibility and conditional replay.

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
| hub threshold | DegreeProbe(n) = 256 | saturated physical conducting-arc probe |
| hub shortlist | ≤ 32 | produced by maintenance (docs/02 §6) |
| probe inspections | (128 + 640 + 2,000) × 256 = 708,608 | classification before each expansion/arc row |
| structural input budget | (128 + 640 + 2,000) × 512 = 1,417,216 | conservative: probe ≤ 256 plus non-hub link-ID reads < 256, or hub tuples ≤ 32 + physical-link checks ≤ 32 per row; authority lookups separately bounded |
| envelope tx deadline | 100 ms (config) | exceeded → whole PPR channel dropped |

Budgets are hard limits. Fanout is derived from them: with 3 seeds and with
128 seeds the same fixed fanout would leave the envelope either nearly empty
or over budget. Because of the clamp minimum, the union at one hop can exceed
its budget by a small margin (e.g. |H₁| = 630 → fanout₂ = 2 → up to 1,260 >
1,232); when that happens, the hop is truncated to its budget by
`coalesce(m_cache,m0) DESC, id DESC`.

If S is empty, the PPR channel is absent: do not evaluate fanout₁, q or a
normalization denominator. If `H₁` is empty, `H₂` is empty and `fanout₂` is not
evaluated; there is no division by zero.

Every envelope query starts from a bounded input pool, not merely a final
LIMIT: ConductingArc(source_id, link_id) supplies the probe in link-ID
index order, with LIMIT 256 before any non-index sort or eligibility filter. A
plan that expands/sorts unrestricted adjacency is not a valid implementation.
Entity and Community visibility are materialized threshold
comparisons (docs/03 §3), never nested MENTIONS/HAS_MEMBER scans.

## 2. Expansion

```text
  S  = at most 128 normalized, policy-eligible seeds         (hop 0)
  H₁ = ∪_{v∈S}  expand(v, fanout₁)  \ S                    (hop 1, ≤ 640)
  H₂ = ∪_{v∈H₁} expand(v, fanout₂)  \ (S ∪ H₁)             (hop 2, ≤ 1,232)
  V  = S ∪ H₁ ∪ H₂                                         (≤ 2,000)
  A  = for each v ∈ V: bounded top-L structural arcs to V, then policy-filtered
```

### Canonical DegreeProbe(n)

DegreeProbe(n) counts the **first 256 ConductingArc rows** for source_id=n.id
in link_id ASC order. The cache has exactly one endpoint row per retained
physical NEXT_EPISODE, MENTIONS, RELATES_TO, HAS_MEMBER or DERIVED_FROM link:
ConductingArc {source_id, link_id, peer_id, role, generation?,
source_extraction_generation?} (docs/01 §5). Parallel links count separately.
The protocol rejects self-links; a defensive legacy rebuild emits a self-loop
once for its single endpoint and verify reports the violation. Under complete,
consistent coverage, values 0..255 are exact physical conducting degree; 256
is saturated and classifies a hub, never an exact count beyond the cap.

The mandatory unique composite (source_id, link_id) RANGE access path performs
source equality and ordered link iteration across all five roles. LIMIT 256
precedes collection, counting, role/time/visibility/generation/policy filtering
or any non-index sort. Rows and physical links change atomically on creation,
deletion, topology rewire and GC; rebuild publication is complete by physical
partition/generation, including hidden/retired partitions. Pin the complete
coverage state via the single Meta.conducting_arc_ready gate before probing,
not a recall-time scan of the generation registry. Missing/incomplete coverage or unavailable
indexes drops PPR as degree_probe_unavailable, never treats an unavailable
cache as degree zero. There is **no native adjacency fallback**. This cache
is nonsemantic, not authority, candidate, conductor or output; reconstruct it
from retained physical links, not a policy-filtered view (docs/01 §5).

Hidden/retired generations and denied relationships may saturate the probe;
they still cannot conduct. Seed damping uses min(physical degree,256), an
explicit approximation to eligible-degree damping (docs/05 §3). Capture the
ordered probe rows (including endpoint/role/generation fields), bounded count,
coverage state and saturation decision per stage/source.
Hidden backfill can change these without changing structure_revision, so
replay injects captured probes rather than re-counting today's adjacency.
Non-hubs resolve only captured probe rows by the appropriate per-role unique
link-ID index, checking physical endpoints, role and generation metadata.
Stale rows still occupy raw probe slots but cannot conduct; discard them with
no refill, capture the mismatch and invalidate coverage for repair. No second
adjacency expansion can race new relationships into the bounded pool. Hubs
ignore probe rows for traversal and read only HubArc ranks 0..31, with the same
bounded physical-link verification before use. Unknown cache availability
aborts PPR; a verified stale-row exclusion never authorizes a native scan.

### expand(v, f)

```text
  probe = first 256 ConductingArc rows for source_id=v.id by link_id ASC
  if size(probe) == 256:                       # saturated hub probe
      pool = first f structurally visible neighbors from HubArc rank 0..31
                                                 # no shortlist → ∅ — not expanded
  else:
      pool = neighbors m from the <256 captured probe arcs, visible(m, T) ∧ visible_gen(m) ∧ visible_gen(link)
             ORDER BY w_role(link) DESC, coalesce(m.m_cache,m.m0) DESC, m.id DESC, link.id ASC
             LIMIT f
  apply hop-union structural budget as below
  return policy-eligible pool only; no refill through unbounded adjacency
```

The third sort key is **`id DESC`** for a reason: UUIDv7 is time-ordered, so
an `id ASC` tie-break would systematically drop recent memories at every
truncation. If a bias is unavoidable, we choose the one that keeps the recent.
The first two keys (role weight, `coalesce(m_cache,m0)`) decide most cases; id only breaks
ties.

`m_cache` is the mass snapshot SET hourly by the maintenance job
(docs/02 §6). It exists so that exact m(now) is not computed for thousands of
neighbors during expansion. It controls truncation order and therefore **gates
which nodes can enter the envelope**, not merely final-score weighting. A
forgotten neighbor may lose its PPR opportunity. The final score uses exact
m(now) plus separate utility (docs/05 §6). It is available from the first
version that runs PPR (docs/09). Until a newly created node is maintained,
immutable total `m0` is the fallback; no null reaches a DESC comparator.

### Cypher shape

Role weights are constants passed as a map and looked up by `type(l)`.
The generation filter picks the selector by role (HAS_MEMBER → community,
others → extraction) and lets originals-layer links (no `generation`) through.
The following are **structural pool queries**, not sufficient serving filters.
After each bounded hop query, inside the same read transaction, the daemon
retains the resolved nodes/physical links, batch-loads their bounded authority and
applies the pinned policy evaluator before constructing the next frontier.
The final arc query receives only eligible V, then its returned arcs are also
policy-filtered before CSR. It never refills a removed slot by scanning more
adjacency; suppression can underfill the envelope until reconciliation. This
preserves inspection/node/arc caps without letting stale index/cache entries
conduct. These queries are bounded structural examples; policy evaluation is
the required daemon-side stage described here, not a fictional Cypher function.
The ConductingArc MATCH below uses its composite RANGE index: ORDER BY
arc.link_id is index order, never a Sort/Top over expanded adjacency. The
link_id IS NOT NULL predicate establishes the required second composite key
for the planner; every valid cache row has this key, so it excludes no valid
row and is not an eligibility filter. Its
inner LIMIT precedes collection, so an empty source under COMPLETE coverage
yields [] and count 0. Carry the captured row fields, not just IDs.

Each non-hub resolution subquery uses a static role branch and its unique
relationship id RANGE index. The directed relationship-index seek returns
both physical endpoints once; only then verify their IDs against source_id
and peer_id. No endpoint-bound Expand is permitted. The same five branches
resolve HubArc tuples (map hub_id to source_id and neighbor_id to peer_id)
in the daemon's bounded link-validation batch, retaining the tuple's role
and endpoint/generation fields in the query output. Reuse already resolved
non-hub physical links for policy checks; do not issue a second link read.
Verify role, endpoints and
copied generation fields, including HAS_MEMBER's source extraction generation,
before accepting a cached link. Missing/mismatched links are discarded without
refill and captured as stale; no cache row itself becomes a CSR arc.
HubArc(hub_id,rank) bounds raw cache reads to 32 before filters, with at most
32 additional per-role indexed physical-link checks. All this work stays
inside the transaction and conservative structural budget.
Entity witness thresholds are validated in the daemon-side policy stage below;
the structural Entity predicates alone are not permission to serve.

```cypher
// hop-1. $frontier = seed ids, $exclude = seed ids, $hop_budget = 640,
// $f = fanout₁, $T, $g_e, $g_c, $w = {NEXT_EPISODE: 1.0, MENTIONS: 1.0, …}
UNWIND $frontier AS fid
MATCH (v:Element {id: fid})
CALL (v) {
  CALL (v) {
    MATCH (arc:ConductingArc {source_id: v.id})
    USING INDEX arc:ConductingArc(source_id, link_id)
    WHERE arc.link_id IS NOT NULL
    RETURN arc { .source_id, .link_id, .peer_id, .role,
                 .generation, .source_extraction_generation } AS probe_row
    ORDER BY arc.link_id ASC LIMIT 256
  }
  RETURN collect(probe_row) AS probe_rows
}
WITH v, probe_rows, size(probe_rows) AS degree_probe
CALL (v, probe_rows, degree_probe) {
  WITH v, probe_rows, degree_probe
  WHERE degree_probe < 256
  UNWIND probe_rows AS row
  CALL (row) {
    WITH row WHERE row.role = 'NEXT_EPISODE'
    MATCH (lo)-[l:NEXT_EPISODE]->(hi)
    USING INDEX l:NEXT_EPISODE(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'MENTIONS'
    MATCH (lo)-[l:MENTIONS]->(hi)
    USING INDEX l:MENTIONS(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'RELATES_TO'
    MATCH (lo)-[l:RELATES_TO]->(hi)
    USING INDEX l:RELATES_TO(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'HAS_MEMBER'
    MATCH (lo)-[l:HAS_MEMBER]->(hi)
    USING INDEX l:HAS_MEMBER(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'DERIVED_FROM'
    MATCH (lo)-[l:DERIVED_FROM]->(hi)
    USING INDEX l:DERIVED_FROM(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
  }
  WITH v, row, lo, l, hi
  WHERE row.source_id = v.id AND type(l) = row.role
    AND ((lo.id = row.source_id AND hi.id = row.peer_id)
      OR (hi.id = row.source_id AND lo.id = row.peer_id))
    AND coalesce(l.generation, -1) = coalesce(row.generation, -1)
    AND (row.role <> 'HAS_MEMBER'
      OR lo.source_extraction_generation = row.source_extraction_generation)
  WITH v, row, l, CASE WHEN lo.id = v.id THEN hi ELSE lo END AS m
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
  RETURN m AS candidate, row { .source_id, .link_id, .peer_id, .role,
      .generation, .source_extraction_generation, physical_link: l } AS selected_arc
  ORDER BY $w[type(l)] DESC, coalesce(m.m_cache,m.m0) DESC, m.id DESC, l.id ASC
  LIMIT $f
  UNION
  WITH v, degree_probe
  WHERE degree_probe = 256
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
  RETURN m AS candidate, arc { source_id: v.id, .link_id,
      peer_id: arc.neighbor_id, .role, .generation,
      .source_extraction_generation, physical_link: null } AS selected_arc
  ORDER BY arc.rank ASC
  LIMIT $f
}
WITH candidate, collect(DISTINCT selected_arc) AS selected_arcs
RETURN candidate.id AS mid, coalesce(candidate.m_cache, candidate.m0) AS ordering_mass,
       selected_arcs
ORDER BY ordering_mass DESC, mid DESC
LIMIT $hop_budget
```

Call with `$hop_budget=640` for hop 1 and `1232` for hop 2; this is the
normative structural union truncation. hop-2 otherwise has the same shape.
Policy-filter the result before it becomes H₁/H₂; sort selected_arcs by
source_id/link_id before validation. Resolve only selected hub tuples by their
recorded role's unique ID index, and reuse returned non-hub physical_link
records. Require at least one of those links
from the eligible frontier to be current-policy eligible; otherwise discard
the neighbor. Do not let a denied source/link enter the next frontier. Before
fetching links, TypeScript allocates every eligible node in V with an empty
adjacency list. The final query returns a
bounded link pool, policy-filtered before normalization; a node with no
eligible returned row remains present and becomes
dangling rather than disappearing through a zero-row subquery.

```cypher
UNWIND $ids AS id
MATCH (a:Element {id: id})
CALL (a) {
  CALL (a) {
    MATCH (arc:ConductingArc {source_id: a.id})
    USING INDEX arc:ConductingArc(source_id, link_id)
    WHERE arc.link_id IS NOT NULL
    RETURN arc { .source_id, .link_id, .peer_id, .role,
                 .generation, .source_extraction_generation } AS probe_row
    ORDER BY arc.link_id ASC LIMIT 256
  }
  RETURN collect(probe_row) AS probe_rows
}
WITH a, probe_rows, size(probe_rows) AS degree_probe
CALL (a, probe_rows, degree_probe) {
  WITH a, probe_rows, degree_probe
  WHERE degree_probe < 256
  UNWIND probe_rows AS row
  CALL (row) {
    WITH row WHERE row.role = 'NEXT_EPISODE'
    MATCH (lo)-[l:NEXT_EPISODE]->(hi)
    USING INDEX l:NEXT_EPISODE(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'MENTIONS'
    MATCH (lo)-[l:MENTIONS]->(hi)
    USING INDEX l:MENTIONS(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'RELATES_TO'
    MATCH (lo)-[l:RELATES_TO]->(hi)
    USING INDEX l:RELATES_TO(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'HAS_MEMBER'
    MATCH (lo)-[l:HAS_MEMBER]->(hi)
    USING INDEX l:HAS_MEMBER(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
    UNION ALL
    WITH row WHERE row.role = 'DERIVED_FROM'
    MATCH (lo)-[l:DERIVED_FROM]->(hi)
    USING INDEX l:DERIVED_FROM(id)
    WHERE l.id = row.link_id
    RETURN lo, l, hi
  }
  WITH a, row, lo, l, hi
  WHERE row.source_id = a.id AND type(l) = row.role
    AND ((lo.id = row.source_id AND hi.id = row.peer_id)
      OR (hi.id = row.source_id AND lo.id = row.peer_id))
    AND coalesce(l.generation, -1) = coalesce(row.generation, -1)
    AND (row.role <> 'HAS_MEMBER'
      OR lo.source_extraction_generation = row.source_extraction_generation)
  WITH a, row, l, CASE WHEN lo.id = a.id THEN hi ELSE lo END AS b
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
  RETURN b.id AS bid, type(l) AS role, l.id AS lid,
      row AS cached_row, l AS physical_link
  ORDER BY $w[type(l)] DESC, coalesce(b.m_cache,b.m0) DESC, b.id DESC, l.id ASC
  LIMIT $L
  UNION
  WITH a, degree_probe
  WHERE degree_probe = 256
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
  RETURN b.id AS bid, arc.role AS role, arc.link_id AS lid,
      arc { source_id: a.id, .link_id, peer_id: arc.neighbor_id,
            .role, .generation, .source_extraction_generation } AS cached_row,
      null AS physical_link
  ORDER BY arc.rank ASC
  LIMIT $L
}
RETURN a.id, bid, role, lid, cached_row, physical_link
```

After at most 256 ConductingArc inspections, the non-hub branch resolves
fewer than 256 captured rows by per-role relationship-ID seeks, so its
eligibility sort is bounded. Never re-expand the source's native adjacency.
The hub branch reads
at most 32 cached tuples, filters eligibility, returns at most `L`, and
performs indexed Element lookups; it never scans native hub adjacency.
Each returned
eligible row is one directed retained arc `a→b`. A
physical conducting relationship is eligible from both endpoints, but each
endpoint selects and caps its row independently; the local graph may therefore
be asymmetric after truncation. Arc identity is `(a.id,lid)`, with no global
link deduplication or forced reverse insertion. The total CSR arc count is at
most `|V|·L = 20,000`.

The three structural queries and bounded policy/source batch reads form one
read transaction. If it exceeds 100 ms, abandon the entire envelope and
proceed with `ppr_used = false`; never solve a timeout-partial graph. The
1,417,216 bound counts probe inspections plus bounded link/HubArc pool reads
and hub physical-link verification (256 + 32 + 32 <= 512 for hubs;
non-hubs at most 255 + 255 <= 512), not additional bounded policy/source
lookups. Per checked Fact, at most 16 direct source Episodes plus 16
non-synthesis support Facts and their at most 16 source Episodes each means
at most 272 Episode ID lookups and 16 support Fact ID lookups before dedup.
Resolved entity bindings and EntityWitness checks use their existing bounded
materialized IDs/indexes, never native witness expansion. Evaluate at most
256 active policies and 512 scalars per
literal (docs/01-02). Policy/control Episodes and their links never conduct.
A derived node with any denied supporting source is ineligible; filtering its
source list and keeping it would silently change support. Entity/community
cache summaries must prove eligibility at the pinned policy revision or are
excluded until rebuilt; absence of such proof is not permission. An Entity's
allowed witness must itself be visible at historical T: a denied old mention
plus an allowed future mention cannot make that Entity eligible in the past.
The rebuildable earliest_allowed_from cache is the minimum visibility threshold
among allowed MENTIONS witnesses (including eligible link/source authority),
pinned to extraction generation and policy_revision. Require matching versions
and earliest_allowed_from <= T as well as structural visibility. Rebuild
before that Entity serves; missing/stale/null proof excludes it with
entity_witness_unavailable. This is one threshold per Entity/generation/policy,
not a cache per arbitrary T or a recall-time witness scan. Capture the exact
threshold/version used for replay. Community summaries likewise require their
policy-eligible rebuilt state. Content on a retained relationship is checked
too, not just its endpoints. Indexed link-ID
lookups verify cached ConductingArc/HubArc tuples still reference live links
with the recorded endpoints, role and generation before eligibility checks.

Current policy applies even at historical T and to snapshot-exempt provenance.
The policy revision barrier before response is stricter than structure_revision
retry/torn behavior (docs/05 §7); a policy race never becomes a torn-policy
result. Policy command text is not ingested instruction execution.

## 3. Hubs

Nodes with DegreeProbe(n) = 256 use only ConductingArc for classification,
never native adjacency. The shortlist built by the maintenance job for
**every Element** with saturated physical conducting-arc degree consists
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

Every non-dangling row sums to one in exact arithmetic (binary64 checks use a
stated tolerance) over the graph the solver actually received. Invisible,
policy-denied, retired and truncated relationships are absent rather
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
  for n = 1..maxIter: compute p^n from p^{n−1}
  δ_n = ‖p^n − p^{n−1}‖₁
  stop after the new iterate when δ_n < τ; otherwise return p^maxIter

  α = 0.85,  τ = 1e-4,  maxIter = 64
```

- Σ p = 1 in exact arithmetic at every iteration (each row is normalized or
  dangling). Require finite nonnegative seeds with positive total, normalize
  once in UUID order, and reject malformed CSR/weights at the solver boundary.
  An empty graph has no distribution and makes the channel absent. A one-node
  dangling graph has q=s=p=1 and converges in one update.
- **Local error bound**: F is an α-contraction in L1, so for returned p^n,
  `‖p^n − p*‖₁ <= α/(1−α) · δ_n`. At δ_n < 1e-4 this is less than
  `0.85/0.15 · 1e-4 = 0.000566666667`. Diagnostics distinguish
  `iterate_delta_l1 = δ_n`, `fixed_point_residual_l1 = ‖F(p^n)−p^n‖₁`
  if computed, and `fixed_point_error_bound_l1 = α/(1−α)·δ_n`.
  The true residual gives the alternative bound residual/(1−α).
  Neither bound measures envelope truncation or retrieval usefulness.
- **maxIter 64**: with p⁰=s, `δ_1 = α‖W′ᵀs−s‖₁ <= 2α` and
  `δ_n <= 2α^n`. `2·0.85^61 ≈ 0.000098988436` is already below τ,
  so the fixed defaults reach the stopping threshold within 61 updates in
  exact arithmetic. Report cap_reached/nonconvergence if numeric checks
  contradict it; never claim convergence solely because the cap fired.
  The default count is a bound, not a measured 20–30-iteration promise.
- **No deadline.** The arc work is bounded: 64 × 20,000 ≤ 1.28M
  multiply-adds, plus O(64·|V|) vector/dangling work. A deadline would make the same input produce
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
| hop-1 query | 128 nodes, ≤ 32,768 probe inspections + ≤ 32,768 pool reads | 5–15 ms |
| hop-2 query | ≤ 640 nodes, ≤ 163,840 probe inspections + ≤ 163,840 pool reads | 10–40 ms |
| bounded link query | ≤ 2,000 rows: ≤ 512,000 probe inspections + ≤ 512,000 pool reads; each row returns ≤ 10 | 5–25 ms |
| CSR build | ≤ 20,000 directed entries | < 1 ms |
| PPR, 64 iterations | ≤ 1.28M multiply-adds | 1–3 ms |

The separate seed-probe stage costs at most 274 × 256 = 70,144 inspections
under its 50 ms timeout (docs/05 §3). Envelope probes plus bounded pool reads
cost at most 1,417,216 structural inputs; with seed probes the per-attempt
total is 1,487,360, excluding separately bounded authority lookups. Capture
probe/pool counters and saturation state internally, not hidden physical
counts in ordinary response metadata.

The envelope tx total, including policy validation, is what the 100 ms deadline
applies to. These timings are estimates, not measurements. Scale benches use
100k and 1M Episodes (approximately 0.421M and 4.21M Elements) as defined in
docs/07 §4; record actual node/link counts rather than equating Episodes with
Elements. Wall-clock channel drops are captured degradation inputs, not
mathematically deterministic decisions.

## 7. Determinism — ordering conventions

| Place | Order |
|---|---|
| vector / BM25 candidates | score DESC, id ASC |
| session candidates | Episodes: time_utc DESC, ingest_seq DESC; Facts: time_utc DESC, id ASC; merge by time, kind, id (docs/05 §2) |
| degree probe (all stages) | ConductingArc(source_id, link_id) RANGE seek, link_id ASC, LIMIT 256 before collection or any role/time/generation/policy filtering |
| seed selection | capped-degree affinity DESC, id ASC; normalize selected set |
| expansion fanout | w_role DESC, coalesce(m.m_cache,m.m0) DESC, **m.id DESC**, link.id ASC |
| hop budget truncation | coalesce(m_cache,m0) DESC, id DESC |
| per-row arc selection (L) | w_role DESC, coalesce(neighbor.m_cache,neighbor.m0) DESC, neighbor.id DESC, link.id ASC |
| CSR node order | UUID bytes ASC |
| CSR arc accumulation | source UUID, role bytes, link.id, neighbor UUID — all ASC |
| PPR list | p DESC, id ASC |
| primary ranking | score DESC, relevance DESC, mass DESC, id ASC |
| conflict companions | raw peer ID ASC, LIMIT 65 (64 inspected + sentinel), first four policy/validity-eligible peers; nullable total if truncated (docs/05 §6) |
| delivered primaries | greedy complete-bundle packing in primary order; rank 0..n−1 afterward |
| RRF ranks | 1-based in each channel; absent item contributes zero |

The numeric PPR kernel uses fixed-order binary64 arithmetic. With identical
CSR, seeds, runtime/implementation and options it is bit-reproducible; this is
not a guarantee that rerunning ANN queries, concurrent cache reads or timeout
races produces the same envelope. `Math.exp`/`Math.pow` in accessibility and
mass scoring and `Math.log2` in hub damping may differ in the last ulp across
runtimes; cross-runtime comparisons give these values tolerance 1e-12. RRF
itself is rank-based addition/division, not an exp/pow formula.

Capture the candidate/index/coverage state, ConductingArc completeness and
stage/source ordered probe rows/counts, stale-row exclusions and saturation
decisions, earliest_allowed_from thresholds and their generation/
policy versions, m_cache/HubArc/profile and utility snapshots, selected channels
and degradation reasons, policy/config/generation versions, and actual CSR
in addition to structure_revision (docs/05 §11).
Policy changes force retry/rejection, not a determinism exception. Numeric
replay and delivered-receipt reconstruction are distinct from full retrieval
reproduction (D47).

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

GDS 2.13.12 is the pinned offline baseline for same-operator solver error and
full-view truncation comparisons (docs/07). It is not a truth oracle or evidence
that PageRank improves downstream retrieval; that needs separate utility and
ablation evaluation (D47).
