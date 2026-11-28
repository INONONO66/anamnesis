# 07 — GDS Validation

Three questions stay separate: **solver error** on the same retained operator,
**truncation error** from reading a smaller graph than the full one, and
**retrieval utility** to a caller. GDS is a numeric baseline for the first, not
a truth or utility oracle, and it is never on the online path (D47). Only the
solver comparison and the access-path fixtures below exist in the repository.
Truncation validation, scale benches and the GDS uses that depend on dreaming
are [09-roadmap](09-roadmap.md) targets.

```text
  ① solver validation    same captured graph ─┬─ TS local PPR ─┐
                                              └─ GDS PPR      ─┴─ compare: L1, top-k
  ② truncation           full-view GDS PPR vs recall's bounded graph   (docs/09)
  ③ retrieval evaluation delivered bundles vs baselines                (scripts/eval, §7)
```

## 1. Environment

- The live authority never loads GDS and never grants a second Bolt client.
  Validation starts a disposable, loopback-only Neo4j+GDS container owned by
  the test runner and loads a fixture.
- **GDS 2.13.12** and a compatible Neo4j 5.26 release are pinned, with their
  artifact digests and procedure configuration recorded in the run's evidence.
  A version mismatch is refused. Nothing here is claimed about master or latest.
- The container has its own credential and temporary volume, no live
  credentials and no live write path, and is removed after the run.
- An unavailable GDS reports UNKNOWN with its reason. It's never a pass.

## 2. Solver validation

**Question**: is the PPR implementation mathematically correct?

The pinned baseline invokes personalized PageRank with the singleton
`sourceNodes=[σ]`; it does not depend on undocumented weighted-source support.
The virtual source supplies the weighted seed. Explicit uniform edges are
added for dangling envelope rows so GDS and TypeScript use exactly the same
operator. Here **α = dampingFactor = 0.85**; teleport is 1−α, not α.

```text
  1. Authenticated offline bench capture → save policy-eligible V, retained directed A,
     role weights, normalized s, dangling, captured ConductingArc rows/coverage,
     stale-row exclusions, degree probes and witness thresholds,
     cache/candidate/config/revision state
  2. Build a GDS in-memory graph on V ∪ {σ}
       · for each non-dangling i: i → j with weight w_role for every retained arc;
         GDS normalizes by Σ retained role weights, exactly docs/06 §4
       · for each dangling i: i → every j ∈ V with weight 1/|V|
         (worst case |V|² ≤ 4M relationships — fine offline)
       · σ → i with positive weight s_i only (Σ s = 1); zero seeds need no edge
       · σ has no incoming edges and is not a dangling destination
  3. gds.pageRank.stream(sourceNodes = [σ], dampingFactor = 0.85,
                         relationshipWeightProperty = 'weight', concurrency = 1,
                         tolerance = 1e-9, maxIterations = 1000)
  4. Restrict raw GDS scores to V, then normalize by their observed sum on V → p_gds
     (reject nonfinite/negative scores or nonpositive sum); compare and verify residual
```

Why step 4 works: for the **probability-normalized augmented fixed point**,
`p_σ = 1−α` and, for j in V,
`p_j = α Σ_i W′_ij p_i + α s_j p_σ`. Thus
`p_V = α(1−α)(I−αW′ᵀ)⁻¹s = α·p*` and `Σ_V p_V = α`.
This proof concerns a normalized probability vector, not an assumption that
raw GDS scores sum to 1. If GDS scales the raw vector by any positive constant,
normalizing its restriction to V removes that scale. **Do not divide arbitrary
raw scores by α.** Record raw total, raw V total and measured normalized
residual; convergence tolerance alone is not proof of operator equivalence.

Independently compute `r_gds = ‖(1−α)s + αW′ᵀp_gds − p_gds‖₁` using the
exported local operator, require `r_gds <= 1e-8`, and bound its error by
`r_gds/(1−α)`. Dense small-graph linear solves and hand-solvable fixtures are
independent references for the projection and σ construction. If the σ
construction is unavailable, linearity gives `p(s)=Σ_i s_i·p(e_i)`; at most
128 normalized, residual-checked singleton runs can replace it, recorded as a
different baseline path rather than a silent fallback.

The runner is `scripts/qa/gds-solver-20.runner.ts` over the twenty
fixtures in `gds-solver-20.fixtures.ts`; the independent oracle is
`scripts/qa/gds-ppr-oracle.ts`, which shares no numeric code with the
production solver. Both export the graph through the same fixed CSR as
docs/06 §5.

### Acceptance

| Criterion | Value | Basis |
|---|---|---|
| finite scores / mass | nonnegative; `abs(Σp−1) <= 1e-12` | probability invariant |
| reference operator residual | `r_gds <= 1e-8` | measured after V normalization |
| `‖p_ts − p_gds‖₁` | ≤ 7e-4 and consistent with recorded residual bounds plus binary64 margin | TS error < 5.666667e-4 plus GDS error ≤ 6.666667e-8 |
| top-k set, k ∈ {10, 20, 50}, k < node count | identical when the **boundary is clear** | reference k-th minus (k+1)-th score > 2·7e-4 is a conservative sufficient gap |
| top-k set, close/tied boundary | raw overlap **report only**; tie-aware membership check | no minimum raw overlap follows from an L1 bound |
| NDCG@k (linear gain = p_gds) | report only | near ties or low gains do not imply an unconditional 0.999 guarantee |

Use ID ASC to report deterministic top-k sets, but do not mistake that tie-break
for an error bound. With δ=7e-4 and reference kth score c, tie-aware acceptance
requires every node above c+2δ to be included and every selected node to have
reference score at least c−2δ; only the boundary band may vary. Use
k'=min(k,|V|), with no boundary gap when k'=|V|; empty graphs are channel-absent,
not a division-by-zero overlap. Report k' and the boundary-band size.

Counterexample to an overlap guarantee: let 40 nodes each have reference
probability 1/40. A second vector adds 1e-6 to 20 nodes and subtracts 1e-6
from the other 20. Both sum to 1, their L1 difference is only 4e-5, yet the
chosen top-20 sets can be disjoint (overlap=0). Small residual/error proves
numeric proximity, not rank-set stability.

### Execution

The twenty fixed-seed graphs include singleton and all-dangling graphs, a
two-cycle, disconnected components, unequal seeds, parallel arcs and near-tied
ranks. Malformed seeds (NaN, infinity, negative or zero total), invalid CSR
indices and nonpositive or nonfinite role weights are rejected before
arithmetic. A projection or normalization mismatch is reported separately from
a solver failure, and thresholds are never loosened to hide either. Runs await
explicit container and job completion with a bounded timeout, never fixed
sleeps.

## 3. Envelope validation

Not built. Comparing full-graph PPR with recall's bounded graph read needs a
fixed online graph read to compare against; see [09-roadmap](09-roadmap.md).

## 4. Scale benches

Not built. No scale number in this set is a measurement
([09-roadmap](09-roadmap.md)).

## 5. Other GDS uses (offline)

None ship. Community detection for dreaming is a
[09-roadmap](09-roadmap.md) target. `scripts/qa/gds-startup-diagnostic.ts`
checks that a GDS container starts and admits a projection; it isn't a solver
test.

## 6. CI gate summary

The repository has no CI workflow files. These are the commands that gate a
change:

| Command | What it runs |
|---|---|
| `bun run gate` | `tsc --build`, ESLint, knip, madge (no import cycles), the `any`/`unknown` gate, jscpd (no duplication) and `bun test --coverage` over `scripts/qa/pure-tests.txt` |
| `bun run test` | builds the runtime, then the `foundation` case of `scripts/qa/runtime-scenarios.ts`: every test under `packages/` and `scripts/qa/` plus the two runtime test roots |
| `bun scripts/qa/runtime-scenarios.ts --case <name>` | one named scenario set (`CASE_TEST_PATHS`), including the owned-Neo4j cases; run database cases serially |
| `bun run schemas` | regenerates `packages/protocol/schemas/`; a diff after it means the committed schemas are stale |
| `bun run test:runtime` | the built Node process tests in `app/anamnesis/process.test.mjs` |

The GDS solver cases, the ConductingArc probe and plan fixtures, forgetting
and replay fixtures, budget and packing fixtures, policy and outcome fixtures
and the digest-version round trips are tests inside these runs. A behavior
not covered by a test in these runs is not a gate.

## 7. Retrieval utility, calibration and replay (D47)

A numerically correct PPR operator can still retrieve the wrong material.
Utility is evaluated on **delivered primary bundles**, not pre-budget ranks.
`scripts/eval/recall-eval.ts` runs labelled queries against a live daemon and
compares them with a BM25 baseline. Utility `U` and adoption are
observational proxies, not truth or causal labels, and a whole-recall reward
is one dependent observation even when its weight is spread over many
Episodes.

Rules for any evaluation run:

- Don't tune on the evaluation split. Split chronologically and by source or
  session, so shared-source siblings and repeat occurrences can't leak across
  splits.
- Report grouped uncertainty, support counts, the unreported-Usage rate and
  channel and degradation cohorts.
- Missing reward is not a negative label; reward zero is observed.
- Without randomized exposure or recorded propensities, report associations,
  not causal benefit.
- No constant is refitted automatically from a few verdicts. Changing an
  immutable `m₀` input needs a new derived generation; replay never rewrites
  `m₀`.

Replay has three levels, and a report says which one it reached:

1. **Numeric replay**: captured CSR, seeds, options and runtime reproduce the
   local solver, with iterate delta, residual, mass conservation and error
   bound.
2. **Usage attribution**: the retained primary order and source IDs
   reconstruct the outcome shares. Reproducing the exact delivery needs the
   harness to capture the response itself.
3. **Full retrieval reproduction**: additionally needs captured candidate,
   index, cache, policy and generation state. `structure_revision` alone can't
   provide it; label missing capture explicitly instead of calling a rerun
   deterministic.

### What the frozen screens do and do not establish

One development-scoped comparison backs the query default. It is not a deployment result.

- [research/retrieval-fusion](research/retrieval-fusion.md) compared
  original-query union recall with equal-weight two-query RRF on a small
  source-informed development set: 20/20 versus 19/20 Hit@1, saturated Hit@3,
  human translations, seven substantive targets and no realistic distractor
  population. It supports keeping the caller's query verbatim as a reversible
  default and nothing stronger in either direction (D48).

A production qualification package for multilingual behavior, the extractor,
the judge or an embedding index is a separate gate. Each needs its own
machine-validated manifests, predeclared quality and resource thresholds and
an explicit decision; this document invents none of those numbers, and an
`ONLINE` index or an operator attestation cannot substitute for one (D51).

Five packages carry that burden (M1 multilingual retrieval and detail
coverage, M2 quote and source fidelity, M3 independent adjudication gold and
causal ablation, M4 CPU context, throughput and contention, M5 Q8_0/FP16 and
Neo4j index parity with the cutover gate) are **trigger-based admission work,
not completed results and not a queued backlog**. Run the package for a
capability at the moment that capability is actually being admitted to
production, and treat its absence as the reason the corresponding default
stays development-scoped and reversible.

### ConductingArc access-path fixtures

Degree boundaries use physical conducting-link counts 0, 255, 256, 257 and a
large hub, interleaving all five roles and parallel link IDs, so neither
per-role caps nor peer deduplication can pass. The fixtures assert one row per
distinct endpoint and link, unique `(source_id, link_id)`, no INVALIDATES or
CONTRASTS rows, copied generation metadata and exact equality with the
independently enumerated physical graph. The protocol rejects a self-link
before any physical or cache write, and a fixture asserts that rejection.

They require exactly `min(degree, 256)` link-ID-ordered rows, saturation at
256 and bounded collection even when empty, with no role, time, generation or
policy predicate before the raw cap. At 257, hidden or denied links among the
first 256 and an eligible link at position 257 must not refill the probe.
Missing or incomplete coverage or indexes fail as `degree_probe_unavailable`
or `ordered_probe_unavailable`, never as degree zero, and no native adjacency
query runs in any missing-cache case. EXPLAIN/PROFILE plans must show the
composite RANGE seek with `link_id ASC` index order and `LIMIT 256` before
collection; a Sort/Top over unrestricted rows, a native Expand or a label scan
fails whatever the row count. Create, delete and session-backfill rewire
compare physical links and endpoint rows in the same committed transaction.
Stale rows (deleted link, wrong peer, wrong role, mismatched generation) are
excluded without refill and still occupy their probe slots. Fixtures subscribe
to the publication event before triggering a write and await it with a bounded
timeout. They live in `scripts/qa/graph-conducting-maintenance.*`,
`graph-ordered-probe.*` and `graph-envelope-access.test.mjs`.
