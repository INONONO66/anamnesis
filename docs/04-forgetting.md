# 04 — Forgetting

This is the normative anamnesis2 design, not a claim that the current
originals/fulltext implementation already implements these dynamics.
[D45 and D47](10-decision-log.md) separate **accessibility** (retention after
confirmed use) from **utility** (reported downstream usefulness). Neither is
truth, validity, policy permission, or calibrated human recall probability.

Forgetting is computed, not stored. For an Episode:

```text
  Δdays(t₁,t₀) = max(0, t₁ − t₀) / 86,400,000
  A_e(now) = R(Δdays(now, t_last_hit(e)), s(e))
  m(e, now) = m₀(e) · A_e(now)
  R(t, S) = (1 + FACTOR · t / S)^DECAY
  DECAY = −0.5, FACTOR = 19/81, t ≥ 0 days, S > 0 days
  R(S, S) = 0.9
```

`S` in equations and cache field `s` denote the same stability, measured in
days. Inputs are immutable `m0`, cached `(s, t_last_hit)`, and the server clock
`now`. The cache is deterministically regenerable from the Episode and Hit
ledger under a pinned dynamics configuration (§7). There is no tick daemon.
Clock regression yields elapsed zero; `0 < R ≤ 1` at finite elapsed time.

## 1. m₀ — intrinsic mass

Assigned once at creation, immutable, in `[0,1]`. These are assumptions for
calibration, not measured estimates of importance or correctness.

| Kind | m₀ default |
|---|---|
| Episode `original-message` | 0.5 |
| Episode `original-document` | 0.6 |
| Episode `correction` | 0.8 |
| Fact, including synthesis | `confidence × prior(sub_kind) × prior(modality)` |
| Entity | 0.5 |
| Community | 0.5 |

| `sub_kind` | Prior |
|---|---|
| preference, decision | 1.0 |
| fact, procedure | 0.9 |
| state | 0.8 |
| event | 0.7 |
| summary | 0.6 |

| `modality` | Prior |
|---|---|
| asserted | 1.0 |
| reported | 0.8 |
| hedged | 0.6 |
| intended | 0.5 |
| hypothetical | 0.3 |

Every Fact, **including synthesis**, requires explicit `modality` judged from
its own content. `confidence` is source-faithfulness given that modality; for
synthesis it is faithfulness to the complete bounded support bundle, not the
probability that the world agrees with it. Never substitute a default modality
or multiply uncalibrated support confidences as if sources were independent.
A `summary`, `reported`, `confidence=0.9` synthesis has `m0=0.432`.

Retain raw extraction/judge/synthesis outputs and their model, prompt,
prior/calibration and configuration versions with the generation audit.
Changing priors requires a new derived generation; Hit replay must never SET
immutable `m0` or recompute an original Episode's birth mass from new priors.
Fact identity includes modality and generation, excludes confidence; the
first immutable confidence on an exact retry stands (docs/01 §1, D42).
A literal `evidence_quote` and its derived nonempty UTF-8-boundary-valid
`span` validate the locus only, not entailment or absence of hallucination
(docs/02 §5).

Nothing else raises `m0`. Occurrence count, echo depth, corroboration-root
count and a Fact's
`content_language` are not inputs to the prior; a repeated claim
has exactly the birth mass its schema, sub-kind, modality and confidence give
it (D48, D49). A replacement `A′` copies A's bounded Episode authority,
confidence, `m0` and prior/calibration versions and its roots, and gets no
fresh accessibility (docs/03 §5).

## 2. State lives on Episodes only

The accessibility state `(s, t_last_hit, hit_count)` exists **only in the
Episode's hit cache**. Facts have no independent reinforcement state. Utility
has separate rebuildable per-Episode sufficient statistics (§5.1).

```text
  initialization (inside the remember transaction)
    s          = S0(m₀(e)) = S_base · (1 + λ · m₀(e))
    t_last_hit = ingested_at
    hit_count  = 0
    S_base = 1 day, λ = 1  ⇒  s ∈ [1, 2] days
```

An original message starts at `1.5` days, a document at `1.6`, a correction at
`1.8`. Initialization uses **that original Episode's m0 at ingestion**, not a
later Fact's confidence or modality. A document from 2019 ingested today starts
being forgotten today; event-time interpretation is handled by snapshot and
temporal validity, not by rewinding this initialization.

`hit_count` counts outcome Hits. Each credited Episode receives one Hit per reported Usage, including for neutral reward.
Control/policy Episodes are not memory candidates or feedback targets.

## 3. Mass of derived elements

```text
  sources(f) = f.source_episode_ids
               1–16 original Episodes, materialized at Fact creation
               (generation/snapshot-exempt provenance, docs/03 §3;
                never exempt from current policy)

  A_f(now) = R_fact(f)
           = max_{e ∈ sources(f)} R(Δdays(now,t_last_hit(e)), s(e) · σ_fact)
  σ_fact   = 30

  m(Fact f)      = m₀(f) · A_f(now)
  m(Entity n)    = m₀(n)                                     (A_n = 1)
  m(Community c) = m₀(c) · max_{f ∈ members(c) ∩ Fact} A_f(now) (v0.3)
                  = m₀(c) if there are no Fact members
```

These equations do not authorize serving an element. Denied sources cannot
support visible derived results; exclude such results under D43 rather than
silently deleting sources, changing denominators, or treating an all-denied
Community as an empty, fully accessible one (§8).

- **max**, not sum or mean, selects the **highest retention**, not necessarily
  the most recently reinforced source. With `σ_fact=30`, a source last hit
  30 days ago with `s=100 days` gives `R=0.998829`; a source last hit yesterday
  with `s=1 day` gives `R=0.996113`. The older but more stable source wins.
- `σ_fact` stretches the time axis: at equal history the modeled gist decays
  30 times more slowly than original text. **30 is an assumption**, not an
  empirical result or per-Fact measured stability.
- Episode-only attribution deliberately accepts **collateral sibling
  refresh**. Adopting one Fact can refresh unrelated Facts sharing that
  Episode, including a Fact that was not delivered. This is not per-Fact
  selectivity. Utility is similarly coupled through source Episodes (§5.1).
- Entities do not decay. Their surrounding Facts can fade, but that does not
  prove the anchor itself will leave bounded candidate lists.
- Authority is a stored bounded set, not a recursive graph traversal.
  `source_count_total` and `sources_truncated` expose truncation (docs/01 §1).
- Generation switches, replacement and re-extraction do not reset Episode
  state. A replacement reserves its correction Episode plus at most 15 old
  sources; retained sources carry their history, omitted sources do not.
  Therefore neither an identical authority set nor identical replacement
  accessibility is guaranteed. Non-recursive INVALIDATES remains unchanged.
  An operator repair takes the same path (docs/03 §5): A′ inherits the
  bounded retained authority, not a fresh accessibility grant.
- Echo lineage is inert here. `echo_state`, `echo_depth`, occurrence count and
  `corroboration_root_episode_ids` never enter `m0`, `S`, `κ_eff`, mass or
  utility, so an assistant restating retrieved text cannot make it more
  accessible (D49). A Hit still attaches to the Episode that actually
  produced the delivered result, and an assistant Episode with unknown or
  truncated lineage contributes no semantic output to be adopted at all.

Illustrative retention (`S=1 day`, `S_eff=30 days`; `1 year=365 days`):

| Elapsed | Episode R | Elapsed | Fact R |
|---|---|---|---|
| 1 day | 0.900 | 1 day | 0.996 |
| 10 days | 0.547 | 30 days | 0.900 |
| 100 days | 0.202 | 1 year | 0.509 |
| 1 year | 0.107 | 3 years | 0.323 |

`S=1` is the boundary illustration (`m0=0`), not the default message's `S0`.

## 4. The now axis — independent of snapshot(T)

Mass and utility are evaluated from state read at **now**, even for a
historical `snapshot(T)`. Snapshot asks what was valid at T; accessibility
asks how retained it is now. Historical T never bypasses **current policy**.

An audit replay through a past ledger cutoff is a separate calculation under
pinned configuration, not ordinary historical recall. `structure_revision`
alone cannot recreate the candidate/index/cache/degradation state of a past
response; Usage preserves ordered attribution and lineage, not the response itself.

## 5. Episode credit and the testing effect (D62)

Only explicit outcomes create Hits. At rank j of n primaries, the share is
(1/(j+1))/H_n. Split each share equally among its distinct direct source
Episodes and sum shared contributions. Each Episode gets one outcome Hit of
credit 0.2 r w; a report contributes at most 0.2 absolute utility. Empty
selections record the verdict without Hits.

## 6. Outcome admission

Each successful recall, including an empty result, creates a Usage and returns
its server-issued usage_id. Usage stores the ordered {id,sources} selection,
created_at, policy_revision, lineage rows and their selection_digest, and the
connection binding. A Usage has no TTL or copy of the response or query.

report_outcome takes {usage_id,idempotency_key,reward}. Both IDs are UUIDv7;
clamp finite reward to [-1,1] and normalize negative zero. Authorize all source
Episodes under current policy before accepting or replaying a verdict. Create
one UsageOutcome and its Episode Hits atomically. Return {reward,applied}; an
identical normalized retry returns applied:false. A reused key with different
usage or reward raises idempotency_conflict; another key on the same usage
raises outcome_already_reported. Missing usage, denied source and unavailable
storage raise unknown_usage, policy_denied and storage_unavailable respectively.
Socket completion never writes to the graph.

## 7. Replay

Start with S0=1+m0, last hit at ingestion and U=0. Fold Hits by (t,id).
At each event decay U by 2 ** (-elapsed_ms / 2592000000), add credit and
clamp to [-1,1]. A nonzero signed coefficient r w drives the existing
adoption update, bounded by S0 and 3650 days. Count every Hit. Cache S,
last hit, count and utility at its anchor time. Decay from that anchor to
read time with the same half-life; never write just because time passed.
Verify compares the full replay to cache; rebuild replaces derived state.

## 8. Where accessibility, utility and policy enter recall

```text
  score(x) = relevance(x) · max(m(x), ε)^γ · (1 + β · U_x)
  m(x) = m₀(x) · A_x
  γ = 0.5, ε = 0.02, β = 0.25, 0 ≤ β < 1
```

- Relevance retains the RRF channels/weights in docs/05. Channel RRF ranks
  start at **1**, while stored selection offsets start at **0**. With normalized
  RRF weights and denominator `60+rank`, relevance cannot exceed `1/61`.
- `γ<1` compresses mass differences; `ε` preserves a positive mass factor for
  otherwise eligible candidates even at `m=0`. With `β<1` and `|U|≤1`, the
  utility multiplier stays positive; at defaults its range is `[0.75,1.25]`.
  Example: `relevance=0.01`, `m=0.62`, `U=-5/29` gives `score≈0.007534611`.
- Mass is a weight at final ranking **and a bounded-envelope selection gate**:
  fanout uses `coalesce(m_cache,m0)` (hourly maintenance, docs/02 §6), so a low
  mass neighbor can be omitted before exact scoring. The score floor does not
  guarantee admission or retrieval. Final mass/utility use the batch-captured
  exact source state; captured shortlist/cache state matters for replay.
- Policy and `valid(T)` are hard filters, never lower confidence scores. A
  denied source cannot support a visible Fact, synthesis, Community, cached
  profile or provenance snippet. D43 checks extraction, remention, dreaming,
  candidates, conduction, assembly and mandatory companion text. No outcome,
  adoption, mass floor or historical snapshot can override suppression.
- Duplicate grouping is assembly-only with representative and explicit
  occurrence IDs/truncation, preserving subject/predicate/time/modality.
  Grouping never rewrites authority or rewards undelivered occurrences. Its
  predicates are the bounded stored fields in docs/02 §5.3, and the resulting
  key is local to one candidate set, never a global identity claim (D49).
- Lineage is not a score term. `echo_state`, `echo_depth`,
  `corroboration_root_count` appear on results as
  provenance and never enter `relevance`, `m`, `U`, `score` or ordering;
  Facts from an Episode with unknown or truncated lineage are excluded from
  serving entirely rather than ranked lower (D49).
- After primary ranking, bounded conflict completion (D46) reads at most 65
  raw CONTRASTS adjacency rows in deterministic peer-ID order: inspect 64,
  reserve one sentinel, and select up to four eligible peers, even outside
  retrieval candidates. `conflict_included_count` is exact. If more than four
  inspected peers are eligible or the sentinel exists, report
  `conflict_truncated=true` and `conflict_total=null`; only an exhausted,
  untruncated scan reports an exact eligible total. An inspected policy-hidden
  peer yields `conflict_redacted=true` without text, ID or hidden count.
  No automatic winner by confidence or recency. The complete bundle, including required
  warnings, must fit D44's budget or that primary is skipped. There is no
  unconditional both-sides guarantee beyond the stated bound.

**Suppression is not erasure (D43).** `policy.set` and `policy.revoke` are
explicit authenticated commands, never instructions automatically executed
from ingested text. They append `anamnesis.memory-policy/1` control Episodes;
active policy is a rebuildable cache. Events carry `policy_id`, `action`
(`deny`/`revoke`), `selector` and `scope` (`derived`/`content`); at least one
selector field is required from `subject_entity_id`, `schema`, `sub_kind`,
`modality`, `literal`. Fields combine with AND. Literal matching is
NFC-normalized Unicode case-sensitive substring matching, not regex.

Derived scope checks canonical claims and resolved entities before derived
writes/Hits; content scope also checks raw Episode text and suppresses ordinary
original recall. Structured selectors cannot guarantee suppression of
unextracted original text; expose that limit. Policy becomes effective at the
revision barrier, with background derived index/cache rebuilding to remove
denied items, **not deletion of originals**. Control Episodes are excluded
from memory search while audit metadata remains. Revoke does not fabricate
previously suppressed Facts; re-extraction is explicit. Existing backups and
privileged raw operator access are outside this boundary. There is no
`gc --erase` or GDPR erasure guarantee.

## 9. Constants and calibration

| Constant | Default | Basis |
|---|---|---|
| DECAY, FACTOR | −0.5, 19/81 | FSRS-inspired power law; `R(S,S)=0.9` |
| S_base, λ | 1 day, 1 | assumption; initialize from immutable original m0 |
| σ_fact | 30 | assumption; ablate against 1 and fitted alternatives |
| prior(sub_kind), prior(modality) | §1 | assumptions, new generation for changed priors |
| a, b, c | 5.0, 1.0, 0.1 | FSRS-inspired adoption gain, not fitted coefficients |
| S_max | 3650 days | cap on signed stability updates; S0 is the floor |
| κ | r × w | signed reward times source weight |
| utility half-life | 30 days | decay the last anchor at read time |
| reward | [−1,1] | clamp a finite client report |
| γ, ε, β | 0.5, 0.02, 0.25 | assumptions; `0≤β<1` |

Reject malformed/non-finite configuration at the boundary: require
`DECAY<0`, `FACTOR>0`, `S_base>0`, `λ≥0`, `S_max≥S_base·(1+λ)`, `σ_fact>0`,
`a,b>0`, `c≥0`, `0<γ<1`, `0<ε≤1`, `0≤β<1` and valid `[0,1]` priors. If refitting DECAY while
preserving the stability definition `R(S,S)=0.9`, derive
`FACTOR=0.9^(1/DECAY)−1`; fitting both independently would redefine S.

Calibration uses explicit outcomes and the primary selections to which they apply.
Separate the tasks:

1. Fit source-faithfulness confidence against annotated source/support pairs,
   including modality, corrections, unsupported syntheses and correlated
   sources. Do not fit truth from bounded span validation.
2. Evaluate accessibility as an adoption proxy conditional on exposure and
   relevance, with held-out time/session splits. Restricting to a top relevance
   band can reduce relevance confounding, not establish unbiased memory
   probability. An unreported Usage is missing data, not failure.
3. Evaluate utility against explicit recall-level outcomes. Use one observation
   per recall with the conserved attribution weights, not many independent
   labels for its Facts or source Episodes. Zero is observed neutral reward.
4. Ablate utility (`β=0`), retention (`σ_fact`, adoption gain), priors, source
   aggregation and the mass gate against held-out retrieval/usefulness metrics.
   Include collateral sibling refresh and synthesis support correlation.

All constants and their versions live in `config.jsonc`/generation audit.
Changing dynamics or utility priors rebuilds caches under a new pinned version;
changing intrinsic priors rebuilds derived generations instead (§1). No
refit result or measured efficacy is asserted here.

D47 separates numerical PPR residual from bounded-envelope truncation and
retrieval usefulness: a small residual proves neither global recall coverage
nor a fixed top-k overlap in close ties. Retrieval validation keeps local
`α=0.85`, uniform dangling handling and normalized virtual-source V, with
`alpha=dampingFactor`, not its complement. The GDS oracle baseline is pinned
**2.13.12**, not master/latest (docs/06–07). Determinism is conditional on
captured candidate/index/cache/degradation state, not merely structure revision.

## 10. Required mathematical and protocol fixtures

These specify implementation checks; this prose-only finalization adds no
prose-pinning tests and does not claim the implementation already passes them.

- `R(S,S)=0.9` within `1e-12`; `R(0,S)=1`; elapsed time is nonnegative under
  clock regression. R is strictly decreasing for increasing finite elapsed
  time, and increasing in S for positive elapsed time (within tolerance).
- Positive and negative credit move stability in opposite directions within `[S0,S_max]`.
- Reciprocal-rank shares sum to one over a nonempty final selection; shared Episode sources receive one merged Hit. Total absolute credit is at most 0.2.
- An idle utility anchor halves after 30 days. Clip after each event; equal-time events are ordered by ID.
- A finite reward is required. Empty selections retain a verdict without Hits; a neutral report on a nonempty selection creates zero-credit Hits.
- Replay with the same key and clamped reward adds no Hits. Conflicting key reuse and a second verdict key fail atomically, including after restart.
- Replaying a fixed-seed set of 1,000 mixed event sequences matches cache
  updates, including out-of-order insertions and identical times ordered by
  ID. Replay never modifies m0 and generation switches never reset Episode S.
- Max-source accessibility chooses retention, not recency; unchanged sources
  do not grant replacement equality after source truncation. Sibling refresh
  is expected, not a failing per-Fact-selectivity test.
- Eligible mass-zero candidates retain a positive score factor, but admission
  through the bounded mass-ordered envelope is not guaranteed. The multiplier `1+βU` stays positive.
- Current-policy barriers reject/retry races before serving or committing;
  denied support cannot leak via receipt feedback, provenance, conflict peers,
  cached profiles or historical T. Only explicit authenticated policy RPCs
  create policy controls; ingested instructions are data.
