# 04 — Forgetting

Forgetting is computed, not stored. An element's current mass is

```text
  m(now) = m₀ · R(now − t_last_hit, S)
  R(t, S) = (1 + FACTOR · t / S)^DECAY          DECAY = −0.5,  FACTOR = 19/81  ⇒  R(S, S) = 0.9
```

with inputs the immutable `m₀`, the cached `(S, t_last_hit)`, and the server
clock `now`. `(S, t_last_hit)` is a cache that is deterministically
regenerable from the Hit ledger. There is no tick daemon.

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
  sources(f)   = the set of Episodes reached by following DERIVED_FROM to depth ≤ 3
                 (ignoring generation and snapshot — the provenance exception, docs/03 §3)

  R_fact(f)      = max_{e ∈ sources(f)} R(now − t_last_hit(e), s(e) · σ_fact)          σ_fact = 30

  m(Fact f)      = m₀(f) · R_fact(f)
  m(Entity n)    = m₀(n)                                                                  (no decay)
  m(Community c) = m₀(c) · max_{f ∈ members(c) ∩ Fact} R_fact(f)                          (v0.3)
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

| kind | κ | Origin |
|---|---|---|
| `recall_hit` | 1.0 | A receipt client reports that a result was adopted |
| `re_mention` | 0.5 | The extraction judge classifies a new utterance as a duplicate of an existing Fact |
| `promotion` | 0.3 | Dreaming synthesis uses this Fact as a source of a higher-level fact |
| `exposure` | 0.15 | A result was shown to an auto client (top-3). Adoption unknown |

### Derived → Episode attribution, with conservation

If the adopted element is a Fact, a Hit is created on each Episode in
`sources(f)`. Total reinforcement is conserved:

```text
  κ_eff(e) = κ(kind) / |sources(f)|
```

If, within one recall, the same Episode is a source of several adopted items,
the Hits **merge into one** with `kappa_eff = min(Σ κ_eff, κ(kind))`. Adopting
a fact with four sources gives each source 0.25; if a source also backs another
adopted item the shares add up but never exceed the kind's κ.

### Reinforcement — updating S

At hit time `t_h`:

```text
  R_hit = R(t_h − t_last_hit, s)
  s′    = min( S_max,  s · (1 + a · κ_eff · (e^{b(1−R_hit)} − 1) · s^{−c}) )
  t_last_hit′ = t_h,   hit_count′ = hit_count + 1

  a = 5.0,  b = 1.0,  c = 0.1,  S_max = 3650 days        (s in days)
```

This is the FSRS stability-increase formula with the difficulty term replaced
by κ_eff. Properties:

- s′ ≥ s. Reinforcement never lowers stability.
- **Spaced beats massed.** The gain of a second hit is `(e^{b(1−R_hit)} − 1)`,
  which grows as R_hit falls (as the gap grows). Recalling something again a
  month later makes it last longer than recalling it twice in a row.
- `s^{−c}`: already-stable memories gain a little less.

```text
  s = 1 d, κ_eff = 1
    R_hit 0.9 (1 day later)    → s′ = 1 + 5·0.105 = 1.53
    R_hit 0.5 (10 days later)  → s′ = 1 + 5·0.649 = 4.24
    R_hit 0.2 (100 days later) → s′ = 1 + 5·1.226 = 7.13
```

### The Hit node

```text
  (:Hit {id, t: t_h (server ms), kind, kappa_eff, recall_id, idem_key}) -[:HIT_OF]-> (:Episode)
  idem_key = sha256(recall_id, episode_id, kind)
```

`recall_id` namespaces: recall_hit and exposure use the recall's UUID,
re_mention uses `extract:<episode_id>`, promotion uses
`dream:<synthesis_fact_id>`. The same cause produces at most one Hit per
Episode and kind.

## 6. Commit protocol

### Modes

A client declares its mode in `hello {commit_mode}`.

| Mode | Who | Hits |
|---|---|---|
| `receipt` | Clients that can observe adoption (an agent reports which results it used) | client `commit` → `recall_hit` |
| `auto` | Clients that cannot observe adoption (context-injection hooks) | server records `exposure` on the top-3 sources right after recall |

An explicit `commit` from an auto client is rejected with
`commit_mode_mismatch`. A client that cannot see adoption reporting adoption
would pollute the ledger. The small κ of exposure is the price of "it was
shown, but we do not know whether it was used".

### Server procedure (receipt)

```text
  commit {recall_id, adopted: [element_id…], kind = recall_hit}
    1. recall_id is in the recall log (in-memory ring, TTL 1 h) and adopted ⊆ that recall's results
       otherwise reject (`unknown_recall` — after a daemon restart the client simply recalls again)
    2. each adopted → sources(x)  (an Episode is its own source)
    3. per-Episode κ_eff summation and merging (§5)
    4. per Episode
         a. idem_key exists → skip
         b. cache check: hit_count == COUNT { (:Hit)-[:HIT_OF]->(e) }
            mismatch → regenerate (s, t_last_hit, hit_count) by full ledger replay, then continue
         c. compute R_hit, s′ (the server does this; the client sends no numbers)
    5. one transaction: Hit CREATE × n + Episode cache SET × n
    6. return {hits_created, episodes: [{id, s, s′}]}
```

recall attaches `sources` to every result, so step 2 comes from the log — if a
generation switched in between, the attribution from recall time is used.

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
  discarded and the Episode's ledger is **replayed in full**. There are no
  checkpoints — one Episode has at most a few hundred Hits.
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
- Envelope fanout ordering uses `m_cache` (SET daily by dreaming, docs/02 §6)
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
| κ | 1.0 / 0.5 / 0.3 / 0.15 | assumption |
| γ, ε | 0.5, 0.02 | assumption |

Refitting: receipt-mode recall logs yield the label "which of the shown
results were adopted". Using the predicted R at exposure time as the feature
and adoption as the target, minimize log loss to fit DECAY, FACTOR, a, b, c.
Adoption is a proxy for recall probability (an irrelevant result is not
adopted even if remembered), so the sample is restricted to the top relevance
band. Constants are recorded in `config.jsonc` and every change gets a version
tag — when constants change, the cache is regenerated by full replay.

## 10. CI fixtures

- `R(S, S) = 0.9` exactly (floating-point tolerance 1e-12)
- R monotonically decreasing, S monotonically increasing
- spaced > massed: the same two hits at 1 d vs 30 d apart → s′(30 d) > s′(1 d)
- κ conservation: adopting a Fact with n sources → Σ κ_eff = κ
- merge cap: an Episode overlapping within one recall has kappa_eff ≤ κ
- replay(e) == cache, property test over 1,000 random Hit sequences
- inserting Hits out of order then replaying == inserting them in order
- an element with `m = 0` stays in the results when relevance dominates (ε behavior)
