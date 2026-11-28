# 04 — Forgetting

**Accessibility** (retention since the last credited use) and **utility**
(reported downstream usefulness) are separate quantities. Neither is truth,
validity, policy permission or a calibrated human recall probability.

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

Assigned once at creation, in `[0,1]`. No code path rewrites it.

| Kind | m₀ |
|---|---|
| Episode | the request's `mass`, default 0.5 |
| Fact | the judge's `confidence`: its belief that the claim is what the source says |

`sub_kind` and `modality` are stored on the Fact and returned on recall. They
don't scale `m₀`; the prior tables designed for that are a calibration item
in [09-roadmap](09-roadmap.md).

Nothing else raises `m₀`. Occurrence count, echo depth, corroboration-root
count and a Fact's `content_language` are not inputs; a repeated claim has
exactly the birth mass its confidence gives it (D48, D49). Fact identity
includes modality and generation and excludes confidence, so the first
confidence on an exact retry stands (docs/01 §1, D42).

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

An Episode with the default mass starts at `1.5` days. Initialization uses
**that Episode's m0 at ingestion**, not a later Fact's confidence. A document from 2019 ingested today starts
being forgotten today; event-time interpretation is handled by snapshot and
temporal validity, not by rewinding this initialization.

`hit_count` counts outcome Hits. Each credited Episode receives one Hit per
reported Usage, including for a neutral reward.

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
```

These equations don't authorize serving an element. A Fact with a denied
source is excluded under D43, not rescored without that source (§8).

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
- Authority is a stored bounded set, not a recursive graph traversal
  (docs/01 §1).
- Generation switches and re-extraction don't reset Episode state.
- Echo lineage is inert here. `echo_state`, `echo_depth`, occurrence count and
  `corroboration_root_episode_ids` never enter `m0`, `S`, mass or
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

Each successful recall creates a Usage and returns its `usage_id`
(docs/01 §3.1).

`report_outcome` takes `{usage_id, idempotency_key, reward}`. Both IDs are
UUIDv7. A finite reward is clamped to [-1,1] and negative zero is normalized.
All source Episodes are authorized under current policy before a verdict is
accepted or replayed. One UsageOutcome and its Episode Hits are created
atomically. The result is `{reward, applied}`; an identical normalized retry
returns `applied: false`.

| Condition | Error |
|---|---|
| a reused key with a different usage or reward | `idempotency_conflict` |
| another key on a Usage that already has a verdict | `outcome_already_reported` |
| no such Usage | `unknown_usage` |
| a credited source is now denied | `policy_denied` |
| storage unavailable | `storage_unavailable` |

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
- Mass enters only at final ranking. No stage orders or truncates candidates
  by mass before scoring.
- Policy and `valid(T)` are hard filters, never lower scores. No outcome,
  mass floor or historical snapshot overrides suppression (docs/01 §3.2).
- Lineage is not a score term. Facts from an Episode with unknown or truncated
  lineage are excluded from serving, not ranked lower (D49).

## 9. Constants and calibration

| Constant | Default | Basis |
|---|---|---|
| DECAY, FACTOR | −0.5, 19/81 | FSRS-inspired power law; `R(S,S)=0.9` |
| S_base, λ | 1 day, 1 | assumption; initialize from the Episode's m0 |
| σ_fact | 30 | assumption; ablate against 1 and fitted alternatives |
| a, b, c | 5.0, 1.0, 0.1 | FSRS-inspired adoption gain, not fitted coefficients |
| S_max | 3650 days | cap on signed stability updates; S0 is the floor |
| κ | r × w | signed reward times source weight |
| utility half-life | 30 days | decay the last anchor at read time |
| reward | [−1,1] | clamp a finite client report |
| γ, ε, β | 0.5, 0.02, 0.25 | assumptions; `0≤β<1` |

These are code constants in `packages/core/src/dynamics/` and
`packages/core/src/store/recall-store.ts`, not runtime configuration. The
solver and retention functions reject nonfinite or out-of-range inputs with a
`RangeError`. If `DECAY` is refitted while keeping `R(S,S)=0.9`, derive
`FACTOR=0.9^(1/DECAY)−1`; fitting both independently would redefine S.

No constant here is fitted. What the frozen development screens do and don't
establish is in [07-gds-validation](07-gds-validation.md) §7, and the
calibration work is listed in [09-roadmap](09-roadmap.md). An unreported
Usage is missing data, not a failure, and a zero reward is an observed neutral
outcome.

## 10. Mathematical and protocol fixtures

- `R(S,S)=0.9` within `1e-12`; `R(0,S)=1`; elapsed time is nonnegative under
  clock regression. R is strictly decreasing in elapsed time and increasing in
  S for positive elapsed time.
- Positive and negative credit move stability in opposite directions within
  `[S0, S_max]`.
- Reciprocal-rank shares sum to one over a nonempty final selection; shared
  Episode sources receive one merged Hit. Total absolute credit is at most 0.2.
- An idle utility anchor halves after 30 days. Clip after each event;
  equal-time events are ordered by ID.
- A finite reward is required. Empty selections retain a verdict without Hits;
  a neutral report on a nonempty selection creates zero-credit Hits.
- Replay with the same key and clamped reward adds no Hits. Conflicting key
  reuse and a second verdict key fail atomically, including after restart.
- Replaying mixed event sequences matches cache updates, including
  out-of-order insertions and identical times ordered by ID. Replay never
  modifies m0 and generation switches never reset Episode S.
- Max-source accessibility chooses retention, not recency. Sibling refresh is
  expected, not a failing per-Fact-selectivity test.
- Eligible mass-zero candidates keep a positive score factor, and the
  multiplier `1+βU` stays positive.
- A denied source can't receive credit through `report_outcome`.
