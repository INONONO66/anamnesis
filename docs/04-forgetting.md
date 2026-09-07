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
count, an operator's acceptance of an adjudication proposal and a Fact's
`content_language` are not inputs to the prior; a repeated or reviewed claim
has exactly the birth mass its schema, sub-kind, modality and confidence give
it (D48–D50). A replacement `A′` copies A's bounded Episode authority,
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

`hit_count` counts all retained Episode Hits for cache integrity, including
audit-only kinds. **Only `recall_hit` changes `s` or `t_last_hit`.** Recording
another kind may advance `hit_count` but is not an accessibility refresh.
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
response; bounded receipt snapshots supply feedback attribution and limited
replay evidence, not a full time machine (D47).

## 5. Hit — ledger, accessibility and utility

### Kinds and effects

| kind | Accessibility effect | Producer | Namespace |
|---|---|---|---|
| `recall_hit` | positive adoption coefficient κ = 1 before source sharing | authenticated receipt-mode `commit` | recall UUID |
| `outcome` | none; separate utility `(reward, weight)` | authenticated receipt-mode `commit` | recall UUID |
| `exposure` | none; audit of top-3 delivered primaries | daemon after auto-mode response | recall UUID |
| `re_mention` | none; occurrence audit | extraction transaction | `extract:<episode_id>` |
| `promotion` | none; synthesis support audit | dreaming transaction | `dream:<synthesis_fact_id>` |

Five producers, one internal Hit write path (§6). Exposure, remention,
promotion and positive, zero or negative outcome never move stability or
reset/rewind `t_last_hit`. They are not FSRS grades. The earlier D40 signed
stability-penalty design is superseded by D45; there is no negative-S branch.

### 5.1 Outcome utility — one bounded verdict on a recall

```text
  U_e = (ν · μ0 + Σ_h w_h · r_h) / (ν + Σ_h w_h)
  h ranges over retained outcome Hits for Episode e
  ν = 4, μ0 = 0, r_h ∈ [−1,1], w_h ≥ 0
```

`ν` and `μ0` are fixed illustrative prior defaults, subject to calibration.
`U_e=0` without attributed outcomes; `−1 ≤ U_e ≤ 1`. Keep the sufficient
statistics `Σ w*r` and `Σ w` rebuildable from retained Hits even after receipts
expire. An explicitly reported zero adds evidence weight with zero numerator;
missing reward adds nothing. Utility measures a reported usefulness proxy,
not truth, independent labels or causal attribution.

For the outcome-bearing commit, choose its distinct `adopted` IDs when that
field is present; otherwise choose all delivered primary IDs from the
receipt. An explicit empty list, or an empty recall, gives **no item
attribution**: retain the verdict at receipt/control level. Companions are not
independent primary items and do not receive extra outcome credit.

```text
  J = selected nonempty primary result set
  rank_j = original delivered rank, 0-based; do not rerank a selected subset
  a_j = (1 / (rank_j + 1)) / Σ_{l ∈ J} (1 / (rank_l + 1))
  b_je = 1 / |sources(j)|  if e ∈ sources(j), otherwise 0
  w_e = Σ_{j ∈ J} a_j · b_je
  Σ_e w_e = 1
```

An Episode is its own one-element source set. A Fact uses its receipt's
immutable 1–16-source snapshot. Merge overlapping Episode shares by addition
with **no extra cap**: all nonempty recall outcome credit is conserved. Store
the original ranks, selected IDs, source shares and resulting weights; do not
resolve current-generation IDs during later feedback.

Example: delivered rank 0 has `[E1]`, rank 1 has `[E1,E2]`, and both are
selected. `a=(2/3,1/3)`, hence `w_E1=5/6`, `w_E2=1/6`, sum 1. With `reward=-1`
and no previous outcomes, `U_E1=-5/29≈-0.172414`, `U_E2=-1/25=-0.04`.
A selected rank-1 item alone instead has `a=1`, not `1/2`.

For a returnable Fact, `U_f = mean_{e ∈ sources(f)} U_e`. This is explicitly a
**coupled heuristic**; correlated sources are not independent evidence. Entity
anchors and Communities have no attributed outcomes and use neutral `U=0` if
a score is needed for them.

For current total weight W, an additional outcome changes utility by
`U′−U = w·(r−U)/(ν+W+w)`. A non-positive reward need not lower U: if U is
already more negative than r, U increases toward r. No outcome changes mass.

### Adoption → Episode attribution

For one confirmed adopted Fact, distribute `κ=1` equally over its sources.
For a single commit's distinct adopted primary items J:

```text
  kappa_eff(e) = min(1, Σ_{j ∈ J, e ∈ sources(j)} 1 / |sources(j)|)
```

Adopting a four-source Fact gives each source `0.25`; a single Fact conserves
one unit. Overlapping multi-item adoption is **capped per Episode**, so do not
claim multi-item conservation for this adoption rule. The cap is not applied
to outcome weights. One `recall_hit` per recall/source is allowed: subsequent
commits cannot top up or repeat a recorded source's reinforcement; previously
unhit sources can get their first Hit. Persist the actual applied shares.

### Reinforcement — only recall_hit updates S

At server hit time `t_h`, with the positive attributed `κ_eff`:

```text
  R_hit = R(Δdays(t_h, t_last_hit), s)
  s′ = min(S_max,
           s · (1 + a · κ_eff · (exp(b·(1−R_hit)) − 1) · (s / 1 day)^(−c)))
  t_last_hit′ = max(t_last_hit, t_h)
  a = 5.0, b = 1.0, c = 0.1, S_max = 3650 days
  S0(m₀) ≤ s ≤ S_max, 0 < κ_eff ≤ 1
```

The power `(s / 1 day)^(−c)` is dimensionless. With valid state, `s′ ≥ s`;
the cap, zero elapsed time and machine precision make monotonicity **weak**,
not strict. For fixed pre-hit state and coefficient, a larger elapsed gap
increases the uncapped gain; strict spaced-greater-than-massed claims require
positive gaps, no saturation and a difference above numeric tolerance.
The stability factor reduces **relative** gain for already-stable memories,
not necessarily absolute gain. This is FSRS-inspired, not a fitted FSRS model
of users of this system.

```text
  s = 1 day, κ_eff = 1
    R_hit = 0.9, elapsed = 1 day           → s′ = 1.525855 days
    R_hit = 0.5, elapsed ≈ 12.789474 days  → s′ = 4.243606 days
    R_hit = 0.2, elapsed ≈ 102.315789 days → s′ = 7.127705 days
```

A negative outcome alone leaves `(s,t_last_hit,m)` unchanged at the same now.
Adoption plus a negative outcome has the **same accessibility as adoption
alone**, with a utility adjustment; it need not rank below the no-event case
because adoption can increase accessibility.

### The Hit node

```text
  (:Hit {id, t, kind, kappa_eff, namespace, idem_key}) -[:HIT_OF]-> (:Episode)
  idem_key = sha256(namespace, episode_id, kind)
```

`id` is server UUIDv7, `t` server ms. `kappa_eff` is positive only for
`recall_hit`, zero for the four other kinds. An `outcome` additionally retains
`reward`, `weight` and bounded `attribution` (contributing result IDs, original
ranks and source shares), plus audit versions sufficient to rebuild utility
without the receipt. The utility cache fields are `utility_reward_sum=Σw*r`
and `utility_weight=Σw`. Never encode reward as negative
`kappa_eff`. Hit targets are Episodes only; audit references to delivered Fact
IDs do not create Fact Hit targets. The same cause makes at most one Hit per
Episode and kind.

## 6. The commit path — the only Hit producer

### Durable RecallReceipt control records

Every issued memory context, including auto mode, has a durable append-only
`RecallReceipt`, separate from semantic Episodes and memory search. Retain
actual delivered primary/companion IDs, 0-based delivered primary ranks,
source snapshots, bounded derived snapshots needed after generation GC,
policy/config/generation versions, selected channels, ranking/cache state,
budget, result digest and context digest. Original source IDs are immutable;
no duplicate full raw Episode text is needed. Receipt persistence must
succeed before publishing a nonempty or feedback-capable response, not in an
in-memory ring that disappears on restart. A degraded empty response without
a durable receipt cannot accept feedback. A persisted impression records an
attempted publication, not proof of client consumption; a failed socket send
is delivery-unknown, never adoption or a negative label. Events append associated
control records (`RecallFeedback`; unique `RecallOutcome` acceptance for the
verdict); mutable lookup/idempotency caches are rebuildable.

Only included primaries receive delivered ranks. With D44, `limit` counts
primary bundles; complete source/contrast/supersedes companions and LF
separators count in the exact budgeted `context_text`, not as extra primary
outcome observations. An oversized bundle is skipped, later bundles are
considered, and `budget.limit=0` yields empty results/context and no item
attribution. Budget is `{unit, limit, tokenizer_id?}` with a nonnegative
integer limit. `utf8_bytes` and `unicode_scalars` counts are exact; `tokens`
requires an installed version/digest-pinned `tokenizer_id`, rejecting unknown
IDs, never estimating. Bundle selection checks the actual deduplicated
prospective `context_text`; `used_budget` counts the final included text,
including separators and mandatory warnings but excluding JSON
transport/diagnostics. The same structured items render that deterministic
LF-separated text; never truncate a claim or required warning to fit.
Absent budget uses the configured output cap; the hard RPC byte cap remains.

The explicit `receipt_ttl_ms` defaults to **3,600,000 ms (1 hour)**;
persist `expires_at = created_at + receipt_ttl_ms` on the receipt at issuance.
Restart does not shorten this window.
`now >= expires_at` rejects feedback, including retries, rather than silently
accepting it. Expiry bounds the feedback window, not utility history: retained
Episode outcome Hits remain authoritative; receipt-only outcomes with no
sources remain control audit, never invented Episode Hits. No receipt,
missing feedback or expired feedback is a negative label.

### Producer 1 — authenticated commit RPC (receipt clients)

A JSON-RPC example (IDs must identify an actual issued receipt and primary):

```json
{
  "jsonrpc": "2.0",
  "id": 17,
  "method": "commit",
  "params": {
    "recall_id": "0192f3b2-0000-7000-8000-000000000001",
    "adopted": ["0192f3b2-0000-7000-8000-000000000002"],
    "reward": -1
  }
}
```

Validate before any write:

- The authenticated caller's `hello.commit_mode` must be `receipt`, else
  `commit_mode_mismatch`. Auto-mode clients cannot fabricate adoption.
- Require an issued, authorized, unexpired receipt; unknown IDs reject
  `unknown_recall`, expired receipts reject `receipt_expired`.
- At least one of `adopted` or `reward` must be present (`empty_commit`).
  `adopted` must be an array of distinct delivered primary IDs; reject
  duplicates, unknown IDs and companion-only IDs. An empty array is valid.
- `reward` must be a finite number in `[-1,1]`; reject null, strings,
  booleans, non-finite or out-of-range values. **Do not clamp.** Presence is
  checked independently of truthiness, so `reward:0` is a real observation.
- If present, `adopted` drives adoption Hits. If reward is present, compute
  outcome attribution by §5.1. The outcome-bearing call's adopted field is
  authoritative; an earlier adoption-only call is not an implicit substitute
  for an absent adopted field. Later adoption cannot redistribute an outcome.
- One outcome per recall: an identical normalized reward/selected-set retry
  is a no-op; a different reward or attribution set rejects as a conflicting
  duplicate. Validate this before applying any new adoption in that request.
  Exact-once adoption is enforced per recall/source, independent of outcome.

### Internal transaction

```text
  commitHits(tx?, namespace, kind, captured_items, t_h = server_time)
    1. resolve immutable bounded source snapshots; compute adoption coefficients,
       outcome weights or audit-only targets according to kind
    2. validate current policy on all items, supports and sources; pin revision
    3. under the serialized write queue, recheck policy revision and idempotency;
       for receipt feedback also recheck receipt expiry/acceptance before effects
    4. for each new Episode/kind idem_key:
         verify hit_count against total Hit count; mismatch → replay first
         recall_hit → update s and t_last_hit
         outcome    → update only utility sufficient statistics
         all kinds  → append Hit and increment hit_count
    5. append receipt feedback events where applicable and SET caches atomically;
       recheck policy before commit; return applied counts (no denied text/IDs)
```

`captured_items` and attributed numbers are internal, never caller-supplied
source IDs, weights or stability. A policy change during recall/commit causes
retry or rejection **before response/feedback**; a torn-policy response or
partial renormalization over remaining sources is forbidden. An unchanged
policy revision still requires checking the receipt's captured items against
current policy. Structural generation changes alone do not alter captured
attribution. All state changes join one transaction; no partial adoption when
an outcome validation fails. The Hit/cache/control portion does not increment
`structure_revision`; policy uses its own stricter revision barrier.

### Producers 2–4 — audit only

- **Exposure:** after an auto-mode response is on the socket, call the path
  for the top three **delivered** primary IDs, not pre-budget candidates.
  The durable receipt already records the impression. An exposure failure
  cannot unsend bytes; report it operationally rather than swallowing it.
- **Re_mention:** in the extraction transaction, optionally record the
  duplicate relationship under `extract:<episode_id>`. Preserve **every
  original and every occurrence's separate extracted assertion/provenance**.
  A duplicate still creates its own immutable Fact and direct Episode
  provenance, links the previous occurrence through existing DERIVED_FROM /
  RELATES_TO roles as appropriate, and never automatically raises confidence,
  truth or accessibility. Same-transaction retries remain idempotent (D46).
- **Promotion:** in the dreaming transaction, record exactly the synthesis's
  bounded support Facts under `dream:<synthesis_fact_id>`, resolved to original
  Episodes. This does not reinforce them or change their `m0`.

These are the other three commit sites. Extraction/dreaming use their ambient
transaction so Fact and audit effects are atomic; receipt/exposure use a
standalone transaction. Policy checks also cover these internal producers.

Intra-Episode suppression is allowed only for an explicit same-speaker,
same-time, same-scope self-correction. Different speakers/reports, times or
modalities remain separate; unresolved contradictions use CONTRASTS (D42,
D46), not a later-text-wins or high-confidence-wins rule.

## 7. Replay — caches are functions of retained authority

```text
  replay(e, dynamics_config, utility_config):
    (s, t, n) = (S0(e.m0), e.ingested_at, 0)
    (sum_wr, sum_w) = (0, 0)
    for h in Hits(e) ORDER BY h.t ASC, h.id ASC:
      if h.kind == recall_hit: (s, t) = adoption_update(s, t, h.t, h.kappa_eff)
      if h.kind == outcome:    (sum_wr, sum_w) += (h.weight*h.reward, h.weight)
      n += 1
    U = (ν*μ0 + sum_wr) / (ν + sum_w)
    return (s, t, n, sum_wr, sum_w, U)
```

- The caches must equal this function under their recorded config version.
  `verify` compares ledger replay, and commit repairs a count mismatch. Count
  equality alone cannot prove cached values are correct; verification compares
  values as well. Replay does not mutate Hits, raw outputs, Episode ingestion
  metadata or `m0`.
- An inserted Hit ordered before an existing Hit by `(t,id)` triggers full
  Episode replay, not an incremental update in arrival order. Every adoption
  step clamps elapsed time and retains `max(t_last_hit,h.t)`, even before
  `ingested_at`. Normal logical server time is monotonic (docs/02 §1), but
  imports and equal timestamps still obey this ordering rule.
- No fixed sleep or live wall clock belongs in replay fixtures. Inject time,
  use fixed event sequences, and seed any generated sequence.
- `anamnesis rebuild --hit-cache` can discard/rebuild both accessibility and
  utility caches from retained originals/Hits. There are no checkpoints in
  this design; replay cost grows with that Episode's ledger, not an assumed
  guaranteed few-hundred-Hit bound. Rebuild is an operational job, not recall.
- A dynamics/utility configuration change is versioned, builds replacement
  caches by full replay, and publishes a consistent version. Receipts retain
  the version and values actually used. Neither replay nor refitting derives
  missing outcome labels from expired receipts. Earlier historical signed-S
  events, if imported, require an explicit audited format migration; never
  silently reinterpret them as confirmed adoption.

## 8. Where accessibility, utility and policy enter recall

```text
  score(x) = relevance(x) · max(m(x), ε)^γ · (1 + β · U_x)
  m(x) = m₀(x) · A_x
  γ = 0.5, ε = 0.02, β = 0.25, 0 ≤ β < 1
```

- Relevance retains the RRF channels/weights in docs/05. Channel RRF ranks
  start at **1**, while delivered receipt ranks start at **0**. With normalized
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
  `corroboration_root_count` and `operator_corrected` appear on results as
  provenance and never enter `relevance`, `m`, `U`, `score` or ordering;
  Facts from an Episode with unknown or truncated lineage are excluded from
  serving entirely rather than ranked lower (D49, D50).
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
| S_max | 3650 days | assumed cap; weak monotonicity only |
| κ adoption | 1.0 before source sharing | assumption; other kinds have no accessibility gain |
| ν, μ0 | 4, 0 | illustrative utility shrinkage prior |
| reward | [−1,1] | explicit finite client report; reject outside bounds |
| γ, ε, β | 0.5, 0.02, 0.25 | assumptions; `0≤β<1` |
| receipt_ttl_ms | 3,600,000 ms | explicit configurable feedback window |

Reject malformed/non-finite configuration at the boundary: require
`DECAY<0`, `FACTOR>0`, `S_base>0`, `λ≥0`, `S_max≥S_base·(1+λ)`, `σ_fact>0`,
`a,b>0`, `c≥0`, `ν>0`, `μ0∈[-1,1]`, `0<γ<1`, `0<ε≤1`, `0≤β<1`, valid
`[0,1]` priors and positive finite receipt TTL. If refitting DECAY while
preserving the stability definition `R(S,S)=0.9`, derive
`FACTOR=0.9^(1/DECAY)−1`; fitting both independently would redefine S.

Calibration uses retained delivered impressions and explicit adoption/outcome
reports, not self-generated exposure/remention/promotion as positive labels.
Separate the tasks:

1. Fit source-faithfulness confidence against annotated source/support pairs,
   including modality, corrections, unsupported syntheses and correlated
   sources. Do not fit truth from bounded span validation.
2. Evaluate accessibility as an adoption proxy conditional on exposure and
   relevance, with held-out time/session splits. Restricting to a top relevance
   band can reduce relevance confounding, not establish unbiased memory
   probability. No response/expired receipt is missing data, not failure.
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
- Adoption never lowers S or `t_last_hit`; cap and same-time hits permit
  equality. Fixed uncapped state gives larger gain for a larger gap. With
  `S0=1.5`, first hit at day 1, second after 0/1/30 days gives approximately
  `2.022748 / 2.539589 / 8.570575` days. At `S_max`, gaps cannot give strict gain.
- For all four non-adoption kinds, accessibility/cache time are unchanged;
  negative outcome alone leaves mass unchanged at the same now. Compare
  adoption-plus-negative with adoption-only, not no event.
- Single-Fact adoption shares sum to 1; multi-item adoption uses the Episode
  cap and is not globally conservative. Outcome attribution always sums to 1
  for a nonempty selected set, including overlap; no second cap is applied.
- Outcome utility is bounded, zero differs from missing, an all-one-source
  bundle gives weight 1, and selecting only a low-ranked item renormalizes it
  to 1. The `5/6,1/6` overlapping example yields `−5/29,−1/25` at reward −1.
- Reject malformed rewards, duplicate/unknown/companion adopted IDs and
  malformed configurations before effects. Explicit `adopted:[]` retains
  only receipt-level outcome; empty/budget-zero recall does the same.
- Identical feedback retry gives no extra Hits or utility; conflicting
  reward/attribution rejects atomically. Expiry boundary rejects at equality,
  restart preserves receipts, and receipt expiry does not lose utility replay.
- Replaying a fixed-seed set of 1,000 mixed event sequences matches cache
  updates, including out-of-order insertions and identical times ordered by
  ID. Replay never modifies m0 and generation switches never reset Episode S.
- Max-source accessibility chooses retention, not recency; unchanged sources
  do not grant replacement equality after source truncation. Sibling refresh
  is expected, not a failing per-Fact-selectivity test.
- Eligible mass-zero candidates retain a positive score factor, but admission
  through the bounded mass-ordered envelope is not guaranteed. Outcome only
  changes utility, and `1+βU` stays positive for accepted configurations.
- Current-policy barriers reject/retry races before serving or committing;
  denied support cannot leak via receipt feedback, provenance, conflict peers,
  cached profiles or historical T. Only explicit authenticated policy RPCs
  create policy controls; ingested instructions are data.
