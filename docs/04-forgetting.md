# 04 — Forgetting

Forgetting is computed, not stored. An element's current mass is

```text
  Δdays(t₁,t₀) = max(0, t₁ − t₀) / 86,400,000
  m(now) = m₀ · R(Δdays(now, t_last_hit), S)
  R(t, S) = (1 + FACTOR · t / S)^DECAY          DECAY = −0.5,  FACTOR = 19/81  ⇒  R(S, S) = 0.9
```

with inputs the immutable `m₀`, the cached `(S, t_last_hit)`, and the server
clock `now`. `(S, t_last_hit)` is a cache that is deterministically
regenerable from the Hit ledger. There is no tick daemon.
Clock regression yields elapsed zero; `R` never exceeds 1 and its base cannot
become negative.

## 1. m₀ — intrinsic mass

Assigned once at creation, immutable. Calibration target.

| Kind | m₀ default |
|---|---|
| Episode `original-message` | 0.5 |
| Episode `original-document` | 0.6 |
| Episode `correction` | 0.8 |
| Fact | `confidence × prior(sub_kind)` — prior: preference, decision 1.0; fact, procedure 0.9; state 0.8; event 0.7; summary 0.6 |
| Entity | 0.5 |
| Community | 0.5 |

## 2. State lives on Episodes only

The forgetting state `(s, t_last_hit, hit_count)` exists **only in the
Episode's hit cache**. The mass of Facts and Communities is **derived** from
their source Episodes.

```text
  initialization (inside the remember transaction)
    s          = S0(m₀) = S_base · (1 + λ · m₀)       S_base = 1 day,  λ = 1   ⇒  s ∈ [1, 2] days
    t_last_hit = ingested_at                           (server time)
    hit_count  = 0
```

Note that t_last_hit starts at **ingestion time**, not event time. A document
from 2019 ingested today starts being forgotten today — forgetting is a
function of how much the mind has handled the memory; how old the event is is
handled by sub_kind and m₀.

## 3. Mass of derived elements

```text
  sources(f)   = f.source_episode_ids
                 1–16 original Episodes, materialized at Fact creation
                 (ignoring generation and snapshot — the provenance exception, docs/03 §3)

  R_fact(f)      = max_{e ∈ sources(f)} R(Δdays(now,t_last_hit(e)), s(e) · σ_fact)     σ_fact = 30

  m(Fact f)      = m₀(f) · R_fact(f)
  m(Entity n)    = m₀(n)                                                                  (no decay)
  m(Community c) = m₀(c) · max_{f ∈ members(c) ∩ Fact} R_fact(f)                          (v0.3)
                   = m₀(c) if the Community has no Fact members (Entities do not decay)
```

- **max**, not sum or mean. A fact with several sources is as alive as its
  most recently reinforced source. A sum inflates with the number of sources;
  a mean lets old sources dilute recent reinforcement.
- **σ_fact** stretches the time axis. With the same hit history a Fact is
  forgotten 30× more slowly than the raw text — the detail of the original
  fades while the gist remains. 30 is an initial assumption and a calibration
  target.
- Entities do not decay. An anchor fades sufficiently through its facts being
  forgotten.
- `sources(f)` is a stored bounded authority set, not a recursive graph
  traversal. `source_count_total` and `sources_truncated` expose synthesis
  truncation (docs/01 §1).
- Because reinforcement history lives on Episodes, **generation switches, the
  replacement protocol (docs/03 §5) and re-extraction never touch forgetting
  state.** When A′ replaces A, A's source Episodes are A′'s sources, so the
  reinforcement A accumulated carries over.

```text
  A feel for R (S_base = 1 d, no reinforcement)

    Episode  (S = 1 d)               Fact  (S_eff = 30 d)
    ┌───────┬───────┐               ┌───────┬───────┐
    │ 1 d   │ 0.900 │               │ 1 d   │ 0.996 │
    │ 10 d  │ 0.546 │               │ 30 d  │ 0.900 │
    │ 100 d │ 0.202 │               │ 1 y   │ 0.510 │
    │ 1 y   │ 0.107 │               │ 3 y   │ 0.323 │
    └───────┴───────┘               └───────┴───────┘
```

## 4. The now axis — independent of snapshot(T)

Mass is **always evaluated at now.** Even when snapshot(T) asks about the
past, forgetting is not rewound to T.

- snapshot is "what the world was like at T"; forgetting is "how alive this
  memory is now". Mixing them makes memories that were "just born at T" look
  unduly fresh in answers to questions about the past.
- Mass at T is well defined (replay the ledger up to T), but no question needs
  that value. If one ever does, it is a single `until` argument on replay.

## 5. Hit — ledger and reinforcement

### Kinds and κ

| kind | κ | Producer | `recall_id` namespace |
|---|---|---|---|
| `recall_hit` | 1.0 | `commit` RPC from a receipt client: a result was adopted | the recall's UUID |
| `outcome` | signed, `reward · rank_decay` (§5.1) | `commit` RPC from a receipt client: a verdict on a whole recall — the context it produced led to a good or bad result | the recall's UUID |
| `exposure` | 0.15 | the daemon, after an auto-mode recall response: top-3 results were shown, adoption unknown | the recall's UUID |
| `re_mention` | 0.5 | the extraction write tx: a new utterance is a duplicate of an existing Fact | `extract:<episode_id>` |
| `promotion` | 0.3 | dreaming synthesis: this Fact became a source of a higher-level fact | `dream:<synthesis_fact_id>` |

Five producers, **one path**: all of them call the same internal commit
function (§6). Nothing else in the daemon creates a Hit. `outcome` is the only
kind whose κ can be **negative**; every other kind is strictly positive and can
only grow stability.

### 5.1 The outcome signal — a signed verdict on a recall

Adoption (`recall_hit`) says *which* results a caller used; it cannot say the
used context led anywhere good. A receipt client that observes the downstream
result reports it once, against the whole recall, as a bounded reward:

```text
  reward ∈ [−1, 1]                         clamped by the server
  rank_decay(x) = 1 / (rank_x + 1)         rank_x = 0-based position of x in the recall's results
  κ_signal(x)   = reward · rank_decay(x)   the per-result signed strength before source attribution
```

`rank_decay` gives the top result the most credit and blame and fades down the
list, so a verdict on a recall does not move every surfaced item equally — the
result the caller most likely acted on carries the signal. One recall yields at
most one `outcome` per source Episode; the namespace (the recall UUID) makes it
idempotent under retry (§ The Hit node).

Why a signed verdict at all: every other producer can only raise stability, so
without this a memory that keeps surfacing and keeps producing bad answers is
reinforced by its own exposure. `outcome` is the FSRS **"again" grade** the
rest of the ledger lacks — the only way `S` is ever held back or lowered
(§ Reinforcement). It is also the negative label §9 refitting needs and cannot
otherwise obtain.

### Derived → Episode attribution, with conservation

If the adopted element is a Fact, a Hit is created on each of its 1–16
materialized source Episodes. Total reinforcement is conserved:

```text
  κ_eff(e) = κ(kind) / |sources(f)|
```

If, within one recall, the same Episode is a source of several adopted items,
the Hits **merge into one** with `kappa_eff = min(Σ κ_eff, κ(kind))`. Adopting
a fact with four sources gives each source 0.25; if a source also backs another
adopted item the shares add up but never exceed the kind's κ.

### Reinforcement — updating S

At hit time `t_h`, with `κ_eff` the per-Episode effective strength from
attribution (§ Derived → Episode). For every positive kind `κ_eff ≥ 0`; only
`outcome` can deliver `κ_eff < 0`.

```text
  R_hit = R(Δdays(t_h, t_last_hit), s)

  κ_eff ≥ 0   (reinforcement — every kind including a positive outcome)
    s′ = min( S_max,  s · (1 + a · κ_eff · (e^{b(1−R_hit)} − 1) · s^{−c}) )

  κ_eff < 0   (the "again" grade — outcome only)
    s′ = max( S0(m₀),  s · (1 + d · κ_eff · (1 − R_hit)) )

  t_last_hit′ = max(t_last_hit, t_h),   hit_count′ = hit_count + 1

  a = 5.0,  b = 1.0,  c = 0.1,  d = 1.0,  S_max = 3650 days,  s ≥ S0(m₀)  (s in days)
```

The positive branch is the FSRS stability-increase formula with the difficulty
term replaced by κ_eff. The negative branch is a bounded penalty. Properties:

- **Positive hits never lower stability** and **negative outcomes never raise
  it.** `s′ ≥ s` when `κ_eff ≥ 0`; `s′ ≤ s` when `κ_eff < 0`.
- **A penalty cannot erase a memory.** The negative branch floors at `S0(m₀)`,
  the birth stability — a bad outcome demotes a memory toward "just learned",
  never below it, and never tombstones it. Forgetting still needs elapsed time;
  the ledger only moves `S`.
- The penalty grows with the gap: `(1 − R_hit)` is small for a memory hit
  moments ago and approaches 1 for a stale one, so a bad outcome on something
  the system was confident about (recently reinforced, high `R_hit`) barely
  moves it, while a bad outcome on a shaky recall demotes it hard.
- **Spaced beats massed** (positive branch). The gain of a second hit is
  `(e^{b(1−R_hit)} − 1)`, which grows as R_hit falls. Recalling something again
  a month later makes it last longer than recalling it twice in a row.
- `s^{−c}`: already-stable memories gain a little less.

```text
  s = 1 d, κ_eff = +1
    R_hit 0.9 (1 day later)    → s′ = 1 + 5·0.105 = 1.53
    R_hit 0.5 (~12.8 days later) → s′ = 1 + 5·0.649 = 4.24
    R_hit 0.2 (100 days later) → s′ = 1 + 5·1.226 = 7.13

  s = 4.24 d, κ_eff = −0.5   (a bad outcome, this result ranked second → reward −1 · 1/2)
    R_hit 0.5                → s′ = max(S0, 4.24·(1 − 1·0.5·0.5)) = 4.24·0.75 = 3.18
```

### The Hit node

```text
  (:Hit {id, t: t_h (server ms), kind, kappa_eff, namespace, idem_key}) -[:HIT_OF]-> (:Episode)
  idem_key = sha256(namespace, episode_id, kind)
```

The same cause produces at most one Hit per Episode and kind.

## 6. The commit path — the only Hit producer

### Internal function

```text
  commitHits(tx?, namespace, kind, elements: [element_id…], κ_of?: element_id → signed κ, t_h = server_time)
    κ_of defaults to the fixed κ(kind) for every element; outcome passes κ_signal per result (§5.1)
    1. each element → sources(x)  (an Episode is its own source)
    2. per-Episode κ_eff = κ_of(x)/|sources(x)|, summed across elements, magnitude-capped at max|κ_of| for the kind  (§5)
       the sum keeps its sign; conservation and the cap are on |κ_eff|
    3. per Episode
         a. idem_key = sha256(namespace, episode_id, kind) exists → skip
         b. cache check: hit_count == COUNT { (:Hit)-[:HIT_OF]->(e) }
            mismatch → regenerate (s, t_last_hit, hit_count) by full ledger replay (§7), then continue
         c. compute R_hit, s′  (the server computes; no caller supplies numbers)
    4. Hit CREATE × n + Episode cache SET × n in the supplied write tx;
       if tx is absent, open one standalone transaction
    5. return {hits_created, episodes: [{id, s, s′}]}
```

Called from exactly four commit sites (five producers — the `commit` RPC drives
two kinds), each with its own namespace (§5). Receipt and exposure calls open a
standalone transaction; extraction and dreaming pass their ambient write
transaction so their Fact and Hit effects are atomic. The negative outcome
branch of the S update lives in `commitHits` too — it is the same function, only
`κ_eff` may now arrive signed. This function is the whole write surface for
forgetting; `structure_revision` is not touched by the Hit/cache portion.

Because `idem_key = sha256(namespace, episode_id, kind)` includes the kind, an
adoption and a verdict on the same recall are distinct Hits on the same
Episode — a `recall_hit` and an `outcome` coexist and both replay.

### Producer 1 — `commit` RPC (receipt clients)

The `commit` RPC carries an adoption list, a verdict, or both. A verdict on a
recall the caller never adopted from is still valid — the context was compiled
and acted on regardless of which items the client flagged.

```text
  commit {recall_id, adopted?: [element_id…], reward?: number in [−1,1]}
    · caller's hello.commit_mode must be receipt, otherwise reject `commit_mode_mismatch`
    · recall_id must be in the recall log (in-memory ring, TTL 1 h),
      otherwise reject `unknown_recall` (after a daemon restart the client simply recalls again)
    · at least one of adopted / reward must be present, otherwise reject `empty_commit`
    · adopted present → adopted ⊆ that recall's results, else reject `unknown_recall`;
      commitHits(recall_id, recall_hit, adopted)
    · reward present → clamp to [−1,1]; for each result x at rank_x, κ_signal(x) = reward·/(rank_x+1);
      commitHits(recall_id, outcome, all result ids, κ_signal per result)
```

The verdict spans **all** results of the recall (not just the adopted subset),
since rank decay already concentrates the signal on the top of the list. recall
attaches `sources` and the 0-based `rank` to every result and the log keeps
them, so both attribution and rank decay use the recall as of its own time even
if a generation switched in between.

### Producer 2 — exposure (auto clients)

After the response of a recall whose client declared `commit_mode = auto`,
the daemon calls `commitHits(recall_id, exposure, top-3 result ids)`. This
runs after the bytes are on the socket, is not part of recall latency, and its
failure does not affect the response (docs/05 §10).

### Producer 3 — re_mention (extraction)

In the extraction write transaction, for every claim judged a duplicate of
Fact F: `commitHits("extract:" + episode_id, re_mention, [F])` (docs/02 §5).

### Producer 4 — promotion (dreaming)

When a synthesis Fact S is created from member Facts:
`commitHits("dream:" + S.id, promotion, member fact ids)` (docs/02 §7).

### Modes

A client declares its mode in `hello {commit_mode}`.

| Mode | Who | Hits |
|---|---|---|
| `receipt` | Clients that can observe adoption (an agent reports which results it used) | client `commit` → `recall_hit` and/or `outcome` |
| `auto` | Clients that cannot observe adoption (context-injection harnesses) | daemon records `exposure` on the top-3 sources after the response |

Only a receipt client can report an `outcome`: a verdict presumes the client
saw what the context produced, the same premise as adoption. An auto client
gets `exposure` and nothing else.

An explicit `commit` from an auto client is rejected. A client that cannot see
adoption reporting adoption would pollute the ledger. The small κ of exposure
is the price of "it was shown, but we do not know whether it was used".

## 7. Replay — the cache is a function of the ledger

```text
  replay(e):
    (s, t, n) = (S0(m₀(e)), ingested_at(e), 0)
    for h in Hits(e) ORDER BY h.t ASC, h.id ASC:
      (s, t, n) = update(s, t, n, h.t, h.kappa_eff)
    return (s, t, n)
```

- The cache must always equal this function's result. `verify` samples it,
  and commit replays on the spot when it sees a `hit_count` mismatch.
- **Out-of-order Hits** (`h.t < t_last_hit` — migration imports, clock
  regression): the incremental update is order-dependent, so the cache is
  discarded and the Episode's ledger is **replayed in full**. Every replay
  step uses `Δdays=max(0,…)` and retains `max(t_last_hit,h.t)`, including a Hit
  earlier than `ingested_at`. There are no checkpoints — one Episode has at
  most a few hundred Hits.
- Dropping the whole cache and regenerating it is always possible
  (`anamnesis rebuild --hit-cache`).

## 8. Where mass enters recall

```text
  score(x) = relevance(x) · max(m(x), ε)^γ          γ = 0.5,  ε = 0.02
```

- Mass is **a weight, not a gate**. A forgotten memory still surfaces when
  relevance dominates.
- γ < 1 compresses mass differences so relevance leads. Calibration target.
- ε keeps the score from collapsing to 0 as m → 0, which would destroy the
  ordering.
- Envelope fanout ordering uses `coalesce(m_cache,m0)` (`m_cache` is SET
  hourly by maintenance; new nodes fall back to total immutable `m0`, docs/02 §6)
  so that exact m is not computed for every neighbor. The final score uses
  exact m(now).

## 9. Constants and calibration

| Constant | Default | Basis |
|---|---|---|
| DECAY | −0.5 | FSRS-4.5 power law |
| FACTOR | 19/81 | so that R(S, S) = 0.9 |
| S_base | 1 day | assumption |
| λ | 1 | S0 ∈ [S_base, 2·S_base] |
| σ_fact | 30 | assumption |
| a, b, c | 5.0, 1.0, 0.1 | near FSRS w8, w10, w9 |
| S_max | 3650 days | cap |
| d | 1.0 | penalty scale for the negative outcome branch; assumption |
| κ | 1.0 / 0.5 / 0.3 / 0.15 | positive kinds; assumption |
| reward | [−1, 1] | `outcome` κ before rank decay; caller-supplied, server-clamped |
| γ, ε | 0.5, 0.02 | assumption |

Refitting: receipt-mode recall logs yield two labels — "which of the shown
results were adopted" (from `recall_hit`) and "did the context lead to a good
result" (from `outcome`, the negative half of the sample that adoption alone
cannot supply). Using the predicted R at exposure time as the feature and
adoption as the target, minimize log loss to fit DECAY, FACTOR, a, b, c; the
outcome label fits the penalty scale d against downstream success. Adoption is
a proxy for recall probability (an irrelevant result is not adopted even if
remembered), so the sample is restricted to the top relevance band. Constants
are recorded in `config.jsonc` and every change gets a version tag — when
constants change, the cache is regenerated by full replay.

## 10. CI fixtures

- `R(S, S) = 0.9` exactly (floating-point tolerance 1e-12)
- R monotonically decreasing, S monotonically increasing
- spaced > massed: the same two hits at 1 d vs 30 d apart → s′(30 d) > s′(1 d)
- κ conservation: adopting a Fact with n sources → Σ κ_eff = κ
- merge cap: an Episode overlapping within one recall has kappa_eff ≤ κ
- signed monotonicity: `κ_eff ≥ 0 ⇒ s′ ≥ s` and `κ_eff < 0 ⇒ s′ ≤ s`
- penalty floor: a chain of `outcome` reward −1 hits drives `s → S0(m₀)` and
  never below; `m` still decays only through elapsed time
- rank decay: reward −1 on a recall moves the rank-0 source twice as much as
  the rank-1 source (`κ_signal ∝ 1/(rank+1)`)
- outcome idempotency: two `commit` verdicts on the same `recall_id` create one
  `outcome` Hit per source Episode, not two
- replay(e) == cache, property test over 1,000 random Hit sequences
- inserting Hits out of order then replaying == inserting them in order
- an element with `m = 0` stays in the results when relevance dominates (ε behavior)
