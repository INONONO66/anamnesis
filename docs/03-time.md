# 03 — Time

The semantic time axis is **event time**. Operational server times are stored
for ingestion, Hits, policy events and Usage, but there is no bitemporal
transaction-time validity interval. `snapshot(T)` therefore means "what the
record currently says the world was like at T", not "what the system knew at T"
([10-decision-log](10-decision-log.md) D4).

## 1. Stored time

Only Episode and Fact carry a semantic event time.

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
  CREATE and immutable. `snapshot(T)` never reads it; the `known_by`
  predicate and the retention clock do (§6).

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

Does x exist in `snapshot(T)`?

```text
  visible(Episode e, T) = e.time_utc <= T
  visible(Fact f, T)    = f.time_utc <= T && f.generation = selected extraction generation
  visible(Link l, T)    = visible(l.from, T) && visible(l.to, T)
```

These are temporal and generation predicates only. Serving also requires the
current policy to allow the item (D43). `T` can't restore a currently denied
original or derived item, and confidence or low mass never substitutes for a
policy or validity check.

A Link exists iff both ends exist; links have no time of their own. Entities
carry no event time and are not recall results. An Entity is admitted to a
generation through its policy witness, checked once at cutover
(docs/01 §3.2), not through a per-request scan. Echo lineage adds nothing to
this axis: `echo_depth`, roots and Usage timestamps are operational
provenance and never supply or shift a Fact's event time or its visibility at
`T` (D49).

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
dropping its denied source from attribution. Denied companion and superseded
text is withheld too, with a warning in its place (docs/05 §6).

## 4. valid(x, T) — non-recursive INVALIDATES

```text
  invalidated(target, T)
    = ∃ INVALIDATES link for target: effective_time_utc <= T
                                     && known(link, known_by)

  valid(Episode e, T) = visible(e, T) && !invalidated(e.id, T)
  valid(Fact f, T)    = visible(f, T) && !invalidated(f.id, T)
```

A Fact is also dropped at assembly when any of its source Episodes is not
servable: denied by policy, invalidated by a revision effective at T, or not
yet known at `known_by`.

An invalidator Fact's own validity is **not consulted.** "Does the original come back when its
invalidator is invalidated" is the doorway to recursion, cycles and rule
explosion, and we keep that door shut. Anything that should come back is
*created anew* by the replacement protocol in §5.

The check is a bounded index seek on the relationship index
`(target_id, effective_time_utc, id)` with `effective_time_utc <= T`. Validity
never expands an unbounded incoming adjacency list.

INVALIDATES links carry `ingested_at`, copied from the invalidating element's
`ingested_at` (null means always known). Under a recall `known_by`, a link
with `ingested_at > known_by` is not yet known and does not invalidate its
target; without `known_by` the predicate above is unchanged (D57).

- INVALIDATES is meaningful only as `Fact → Fact` and `Episode → Episode`
  (revisions).
- CONTRASTS has no effect on validity and elects no winner by confidence or
  recency. Recall attaches up to four peers per primary as companions
  (docs/05 §6).

## 5. Change vs correction

A Fact→Fact INVALIDATES link is written when the relation judge (D53) decides
that a new Fact invalidates an existing one. The link copies `target_id` and
the invalidating Fact's `effective_time_utc` and `ingested_at`. From that
time onward the target isn't valid; before it, the target still serves in
historical snapshots.

```text
  T ──A────────C──▶    C invalidates A
                       snapshot(T < C.time): A is valid
                       snapshot(T ≥ C.time): A is not valid, and A appears
                                             under C's provenance.supersedes
```

The pipeline has no separate correction mode that backdates the invalidating
Fact to the target's time, and no replacement Fact that restores a wrongly
invalidated target. Because INVALIDATES is non-recursive (§4), invalidating C
doesn't bring A back; a user restating A creates a new Fact. Both designs are
recorded in the decision log archive
([D18](history/decision-log-archive.md#d18--corrections-are-backdated-ctime--btime))
and listed in [09-roadmap](09-roadmap.md).

## 6. Why no transaction time

Bitemporal storage (event time × record time) answers "what did the system
know then" exactly, but it attaches two interval axes to every derived
element and forces the record axis to be recomputed on every generation
switch, re-extraction and correction. What the user of a personal memory
engine actually asks is "what was the world like then"; "what did the system
know then" is a debugging question.

Debugging uses `Episode.ingested_at`, generation integers, Hit server time and
durable Usage records (D62). Serving reads
`ingested_at` in two places. The recall `known_by` parameter
([05-recall](05-recall.md) §1) is a single `ingested_at <= known_by`
predicate over Episodes, Facts and INVALIDATES links, no interval axis and no
per-element valid-from/valid-to. And for an element with no Hit,
`ingested_at` is the retention clock origin of its mass (Episode mass and the
Fact accessibility A_Fact over its source Episodes).
D57 records it as a narrow exception to D4, not a reversal. A structural
revision alone can't reconstruct a result: policy, index and cache state,
`now` and the output budget also matter. Usage captures the final ordered
source attribution.

## 7. Where the predicates run

`valid(T)`, including `known_by`, is applied at the candidate stage: every
Fact channel (BM25, vector) and every Episode channel filters on it, so a Fact
with an effective known INVALIDATES at T never enters the candidate set and
never seeds PPR. Source policy is not a channel predicate for Facts: a Fact
whose source Episode is denied can still be a candidate and a seed, and is
dropped at assembly. The PPR graph is built from the generation's arcs without
re-checking peer validity, so an invalidated Fact can still be a graph peer
that carries mass between seeds. It's never a seed and never a result.

Validity is checked again at assembly. When an invalidating Fact is served,
the Facts it invalidates are attached under `provenance.supersedes` (at most
8, `time_utc DESC, id ASC`, policy-denied content redacted), never through
recursive traversal ([05-recall](05-recall.md) §6). The `time` window and
`known_by` are candidate-stage predicates on every channel, and the window is
re-applied to the ranked list before packing. The predicates are
`RECALL_ALLOWED` and `FACT_VALID` in
`packages/core/src/store/recall-store.ts`.
