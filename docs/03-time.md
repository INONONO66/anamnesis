# 03 — Time

The only stored time is **event time**. "When the system learned it"
(transaction time) is not stored. `snapshot(T)` therefore means "what the
world was like at T", not "what the system knew at T"
([10-decision-log](10-decision-log.md) D4).

## 1. Stored time

Only Episode and Fact carry a time.

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

`visible_gen` is defined in [01-storage](01-storage.md) §4. Reasons:

- `Entity.visible_from_utc` is a rebuildable cache: the minimum visible time
  of its MENTIONS sources in that extraction generation. Active backfill may
  lower it in the same transaction that adds the mention.
- A Community generation captures each member's visibility threshold on
  HAS_MEMBER. For `k=max(1,ceil(0.5·member_count))`,
  `Community.visible_from_utc` is the k-th smallest captured threshold. The
  majority rule is therefore one property comparison at recall, not an
  unbounded member count.
- A Link exists iff both ends exist. Links have no time of their own.

### The DERIVED_FROM exception — provenance only

A backdated Fact (the move in §1) is visible at T = 2020 while its source
Episode (2026) is not. Two rules diverge here.

- **Provenance assembly**: the source Episode **is attached.** When the user
  asks "where did this fact come from" there must always be an answer. It is
  marked `provenance[].visible_at_T = false`.
- **PPR conduction**: `visible(Link)` is false, so it **does not conduct.**
  Spreading inside snapshot(T) never leaks into future Episodes.

## 4. valid(x, T) — non-recursive INVALIDATES

```text
  source_live(e, T) = ¬∃ (y)-[:INVALIDATES]->(e) : y.time_utc <= T

  valid(Episode e, T) = visible(e, T) && source_live(e, T)

  support_valid(f, T) = true                                      if f is not synthesis
                      = ∀ u ∈ f.support_fact_ids : valid(u, T)     if f is synthesis

  valid(Fact f, T) = visible(f, T)
                   && ∃ e ∈ f.source_episode_ids : source_live(e, T)
                   && support_valid(f, T)
                   && ¬∃ (y)-[:INVALIDATES]->(f) :
                          y.time_utc <= T && visible_gen(y) && visible_gen(link)
```

An invalidator Fact's own validity is **not consulted.** "Does the original come back when its
invalidator is invalidated" is the doorway to recursion, cycles and rule
explosion, and we keep that door shut. Anything that should come back is
*created anew* by the replacement protocol in §5.

`source_live` deliberately does not require the source Episode itself to be
visible at T. A Fact backdated to 2019 from an Episode uttered in 2026 remains
visible in a 2020 world snapshot under the provenance exception, while a 2027
revision of that Episode can stop the Fact from 2027 onward.

Both existential INVALIDATES checks compile to a composite relationship-index
seek on `(target_id, generation?, effective_time_utc, id)` with
`effective_time_utc <= T`, ordered by time/id and `LIMIT 1`; validity never
expands an unbounded incoming adjacency list.

Synthesis support is one bounded level: `support_fact_ids` may reference only
non-synthesis Facts. If any support becomes invalid, the synthesis becomes
invalid and the next dreaming run creates a replacement; it is never
resurrected recursively.

- INVALIDATES is meaningful only as `Fact → Fact` and `Episode → Episode`
  (revisions).
- CONTRASTS has no effect on validity. Both Facts stay valid and appear
  together in recall results as a contradiction — resolution is left to the
  user or to a later utterance.

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
  create replacement   A′ {content = A.content, time = A.time, sub_kind = A.sub_kind}
                       A′ ─DERIVED_FROM─▶ A,  A′ ─DERIVED_FROM─▶ C     (provenance)
                       A′.source_episode_ids = [C's correction Episode]
                           + deterministic top 15 of authority(A)
                       A′ ─INVALIDATES──▶ A                             (A leaves for the whole range)
```

Result: for every T ≥ A.time, A′ is valid and A and B are invalid. No
duplicate exposure, and the validity definition stays one hop. The cost is one
Fact. A′'s mass is computed from its bounded materialized source Episodes, so
the hit history A accumulated carries over to A′
([04-forgetting](04-forgetting.md) §3 — one of the reasons Hits attach to
Episodes).

The extraction judge decides whether a replacement is needed by looking up
what the corrected Fact B had itself invalidated (if B carries INVALIDATES and
C reverses it, create A′). If the LLM misses it, A stays invalid — a better
failure mode than automatic restoration by a recursive rule: an invisible fact
can be re-stated by the user, but a wrongly resurrected fact produces silent
wrong answers.

## 6. Why no transaction time

Bitemporal storage (event time × record time) answers "what did the system
know then" exactly, but it attaches two interval axes to every derived
element and forces the record axis to be recomputed on every generation
switch, re-extraction and correction. What the user of a personal memory
engine actually asks is "what was the world like then"; "what did the system
know then" is a debugging question.

Debugging has partial substitutes: `Episode.ingested_at`, generation integers,
Hit.t (server time). If "the recall result as of revision N" ever becomes
necessary, a revision log on Meta is enough and the schema does not change.

## 7. Cypher sketch

Candidate search and envelope expansion put visible(T) **in the WHERE
clause**. Generation-scoped indexes exclude hidden generations before top-k;
materialized visibility thresholds avoid nested graph scans.

```cypher
// The application selects this validated index from pinned active generation 43.
CALL db.index.vector.queryNodes('vec_fact_g43_bge_m3_1024', 256, $q)
YIELD node AS f, score
WHERE f:Fact
  AND f.time_utc <= $T
RETURN f.id, score
ORDER BY score DESC, f.id ASC
LIMIT 64
```

```cypher
// Entity visibility (neighbor filter during envelope expansion)
WITH n
WHERE n.generation = $g_e AND n.visible_from_utc <= $T
```

valid(T) is not applied at the candidate or envelope stage — an invalid Fact
is still a conductor (the Entities connected through it are still relevant).
valid is applied **only at result assembly**; invalid Facts drop out of the
results but are exposed through the INVALIDATES chain in `provenance`
([05-recall](05-recall.md) §6).
