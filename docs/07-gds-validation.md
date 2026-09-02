# 07 — GDS Validation

Local PPR can be wrong in two ways: **the solver is wrong** (a different answer
on the same graph) or **the envelope is wrong** (a different answer because it
saw a different graph). The two are measured separately. GDS is the baseline
for both measurements and is never on the online path.

```text
  ① solver validation    same envelope graph  ─┬─ TS local PPR ─┐
                                               └─ GDS PPR      ─┴─ compare: L1, top-k
  ② envelope validation  full-graph GDS PPR  vs  envelope + TS PPR ─ compare: overlap@20
```

## 1. Environment

- The GDS plugin is loaded only under a **separate compose profile**
  (`anamnesis up --profile gds`). The default profile's Neo4j has no plugin —
  the online path cannot use GDS even by accident.
- Validation is the `anamnesis bench …` CLI and bypasses the daemon (its own
  bolt connection, read-only).
- Fixture graphs: synthetic (§4) and real-usage snapshots (dumps), both.

## 2. Solver validation (v0.2)

**Question**: is our PPR implementation mathematically correct?

GDS personalized PageRank takes a *set* of source nodes with equal teleport
probability, not a weighted seed vector, and handles dangling mass its own
way. Both differences are removed on the graph side rather than by loosening
the tolerance.

```text
  1. Run recall with diagnostics.dump_envelope = true → save V, E, s, leak, deg
  2. Build a GDS in-memory graph on V ∪ {σ}
       · i → j  for every ordered pair in V×V, weight  W_ij + leak_i / |V|
         (W_ij = w_ij / D_i as in docs/06 §4; the uniform leak is folded into a dense row.
          Each row sums to exactly 1, so GDS's own out-weight normalization is a no-op.
          |V|² ≤ 4M relationships — fine offline)
       · σ → i  weight s_i   (Σ s = 1)
  3. gds.pageRank.stream(sourceNodes = [σ], dampingFactor = 0.85,
                         relationshipWeightProperty, tolerance = 1e-9, maxIterations = 1000)
  4. Take p_gds restricted to V and divide by α (equivalently, renormalize to Σ = 1) → compare
```

Why step 4 is exact: with the teleport going to σ alone, `p_σ = 1 − α` at the
fixed point, and for j ∈ V
`p_j = α (Σ_i W′_ij p_i + s_j p_σ) = α(1−α) s_j + α Σ_i W′_ij p_i`, i.e.
`p_V = α(1−α)(I − αW′ᵀ)⁻¹ s = α · p*`. The GDS vector on V is exactly α times
our fixed point.

Fallback if the σ construction is ever unavailable: PPR is linear in s, so
`p(s) = Σ_i s_i · p(e_i)` — run once per seed (≤ 128 runs) and combine.

### Acceptance

| Criterion | Value | Basis |
|---|---|---|
| `‖p_ts − p_gds‖₁` | ≤ 7e-4 | error bound 6.7e-4 at τ = 1e-4 (docs/06 §5) + GDS residual |
| top-k set, k ∈ {10, 20, 50} | identical when the **boundary is clear** | the k-th and (k+1)-th p differ by > 2·7e-4 |
| top-k set, boundary close | overlap@k ≥ 0.95 | difference ≤ 2·7e-4 — either order is legitimate |
| NDCG@k (gain = p_gds) | ≥ 0.999 | rank flips, if any, lose a negligible amount of reference score |

The clear/close distinction exists because two solvers that are both within
tolerance may legitimately order two nodes whose p differ by less than 7e-4
differently. Counting that as failure would make the validation measure noise.

### Execution

In CI, on 20 synthetic graphs (fixed seed), every PR. Real-usage dumps
nightly. A failure is either a solver bug or a normalization-definition
mismatch, and both are fixed in code — the thresholds are not loosened.

## 3. Envelope validation (v0.3)

**Question**: how well does the 2-hop / 2,000-node truncation reproduce
full-graph PPR?

```text
  1. Full-graph projection (visible(T) and visible_gen applied, conducting roles, both directions, w = weight)
       · run with all role weights = 1.0 (the default). Then W_ij = 1/D_i everywhere, there is no leak
         on the full graph, and GDS's degree normalization coincides with ours
       · add σ → seeds with weight s_i as in §2; sourceNodes = [σ]; divide the result on V by α
  2. gds.pageRank.stream(personalized) → p_full
  3. recall's envelope + TS PPR → p_local (0 outside the envelope)
  4. Compare
```

Seeds with conducting degree 0 (isolated) are excluded from the validation
set — GDS's dangling treatment and ours need not agree for them, and they
cannot spread anyway.

### Metrics

| Metric | Target | Meaning |
|---|---|---|
| overlap@20 | ≥ 0.80 | share of full-PPR top 20 present in local top 20 |
| overlap@50 | report only | |
| envelope recall | report only | share of full top 50 inside the envelope — what truncation missed |
| Σ_{V} p_full | report only | share of full-PPR mass the envelope contains |
| total leak Σ leak_i p_i | report only | how much boundary normalization did |

If overlap@20 < 0.8, **do not raise the limits first** — look at the cause:
hub shortlist quality, fanout ordering bias, seed distribution. Raising limits
trades against the latency budget and is the last resort.

### Query distribution

Seed distributions are sampled from real recall logs (receipt mode). Synthetic
queries have seeds that are too even or too clustered to resemble real ones.

## 4. Scale benches (v0.3)

Synthetic graph generator `anamnesis bench gen --episodes N`:

- Episodes N, Facts ≈ 3N, Entities ≈ 0.2N (Zipf mention distribution → hubs
  emerge naturally), Communities ≈ 0.01N
- log-normal session lengths, NEXT_EPISODE chains
- times uniform over a 2-year span, 5 % backdated Facts
- Hit ledger: recency-biased sample → caches regenerated by replay

| N | Nodes | Links | Measures |
|---|---|---|---|
| 100k | ~0.4M | ~2M | recall p50/p95, envelope tx distribution, torn rate |
| 1M | ~4M | ~20M | same + Neo4j page cache size, cold-start time, dreaming time |

Targets (docs/05 §9): recall p50 < 100 ms, p95 < 250 ms at 1M. Envelope tx
over the 100 ms deadline in < 1 % of recalls.

## 5. Other GDS uses (offline)

| Use | Algorithm | Where the result goes |
|---|---|---|
| dreaming communities | Leiden | community generation (docs/02 §6) |
| global centrality | PageRank, betweenness | reports only. Not in online scores — degree is enough for the hub test |
| graph health | WCC, degree distribution | `anamnesis bench health` report |

## 6. CI gate summary

| Gate | When | On failure |
|---|---|---|
| forgetting fixtures (docs/04 §10) | every PR | blocks merge |
| PPR unit: convergence, mass conservation, determinism (same input twice → bit-identical) | every PR | blocks merge |
| RRF scale invariance (channel scores × c → same ranking) | every PR | blocks merge |
| ordering conventions (all of docs/06 §7 as property tests) | every PR | blocks merge |
| solver validation, 20 synthetic | every PR (GDS container) | blocks merge |
| solver validation, real dumps | nightly | auto-files an issue |
| envelope validation | nightly | report. Persistent overlap@20 < 0.8 → issue |
| scale benches | before release | report |
