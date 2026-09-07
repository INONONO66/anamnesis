# 03 — Time

The semantic time axis is **event time**. Operational server times are stored
for ingestion, Hits, policy events and receipts, but there is no bitemporal
transaction-time validity interval. `snapshot(T)` therefore means "what the
record currently says the world was like at T", not "what the system knew at T"
([10-decision-log](10-decision-log.md) D4).

## 1. Stored time

Only Episode and Fact carry a semantic event time. Control Episode event times
record command acceptance and are not selectable historical memory content.

| Property | Meaning |
|---|---|
| `time_value` | The original expression (`"2019-03"`, `"last Tuesday"`, an ISO string). For audit |
| `time_utc` | ms epoch used for comparison. Low precision maps to the start of the interval (`2019-03` → March 1, 00:00Z) |
| `time_precision` | `instant \| day \| month \| year \| inherited` |

- Episode: the time given by the source. Utterance time for a message,
  revision time for a document.
- Fact: **the time the claim took effect.** "I moved to Seoul in 2019" said in
  2026 gives Fact.time = 2019, Episode.time = 2026.
- Episodes additionally carry `ingested_at` (server ms), written once at
  CREATE, immutable, and **not used in snapshot computation** — audit and
  spool-drain ordering only.

## 2. Time resolution (extraction)

```text
  explicit    "March 2019", "yesterday" with an absolute reading     → that value, precision as given
  relative    "since last month", "three years ago"                  → computed against Episode.time, precision lowered
  none        a statement with no time reference                     → inherit Episode.time, precision = inherited
```

An inherited time says "this was true at the time of the utterance" and says
nothing about earlier. In snapshot(T) with T < Episode.time the Fact is not
visible — representing what we do not know about the past as "absent" is the
only honest choice.

## 3. visible(x, T)

Does x exist in `snapshot(T)`? The definition differs per kind, and for
Entity, Community and Link it is **derived**.

```text
  visible(Episode e, T)   = e.time_utc <= T
  visible(Fact f, T)      = f.time_utc <= T && visible_gen(f)
  visible(Entity n, T)    = visible_gen(n)
                          && n.visible_from_utc <= T
  visible(Community c, T) = visible_community(c, active[community], active[extraction])
                          && c.visible_from_utc <= T
  visible(Link l, T)      = visible_gen(l) && visible(l.from, T) && visible(l.to, T)
```

These are temporal/generation predicates only. Ordinary serving additionally
requires `allowed_now(x)` under the current pinned `policy_revision` (D43).
Policy/control Episodes and RecallReceipt records are never memory candidates
or conductors. `T` cannot restore a currently denied original or derived item;
confidence and low mass cannot substitute for policy/validity checks.

`visible_gen` is defined in [01-storage](01-storage.md) §4. Reasons:

- `Entity.visible_from_utc` is a rebuildable cache: the minimum visible time
  of its MENTIONS sources in that extraction generation. Active backfill may
  lower it in the same transaction that adds the mention.
- A Community generation captures each member's visibility threshold on
  HAS_MEMBER. For `k=max(1,ceil(0.5·member_count))`,
  `Community.visible_from_utc` is the k-th smallest captured threshold. The
  half-members threshold is therefore one property comparison at recall, not an
  unbounded member count. Empty communities are not created (there is no
  first member threshold for zero members).
- A Link exists iff both ends exist. Links have no time of their own.

Cached thresholds do not prove policy permission. An Entity needs a current
allowed supporting mention that is itself visible at `T`; an affected
Community/profile/synthesis must be suppressed until rebuilt from allowed
supports. Current policy gates endpoints and link content before conduction,
even when structural caches are stale.

The Entity witness is one more property comparison, not a per-request scan:

```text
  earliest_allowed_from(n, g_e, policy_revision)
    = min { e.time_utc : (e)-[:MENTIONS]->(n), e.generation ∈ {none, g_e},
            allowed(e, policy_revision) }        null if the set is empty

  witnessed(n, T) = earliest_allowed_from(n, g_e, policy_revision) <= T
```

`EntityWitness {generation, policy_revision, entity_id, earliest_allowed_from}`
rows are rebuilt for the active generation whenever `policy_revision`
advances, as part of the policy barrier's reconciliation, and are keyed by
`(generation, policy_revision)` only. Echo lineage adds nothing to this axis:
`echo_depth`, roots and receipt times are operational provenance and never
supply or shift a Fact's event time or its visibility at `T` (D49). There is no cache keyed by an arbitrary
request `T`; the request supplies `T` and compares. A null or missing row,
or an unavailable cache, excludes the Entity from candidates, seeds and
conduction; it never falls back to `visible_from_utc` alone (docs/01 §3.2,
§7).

### The DERIVED_FROM exception — provenance only

A backdated Fact (the move in §1) is visible at T = 2020 while its source
Episode (2026) is not. Two rules diverge here.

- **Provenance assembly**: an allowed source Episode **is attached.** Its
  later event time alone is not a reason to hide the citation. It is
  marked `provenance.derived_from[].visible_at_T = false`.
- **PPR conduction**: `visible(Link)` is false, so it **does not conduct.**
  Spreading inside snapshot(T) never leaks into future Episodes.

The exception is temporal, never a policy bypass. A denied source cannot
support a visible derived result; reject the whole affected Fact, rather than
dropping its denied source from attribution. Suppress denied companion and
superseded text too; a hidden conflict may produce only a redacted warning,
without its text, ID or hidden-peer count. The serving publication barrier
revalidates policy after assembly (docs/02 §1).

## 4. valid(x, T) — non-recursive INVALIDATES

```text
  invalidated(target, generation, T)
    = ∃ retained INVALIDATES edge OR replayed InvalidationMarker for target:
        owning/mapped generation matches && effective_time_utc <= T

  source_live(e, T) = !invalidated(e.id, 0, T)

  valid(Episode e, T) = visible(e, T) && source_live(e, T)

  support_valid(f, T) = true                                      if f is not synthesis
                      = ∀ u ∈ f.support_fact_ids : valid(u, T)     if f is synthesis

  valid(Fact f, T) = visible(f, T)
                   && ∃ e ∈ f.source_episode_ids : source_live(e, T)
                   && support_valid(f, T)
                   && !invalidated(f.id, f.generation, T)
```

An operator acceptance time is operational too. Reviewing or correcting an
adjudication never changes an `effective_time_utc`, never backdates an
existing edge and never removes one; a repair adds new records and, where
needed, a replacement Fact under §5 (D50).

An invalidator Fact's own validity is **not consulted.** "Does the original come back when its
invalidator is invalidated" is the doorway to recursion, cycles and rule
explosion, and we keep that door shut. Anything that should come back is
*created anew* by the replacement protocol in §5.

`source_live` deliberately does not require the source Episode itself to be
visible at T. A Fact backdated to 2019 from an Episode uttered in 2026 remains
visible in a 2020 world snapshot under the provenance exception, while a 2027
revision of that Episode can stop the Fact from 2027 onward.

Both existential INVALIDATES checks use bounded index seeks: the existing
relationship index `(target_id, generation?, effective_time_utc, id)` and the
marker index `(target_id, generation, effective_time_utc, evidence_id)`, with
`effective_time_utc <= T`, ordered by time/ID and `LIMIT 1` each. Either match
invalidates; duplicate edge/marker evidence does not change the predicate.
Validity never expands an unbounded incoming adjacency list. Complete marker
coverage is required before serving a reconciled generation.

Synthesis support is one bounded level: `support_fact_ids` may reference only
non-synthesis Facts. If any support becomes invalid, the synthesis becomes
invalid and the next dreaming run creates a replacement; it is never
resurrected recursively.

Temporal invalidation is not suppression, within or across generations. An
established INVALIDATES decision continues to determine validity when its
source is denied: preserve content-free target/effective-time/generation
markers in the non-serving view, rebuilt from retained authority (docs/01 §4).
No marker or denied-source text/IDs are returned or conducted. Policy
reconciliation is not semantic re-extraction and cannot remove these outcomes.
Activation requires preservation at every supported T or fails closed. This
keeps invalidation non-recursive and prevents suppression-induced resurrection.
Independently, `allowed_now(f)` fails if any materialized Episode authority or
required support Fact is denied. A policy revoke cannot manufacture outputs
that were never extracted while denied (D43).

Boundary fixture (A and B have live original authority; no other invalidation):

```text
  A.time = 10; B.time = 20; B -[:INVALIDATES]-> A
  deny B; reconcile generation 42 -> 43
  marker(target = A_in_43, generation = 43, effective_time_utc = 20)

  T                 9       10       19       20       21
  valid(A), before  false   true     true     false    false
  valid(A), after   false   true     true     false    false
```

B's content is suppressed after the deny; its invalidating effect is not.
Revoking the deny does not remove the marker. Omitting the marker or failing
to reconstruct its target mapping rejects activation rather than making A
valid at T=20. Every interval and equality boundary must agree, not just the
five illustrative T values.

- INVALIDATES is meaningful only as `Fact → Fact` and `Episode → Episode`
  (revisions).
- CONTRASTS has no effect on validity and elects no winner by confidence or
  recency. After primary ranking, assembly completes up to four valid,
  non-denied peers per primary through indexed adjacency with limit+1,
  deterministic peer-ID order, even if peers were not retrieval candidates.
  The scan reads at most 65 raw `CONTRASTS` rows: it inspects the first 64
  with bounded visibility, validity and policy checks and treats the 65th
  only as a has-more sentinel. `conflict_total` is the exact eligible count
  only when the scan exhausted the rows within 64 and found at most 4
  eligible peers; otherwise it is `null`, and `conflict_truncated=true`
  says the bundle is explicitly incomplete. A hidden peer yields only a
  redacted indicator, with no ID, text or hidden count. Budget admission includes the full permitted companion
  text and mandatory warnings, otherwise skips the primary bundle (D44, D46;
  docs/05). There is no unconditional both-sides guarantee beyond this bound.

## 5. Change vs correction

"A is no longer right" comes in two kinds, and the extraction judge outputs
`mode`.

```text
  change      the world changed.   "I moved"           → new Fact C, C.time = the event's time
  correction  the record was wrong. "No, that's not it" → new Fact C, C.time := B.time (B = the corrected Fact)
```

Both create `C -[:INVALIDATES]-> B`. The only difference is C.time.

```text
  change      T ──A────B────C──▶    in snapshot(B.time ≤ T < C.time) B is valid. B was true in the past
  correction  T ──A────B/C─────▶    in snapshot(T ≥ B.time) B was never valid at any T
```

Backdating a correction is **a direct consequence of defining Fact.time as
"effective time"**: a correction says B was wrong from the very moment it was
recorded as taking effect.

### Replacement protocol — restoring a wrongly invalidated A

If B invalidated A, B turned out to be wrong, and A is still true:

```text
  before               A  ◀──INVALIDATES── B
  correction C arrives A  ◀──INVALIDATES── B  ◀──INVALIDATES── C(time := B.time)
                       A is still invalid (non-recursive — the A ← B edge stands even though B is invalid)
  create replacement   A′ {content = A.content, time = A.time,
                           sub_kind = A.sub_kind, modality = A.modality}
                       A′ ─DERIVED_FROM─▶ A,  A′ ─DERIVED_FROM─▶ C     (provenance)
                       A′.source_episode_ids = [C's correction Episode]
                           + deterministic top 15 of authority(A)
                       A′ ─INVALIDATES──▶ A                             (A leaves for the whole range)
```

Absent later invalidation or denial, A′ is valid for T ≥ A.time, and A is
invalid over that range; B is invalid for T ≥ B.time and not yet visible before
it. The replacement protocol itself adds no duplicate current exposure, and
the validity definition stays one hop. A′ retains the same meaning-bearing
entity/property bindings; its confidence is judged for source-faithfulness and
its immutable `m0` uses the pinned generation priors. Its accessibility is
computed from its bounded Episode authority, so retained source histories
carry over, while a truncated source does not. This is Episode-shared
accessibility, not a per-Fact reinforcement guarantee
([04-forgetting](04-forgetting.md) §3 — one of the reasons Hits attach to
Episodes).

The extraction judge decides whether a replacement is needed by looking up
what the corrected Fact B had itself invalidated (if B carries INVALIDATES and
C reverses it, create A′). If the LLM misses it, A stays invalid — a better
failure mode than automatic restoration by a recursive rule: an invisible fact
can be re-stated by the user, but a wrongly resurrected fact produces silent
wrong answers.

### Operator correction uses the same protocol (D50)

When the adjudicator itself was wrong and no user correction exists, the
repair is an authenticated `adjudication.correct` call
([02-daemon-and-pipelines](02-daemon-and-pipelines.md) §5.2), never a delete
and never an edit of history. The daemon first appends a CREATE-only
`anamnesis.operator-adjudication/1` Episode. That Episode is excluded from
ordinary search, extraction and PPR, and its deterministic renderer states
what the operator decided; it never impersonates user prose, because
fabricating a user correction would be the same class of error the repair is
fixing.

Restoring a wrongly invalidated A then follows the protocol above, with A′
appended in A's ACTIVE generation. A′ copies A's meaning and effective time,
its exact 1..16 source authorities, bounded support IDs, confidence,
`m0`/version fields and corroboration roots without a boost, derives from A,
and invalidates A so exactly one copy of that meaning serves. Every bad edge
is retained. The evidence seek is ordered by
`(effective_time_utc ASC, evidence_id ASC)` with `LIMIT 65`: the request names
1..8 bad evidence IDs and the correction record carries at most 64 other
retained incoming evidence IDs onto A′ as content-free markers. A 65th row,
or a named ID outside the inspected set, rejects
`repair_evidence_overflow` rather than bypassing another valid invalidation.
The mistaken invalidator B stays valid unless it is independently corrected,
and non-recursive validity is unchanged.
A′ keeps `A.time`, so it serves every `T >= A.time` once the correction
commits, and operator acceptance time is audit-only: it never becomes an
effective time and never shifts a snapshot boundary. Ordinary recall labels
the repaired provenance `operator_corrected`
([05-recall](05-recall.md) §6). Operator acceptance is a decision record, not
a human-gold label, and it authorizes no unattended invalidation elsewhere.

## 6. Why no transaction time

Bitemporal storage (event time × record time) answers "what did the system
know then" exactly, but it attaches two interval axes to every derived
element and forces the record axis to be recomputed on every generation
switch, re-extraction and correction. What the user of a personal memory
engine actually asks is "what was the world like then"; "what did the system
know then" is a debugging question.

Debugging uses `Episode.ingested_at`, generation integers, Hit server time and
durable RecallReceipt impressions (D45, D47). A structural revision log alone
cannot reconstruct a result: policy/config versions, selected channels,
candidate/index/cache state, degradation decisions, mass/utility reads, `now`
and exact output budgeting also matter. Receipts capture bounded source/result
snapshots and ranking evidence, not a complete historical database. Their
explicit retention window bounds feedback acceptance, not semantic validity;
missing or expired feedback is never a negative utility label. Current policy
still governs ordinary historical recall and feedback after a restart.

## 7. Cypher sketch

Candidate search and envelope expansion apply visible(T) plus current policy
before admitting seeds or conductors. Generation-scoped indexes exclude hidden
generations before top-k;
materialized visibility thresholds avoid nested graph scans.

```cypher
// Temporal candidate subquery, not a complete serving query.
// The application selects this validated index from pinned active generation 43.
// Apply current policy to every returned row before seed admission.
CALL db.index.vector.queryNodes('vec_fact_g43_bge_m3_1024', 256, $q)
YIELD node AS f, score
WHERE f:Fact
  AND f.time_utc <= $T
RETURN f.id, score
ORDER BY score DESC, f.id ASC
LIMIT 64
```

```cypher
// Entity temporal visibility plus the current-policy witness (docs/03 §3).
MATCH (n:Entity {id: $entity_id})
WHERE n.generation = $g_e AND n.visible_from_utc <= $T
MATCH (w:EntityWitness {generation: $g_e, policy_revision: $policy_revision,
                        entity_id: n.id})
WHERE w.earliest_allowed_from IS NOT NULL AND w.earliest_allowed_from <= $T
RETURN n.id
```

valid(T) is not applied at the candidate or envelope stage — an invalid but
non-denied Fact may still conduct (the Entities connected through it can
remain relevant). A denied Fact never conducts, regardless of validity.
valid is applied **only at result assembly**; invalid Facts drop out of the
results but allowed invalidated Facts may appear in bounded supersedes
provenance, never through unbounded recursive traversal
([05-recall](05-recall.md) §6).
