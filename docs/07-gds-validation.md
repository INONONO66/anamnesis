# 07 — GDS Validation

Three questions must remain separate: **solver error** on the same retained
operator, **truncation error** from a different envelope/operator, and **retrieval
utility** to a caller. GDS is a numeric baseline for the first two, not a truth
or downstream-utility oracle, and is never on the online path. D47 in
[10-decision-log](10-decision-log.md) defines this separation; D43-D46 supply
policy, budget, feedback and occurrence/conflict contracts being evaluated.
All gates and targets here are normative future work, not claims that the
current originals/fulltext code implements or has passed them (docs/09).

```text
  ① solver validation    same envelope graph  ─┬─ TS local PPR ─┐
                                               └─ GDS PPR      ─┴─ compare: L1, top-k
  ② envelope validation  full-view GDS PPR vs envelope + TS PPR ─ compare: L1, overlap, boundary mass
  ③ retrieval evaluation delivered budgeted bundles vs baselines ─ task utility, faithfulness, coverage
```

## 1. Environment

- The live authority never loads GDS and never grants a second Bolt client.
  `anamnesis bench …` starts a disposable, loopback-only Neo4j+GDS container
  and loads a synthetic fixture or verified point-in-time dump.
- Pin **GDS 2.13.12**, a compatible Neo4j 5.26 release, exact container/JAR
  SHA-256 digests, procedure config, projection code and runtime in the bench
  manifest. Refuse version mismatch; no claim about master/latest behavior.
  Upgrading the baseline requires rerunning operator fixtures, not assuming
  sourceNodes/damping/normalization contracts stayed unchanged.
- The disposable container has a distinct random credential and temporary
  volume, network disabled except the isolated local control channel, no live
  credentials and no live write path. Destroy it after results/log capture.
  Export/import and control RPCs are authenticated explicit operator commands;
  ingested content never starts a benchmark or policy command.
- Fixture graphs: synthetic (§4) and redacted/locally retained real-usage
  snapshots (dumps), both.

## 2. Solver validation (v0.2)

**Question**: is our PPR implementation mathematically correct?

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

Planned CI runs 20 fixed-seed synthetic graphs per PR; real-usage snapshots
nightly. Include singleton and all-dangling graphs, two-cycle, disconnected
components, unequal seeds, parallel arcs, asymmetric row caps, maintained and
missing hub shortlists, generation/time/policy exclusions and near-tied ranks.
Reject malformed seeds (NaN, infinity, negative or zero-total), invalid CSR
indices and nonpositive/nonfinite role weights before arithmetic. Separate
projection/normalization mismatch from solver failure; never loosen thresholds
to conceal either. Await explicit container/job completion with a bounded
timeout, never fixed sleeps or timing-dependent assertions.

## 3. Envelope validation (v0.3)

**Question**: how well does the 2-hop / 2,000-node truncation reproduce
full-graph PPR?

Let `U` be the complete visible, **current-policy-eligible** vertex set for the
pinned snapshot and generation pair. `V ⊆ U` is the local envelope. The full
operator uses all eligible conducting links, independent of m_cache, hub
shortlists and row caps; local normalization uses only retained arcs. Keep the
same normalized seed vector and weights in both. This comparison measures
truncation/renormalization, not a second solver-quality criterion.

```text
  1. Full-view projection on U (visible(T), visible_gen and current policy applied, conducting roles, both directions,
     relationship weight = w_role)
       · GDS normalizes each row over exactly the visible projected links, the
         same retained-row rule used by local PPR. Any positive role weights are fine
       · each dangling row has explicit uniform transitions to U, not to σ
       · add σ → seeds with weight s_i as in §2; sourceNodes = [σ]; normalize scores on U
  2. gds.pageRank.stream(personalized) → p_full
  3. recall's envelope + TS PPR → p_local (0 outside the envelope)
  4. Compare
```

Do not silently exclude isolated seeds: that changes the real query
distribution and dangling operator. Explicit full-view dangling rows may cost
`|dangling(U)| × |U|` offline relationships. The manifest declares projection
byte/relationship limits; if this exceeds them, report `oracle_unavailable`
for that query, not a fabricated exact GDS result. A separately labelled
non-isolated cohort can be reported, never substituted for the all-query
cohort. Independently evaluated sparse full-view power iteration with uniform
dangling redistribution is a reportable alternate reference, not a GDS run.

### Metrics

| Metric | Target | Meaning |
|---|---|---|
| overlap@20 | aspirational ≥ 0.80; report, not proof | share of full-PPR top 20 present in local top 20; report tie-aware variant |
| overlap@50 | report only | |
| envelope recall | report only | share of full top 50 inside the envelope — what truncation missed |
| Σ_{V} p_full | report only | share of full-PPR mass the envelope contains |
| boundary mass | report only | `Σ_{i∈U\V} p_full(i)` |
| `‖p_local−p_full‖₁` / full-operator residual of p_local | report only | local p is zero-extended to U; distinct from same-envelope solver residual |
| oracle coverage | report only | eligible queries, unavailable projections and reasons, isolated-seed cohort |

If overlap@20 < 0.8, **do not raise the limits first** — look at the cause:
hub shortlist quality, fanout ordering bias, seed distribution. Raising limits
trades against the latency budget and is the last resort.

### Query distribution

Sample seed distributions from retained RecallReceipts, preserving chosen
channels, unavailable-channel reasons, output budget and policy/config versions.
Do not evaluate only adopted or successful recalls. Report snapshot selection,
privacy/redaction changes and receipt expiry/missingness; none is a negative
label. Fixed-seed synthetic queries complement, not replace, this distribution.

## 4. Scale benches (v0.3)

Synthetic graph generator `anamnesis bench gen --episodes N`:

- Episodes N, Facts ≈ 3N, Entities ≈ 0.2N (Zipf mention distribution → hubs
  emerge naturally), Communities ≈ 0.01N
- log-normal session lengths, NEXT_EPISODE chains
- times uniform over a 2-year span, 5 % backdated Facts
- Hit ledger: recency-biased sample → caches regenerated by replay

| N | Nodes | Links | Measures |
|---|---|---|---|
| 100k | ~0.421M Elements | ~2M | recall p50/p95, envelope tx distribution, structural torn and policy-retry rates |
| 1M | ~4.21M Elements | ~20M | same + page cache, cold start, dreaming, receipt-write latency and size |

Counts follow N+3N+0.2N+0.01N=4.21N; ledger, policy and cache/control nodes
are additional and reported separately. Link counts are generator targets,
not inferred exact counts. Record actual counts, hardware/runtime, warm/cold
state, concurrency, query distribution, budget units and policy-rule count.
Record internal per-stage ConductingArc probe rows, coverage/publication state,
saturation counts, per-role link-ID seeks, stale-row exclusions, link/HubArc pool
reads, seed-probe timeouts and missing witness-cache exclusions. Assert
≤ 70,144 seed probe inspections, ≤ 708,608 envelope probe inspections and
≤ 1,417,216 envelope probe-plus-pool reads per attempt (≤ 1,487,360 including
seeds); report separately bounded authority lookups and retry multipliers.
Inspect EXPLAIN/PROFILE plans at both scales: MATCH ConductingArc must seek
the composite (source_id, link_id) RANGE index with source equality and
link_id ASC index order, then LIMIT 256 before collection/counting or any
role/time/generation/policy filter. A Sort/Top fed by unrestricted source rows,
native Expand, label scan or all-relationship scan fails regardless of final
row count or elapsed time. Record rows/db-hits at the index and Limit
operators, not just returned counts; planner hints alone do not prove order.
Non-hub captured-row resolution must use the row's static role branch and
that role's unique relationship id index (at most one physical link per row),
then verify endpoints/role/generation, with no second adjacency expansion.
HubArc raw range reads are at most 32 plus at most 32 such link-ID checks.
If any required cache/index/ordered plan is unavailable, assert whole-PPR
degradation to channel-only fusion; native adjacency is never an alternative.
Docs/05 targets p50 < 100 ms and p95 < 250 ms at a **1M-Element** reference
scale. Run that scale explicitly as well as these Episode-count stress sizes;
do not relabel 1M Episodes as 1M Elements. Target envelope timeouts below 1%
of recalls; report whole-channel drop rates rather than hiding them in latency.
These are unmeasured design targets, not a claim of resident GDS execution.

## 5. Other GDS uses (offline)

| Use | Algorithm | Where the result goes |
|---|---|---|
| dreaming communities | Leiden | community generation (docs/02 §7) |
| global centrality | PageRank, betweenness | reports only. Online hub classification uses saturated DegreeProbe, not full degree |
| graph health | WCC, degree distribution | `anamnesis bench health` report |

## 6. CI gate summary

| Gate | When | On failure |
|---|---|---|
| forgetting fixtures (docs/04 §10) | every PR | blocks merge |
| PPR unit: convergence, mass conservation, deterministic fixed captured CSR/seeds/runtime | every PR | blocks merge |
| RRF scale invariance (channel scores × finite c > 0 → same ranking; no overflow), 1-based ranks and relevance ≤ 1/61 | every PR | blocks merge |
| ordering conventions (all of docs/06 §7 as property tests) | every PR | blocks merge |
| ConductingArc coverage/ordered probes and historical Entity witnesses | every PR once implemented | blocks merge: composite RANGE/Limit plan, atomic physical-link cache maintenance, missing-cache PPR degradation, per-role bounded stale-link checks, captured saturation replay, no future-only allowed witness at past T |
| solver validation, 20 synthetic | every PR (GDS container) | blocks merge |
| solver validation, real dumps | nightly | auto-files an issue |
| envelope validation | nightly | report. Persistent overlap@20 < 0.8 → issue |
| budget/receipt contracts (D44-D45) | every PR once implemented | blocks merge: exact renderer count, rank packing, source credit conservation, expiry/restart/idempotency |
| policy/conflict/occurrence contracts (D43/D46) | every PR once implemented | blocks merge: no denied serving/conduction/feedback, bounded companions and explicit incomplete/redacted state |
| language and evidence contracts (D48) | every PR once implemented | blocks merge: required immutable `content_language`; an `en` generation rejecting `language_policy_mismatch` with no per-claim source fallback; a projection-derived name (`Jihyun`, `(Sato)`) rejected as an Entity alias and as `normalized_name`; Latin `a`/`c`/`e`/`o`/`p`/`x` distinguished byte-for-byte from their Cyrillic homoglyphs in identifiers, so a homoglyph swap fails `evidence_mismatch` rather than matching; literal `evidence_quote` of 1..8,192 bytes with the derived `[start,end)` span and a repeated quote without a valid span rejected; `RecallRequest.query` byte-identical on the BM25 and vector channels with no second generated query |
| lineage and grouping contracts (D49) | every PR once implemented | blocks merge: version-1 verify/retry/journal-replay/rebuild/backup round trips byte-stable with zero SETs while a changed version-1 field still conflicts, a re-spool of that same revision from a version-2-aware daemon draining as the same version-1 no-op because the stored row's version wins, a versionless acknowledged journal record still creating the version-1 Episode it promised, and version-2 `origin_role`-only or lineage-body-only changes under one `revision_key` rejecting `revision_conflict`; bounded parents (≤4), roots (≤16) and depth (≤8) with overflow storing `unknown` + `echo_lineage_truncated`; the receipt's stored `selection_digest` equal to the value `EchoLineage.context_digests` copies; an unknown-lineage assistant **Episode** and every output derived from it ineligible for candidates, synthesis and invalidation; a complete synthesis support union storing `context_derived` with `max(support.echo_depth)` and no added hop, and an incomplete or over-cap union storing `unknown` + truncated; echo fields absent from every score, ordering and conflict decision; `same_scope_l1b` (equal non-null `correction_scope_text` with differing corrected value) accepted where `same_scope_group` (complete byte-identical RFC-8785 `scope`) is refused, and neither predicate substituted for the other; `subject_keys` null rather than a literal fallback; symmetric non-transitive `known_conflict` |
| adjudication and correction contracts (D50) | every PR once implemented | blocks merge: shadow mode writes no Fact or edge; a failed call leaves no proposal; attempt and proposal both persisting `source_head_revision_key` and `policy_revision`, and the proposal persisting the complete `proposed_claim_digest` over the enumerated L1 object; W1 rejecting on a changed stored head key, a changed stored policy revision, a claim that no longer reproduces the digest, or a changed candidate digest, without inferring proposal-time state from the current graph; single-use consumption; a correction map covering every correction under policy, including a denied invalidator whose content-free marker is still repairable by ID; correction appending without deleting history and never fabricating user prose; A-prime keeping `A.time` |
| embedding identity, failure and coverage contracts (D51) | every PR once implemented | blocks merge: the three canonical fingerprints recomputing to `711660f8…96e13e9`, `0dfe3d3a…bef81a9` and `16d404a7…795405b`, with a quantization or HNSW change producing different IDs; emitted DDL matching `vec_episode_<indexhex>` / `vec_fact_g<N>_<indexhex>` / `vec_rel_g<N>_<indexhex>` and the exact options object (`vector-2.0`, 1024, cosine, quantization disabled, `hnsw.m=16`, `ef_construction=100`); every failure transition including `worker_lost` lease loss and `PENDING|RUNNING|RETRY_WAIT|BLOCKED → CANCELLED`, with `malformed_response` and `client_error` blocking immediately and only the four transient classes retrying three times at `[1000, 10000]` ms; coverage stopping at the hole; a vector-channel recall whose active profile has both an `(episode,0)` and an `(extraction,N)` partition simultaneously BLOCKED with different cursors and different omission digests recording **both** rows in `embedding_coverages`, sorted `episode` before `extraction`, each with its own `required_ingest_seq` (current `Meta.ingest_seq` for the Episode row, the active generation's current `covered_ingest_seq` for the extraction row) and exact `lag`, on the response diagnostics and the durable receipt alike, with a one-row array only when no active extraction generation exists and a missing applicable row refusing the vector channel instead of serving a partial array; cutover refused unless target-model episode coverage equals **current** `Meta.ingest_seq` and extraction coverage equals the active generation's current `covered_ingest_seq`; the zero-skip production default rejecting any nonzero skip that no qualification declared; authenticated retry/skip/cancel records; production activation refused on ONLINE status or operator attestation alone |
| utility/calibration ablations (§7) | versioned evaluation runs | report held-out effect and uncertainty, never rewrite immutable m0 |
| scale benches | before release | report |

Every D48–D51 row above says **once implemented** literally: these are the
validation scenarios the eventual code must satisfy, written down now so the
machine values cannot drift. No fixture in those four rows exists in the
repository today, and none of them is evidence that the behavior works.

## 7. Retrieval utility, calibration and replay (D47)

A numerically correct PPR operator can retrieve the wrong material. Evaluate
**delivered primary bundles**, not pre-budget ranks, against held-out tasks
with independently judged relevance, source-faithfulness, conflict coverage
and downstream task success. Report exact budget/unit/tokenizer, primary and
companion counts, missing/conflict-redacted bundles, policy-denied rate and
source-provenance completeness. Utility U and adoption are observational
proxies, not truth or causal labels; a whole-recall reward is one dependent
observation even when its conserved weight is distributed to many Episodes.

Required ablations, keeping the query split, policy, budget/renderer and
candidate capture fixed:

| Comparison | Question |
|---|---|
| BM25 originals; candidate-only RRF; RRF + local PPR | Does graph spreading add useful context over simpler retrieval? |
| capped physical-degree seed damping vs no damping; exact eligible-degree damping offline only | Measure approximation cost, including hidden-generation saturation, without adding an online full-degree scan |
| beta=0 vs beta=.25 | Does receipt utility improve held-out task results rather than amplify exposure bias? |
| gamma=0 vs gamma=.5 | Does final mass weighting help? Separately remove mass ordering in envelope truncation; gamma=0 alone does not remove that gate |
| sigma_fact=1 vs 30; modality/sub_kind priors flat vs defaults | Are slower derived decay and content priors useful assumptions? |
| Episode-only sibling coupling and source-mean U | Quantify collateral refresh and shared-source utility correlation, not per-Fact selectivity |
| bounded conflict/occurrence assembly vs no grouping | Measure warning/coverage cost under the same exact output budget without discarding occurrence provenance |

Do not tune on the evaluation split. Split chronologically and by source/
session groups so shared-source siblings and repeat occurrences cannot leak
across training and evaluation. Report grouped uncertainty, support counts,
missing/expired receipt rate and channel/degradation cohorts. Exposure-only
rows and missing reward are not negative labels; reward zero is observed.
Without randomized exposure or documented propensity capture, report
associations, not causal benefit. No automatic prior refit from a few outcome
verdicts: retain raw judge outputs, prior/config versions and selection state;
fit candidate parameters offline, evaluate held-out ablations, and activate
only through authenticated operator configuration/generation commands.

Intrinsic m0 calibration changes require a **new derived generation**.
Accessibility/utility replay uses versioned dynamics and retained Hit events;
it does not overwrite immutable m0. Accessibility initialization remains
Episode S0(original m0) at ingestion, and only adoption changes S/t_last_hit.
Outcome-only leaves those values unchanged. Adoption+negative must be compared
with adoption-only, not no event. Include zero/missing outcomes, already-negative
U receiving a less-negative reward, S_max saturation, shared sources and empty
adoption sets in numerical checks.

Replay reports distinguish three levels:

1. **Numeric replay**: captured CSR, seeds, config and runtime reproduce the
   local solver; record iterate delta, actual fixed-point residual if computed,
   mass conservation and error bounds.
2. **Receipt reconstruction**: retained immutable source IDs and bounded
   derived snapshots reconstruct delivered primaries/companions, actual ranks,
   renderer, exact used_budget and result/context digests. Receipt expiry
   closes feedback, not the retained Hit outcome replay needed for U.
3. **Full retrieval reproduction**: additionally capture candidate/index/
   coverage state, stage/source ConductingArc rows/counts/saturation and
   publication state, stale-row exclusions, Entity
   earliest_allowed_from thresholds pinned to generation/policy, cache snapshots,
   policy/generation/config versions and channel/timeouts/degradation decisions.
   structure_revision alone cannot reproduce ANN, hidden-generation adjacency
   backfill, maintenance or concurrent feedback. Label unavailable capture
   explicitly instead of calling a best-effort rerun deterministic.

Ordinary reconstruction is still subject to **current** policy, including
historical T. Privileged offline audit of suppressed data is a separate
operator surface, never an ordinary-serving bypass.

### What the frozen screens do and do not establish

Two development-scoped comparisons back the current defaults, and neither is a
deployment result.

- [research/adjudication-conformance](research/adjudication-conformance.md)
  measures strict-format conformance on 36 synthetic cases against a frozen
  prompt. It selects a development adjudicator for that one role and prompt;
  it certifies no extractor, transfers to no other role and supplies no
  production error bound. Two disclosed gold choices are arguable and the set
  has no production-shaped negatives, so unattended invalidation stays blocked
  behind independent labels with declared false-invalidation,
  missed-update, candidate-completeness, transport and parse bounds (D50).
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

Degree boundaries use physical conducting-link counts 0, 255, 256, 257 and
a large hub, interleaving all five roles and parallel link IDs so that five
separate per-role caps or peer deduplication cannot pass. Assert one row per
distinct endpoint/link pair, unique (source_id, link_id), no INVALIDATES or
CONTRASTS rows, copied generation/source extraction metadata and exact
equality with the independently enumerated retained physical graph. Include
original NEXT_EPISODE, active/hidden/retired extraction links and cross-stream
HAS_MEMBER. The current protocol rejects a self-link before any physical/cache
write; assert that rejection. A separate defensive legacy-corruption rebuild
fixture emits only one row for a self-loop's single endpoint and reports the
violation, not a newly permitted ordinary self-link.

Require exactly min(degree,256) globally link-ID-ordered ConductingArc rows,
256 saturation, bounded collection even when empty, and no role/time/
generation/policy predicate before the raw cap. At 257, put hidden/denied
links among the first 256 and an eligible link at 257: it must never refill
the probe. Non-hubs resolve only <256 captured rows via the correct role's
unique link-ID index before sorting; hubs read at most 32 HubArcs and verify
their physical links by the same bounded path. Missing shortlists produce
dangling rows, whereas missing/incomplete ConductingArc coverage or indexes
drops the entire PPR channel as degree_probe_unavailable, not degree zero.
Assert no native adjacency query executes in any missing-cache case.
The online completeness check reads only Meta.conducting_arc_ready; verify
its agreement with partition publication during rebuild/GC without a
recall-time generation-registry scan. Missing/false gates are unavailable.

Create/delete, session backfill rewire, retired-generation GC and rollback
fixtures compare physical links and endpoint rows in the same committed
transaction; aborted writes expose neither half. During a paged rebuild no
incomplete partition is published COMPLETE. Include a hidden-generation
rebuild: physical degree depends on it even when its semantic output is
invisible. Subscribe to actual transaction/publication barrier events before
triggering the write, then await the signal with a bounded timeout. Cache
loss disables PPR until retained-graph reconstruction and verification
publish complete coverage; channel-only recall remains available under its
ordinary policy/receipt requirements.

Inject captured stale rows for deleted links, wrong peer/source, wrong role
and mismatched generation metadata (including HAS_MEMBER's extraction pin).
Prove that each lookup is the recorded role's indexed ID seek, that endpoint/
role/generation validation excludes mismatches without refill, and that stale
rows still occupy the raw probe slots. Repeat for HubArc tuples. Detect
missing coverage rows during verify, invalidate coverage and require rebuild;
never infer complete degree from a partially rebuilt source. No cached tuple
is authority, a semantic candidate, conductor or output in these fixtures.

Verify capped seed damping, fixed-capture replay despite hidden backfill,
ordered-plan rejection and whole-PPR degradation on seed-probe failure.
Prove the conservative counters with an exact small fixture as well as scale
plans: each source-stage costs at most 512 structural inputs (non-hub
255+255; hub 256+32+32), (128+640+2000)*512 = 1,417,216, and 274*256 seed
rows bring the per-attempt ceiling to 1,487,360. Track policy/source work
separately: per Fact at most 16 direct Episode sources plus 16 support Facts
with 16 Episode sources each (272 Episode and 16 support-Fact ID lookups),
at most 256 active policies and 512 scalars per literal. Never assert input
work bounds from final result sizes or wall-clock timing alone.

Entity witness fixtures include a denied old mention plus an allowed future
mention: past T excludes the Entity even when its structural visible_from_utc
is old. At earliest_allowed_from equality it can become eligible; matching
generation/policy, link/source permission and structural visibility still
apply. Missing/stale/null cache proof excludes at candidates, conduction and
assembly; rebuild before serving, never scan MENTIONS or create a cache per T.
Use explicit publication/barrier events to exercise policy changes and rebuild
completion; replay captures the threshold and versions actually used.

Boundary validation uses parsed values and resident arithmetic, not prose
snapshots: malformed budget numbers/units/tokenizers, UTF-8 vs scalar counts,
zero/exact-fit/oversized skipped bundles, shared-companion deduplication and
post-budget ranks, 64-primary/256-companion caps, full-response 1 MiB cap, and
receipt TTL equality/restart/duplicate outcomes. For conflicts exercise raw
adjacency sizes 0, 4, 5, 64 and 65 with denied/invalid peers; at most 64 peers
are inspected, at most four included, totals are null when truncated, and
unknown remainder always carries the mandatory incomplete warning. Policy
races subscribe to the actual barrier state before triggering the command,
then await completion with a bounded timeout, never sleeps or polling luck.
