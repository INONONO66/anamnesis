# 05 — Recall

Recall is LLM-free and semantically read-only: returning context never creates
memory and never reinforces accessibility. It does write one durable Usage
before it answers (§10). The pipeline described here is the one the `recall`
RPC runs, named `derived-hybrid-v1` in diagnostics and implemented by
`Store.recall` in `packages/core/src/store/recall-store.ts`. Retrieval is
Fact-first: Fact hits resolve their source Episodes through `DERIVED_FROM`,
and Episodes stay provenance and context (D56). Channels and bounds that are
designed but not built are listed in [09-roadmap](09-roadmap.md).

```text
  recall(query, session?, T = now, limit = 10, window = 2, budget?, time?, order?, known_by?)
    │
    ├─ ① pin         usage_id · T · now · policy_revision · selected generation · query embedding
    │
    ├─ ② candidates  Episode: exact ID(1) · BM25(64) · session(32)
    │                Fact:    BM25(64) · vector(64), selected generation only
    │
    ├─ ③ PPR         every candidate seeds uniformly; seed-adjacent ConductingArc rows (LIMIT 1024)
    │
    ├─ ④ fusion      RRF over channel ranks = relevance
    │
    ├─ ⑤ assembly    m(now), U · score · valid(T) · policy · supersedes · CONTRASTS peers · session window
    │
    ├─ ⑥ packing     exact budget, whole bundles
    │
    └─ ⑦ response    durable Usage → results[≤ limit] · companions · context_text · diagnostics
```

## 1. Pin

These are fixed for one request.

| Value | Meaning |
|---|---|
| `usage_id` | Server UUIDv7 for the final selection, parent lineage and the optional explicit outcome |
| `T` | Snapshot time. Defaults to now. For a question about the past the caller supplies it |
| `now` | Server ms. Reference for forgetting (docs/04 §4) |
| `policy_revision` | Current suppression policy, whatever `T` is |
| selected generation | The extraction generation the selector points at. Fact channels and PPR read only it |
| request options | `session`, `limit` (0..64, default 10), `window` (0..8, default 2), `budget`, `time`, `order`, `known_by` |
| query embedding | One provider call on the caller's verbatim query with the profile's query prefix. On failure the vector channel is absent |

The query text is used **as the caller wrote it** (D48). BM25 runs on that
exact string and the vector channel embeds the same string. The daemon never
translates the query and never issues a second generated query. The offline
comparison behind that default is in
[research/retrieval-fusion](research/retrieval-fusion.md); it measured 20/20
against 19/20 Hit@1 on a small development set and settles nothing beyond "no
mandatory translation".

### Time window, order and known_by

`time = { from?, to? }` narrows the result to a window of `time_utc`. `to`
defaults to T; `to > T` or `from > to` is `invalid_params`. The window is a
hard filter, applied to every candidate channel and again to the ranked list
before packing. It isn't a score adjustment. CONTRASTS peers delivered as
companions are bounded only by `time_utc <= T`, so a companion outside
`[from, to]` can accompany an in-window primary as context, not as a result.

`order` is `"relevance"` (default) or `"chronological"`. Selection is the
same either way: the top `limit` primaries by score. `chronological` changes
only the presentation order of the selected primaries, `time_utc ASC, id
ASC`, and ranks follow that presentation order.

`known_by` has the same type as T. When present, only Episodes and Facts
with `ingested_at <= known_by` are served, and an INVALIDATES link whose own
`ingested_at` is later than `known_by` doesn't invalidate its target (a null
link `ingested_at` is always known). This is one predicate over one stored
timestamp, not a second time axis (D57).

## 2. Candidate channels

| Channel | Kind | Source | Size | Order |
|---|---|---|---|---|
| `identity` | Episode | the query string as an exact Episode ID | 1 | none |
| `bm25` | Episode | fulltext `element_content` (cjk analyzer) | 64 | Lucene score DESC, id ASC |
| `session` | Episode | latest Episodes of the request's `session` | 32 | `time_utc DESC, ingest_seq DESC, id ASC` |
| `bm25` | Fact | fulltext `element_content`, selected generation | 64 | Lucene score DESC, id ASC |
| `vector` | Fact | `vec_fact_<profileId>`, selected generation | 64 | cosine DESC, id ASC |

- `identity` is an exact Episode-ID lookup. Diagnostics name it
  `identity_mode: "exact_episode_id"`. It isn't an Entity or person channel.
- Fulltext and vector queries over-fetch 256 rows and filter down to 64. If
  fewer survive, recall proceeds with what it has. An empty Lucene query
  leaves BM25 absent.
- A vector row counts only when its `profile_id`, `input_revision` and
  `input_digest` still match the Fact, so a stale vector can't surface a
  changed Fact.
- Every Episode channel requires the Episode to be visible at T, known at
  `known_by`, inside the `time` window, not selected by an active deny and not
  invalidated by an effective known revision. Every Fact channel requires the
  selected generation, the `time` window and `valid(T)` (docs/03 §4, §7).
- Source policy for a Fact is checked at assembly, not in the channel
  predicate: a Fact with a denied source can be a candidate and a seed, and is
  dropped before it can be returned.
- Episodes and Facts share these channels and caps whatever language they are
  stored in. There is no language quota and no translated candidate set. A
  Fact with no vector is absent from the vector channel only.
- An assistant Episode with unknown or truncated lineage produces no Facts
  (docs/01 §3.3), so nothing derived from it reaches the Fact channels.

## 3. Seeds

Every distinct candidate, Episode or Fact, seeds PPR with weight 1. The solver
normalizes the seed vector. There is no seed ranking, cap or degree damping.

## 4–5. Envelope and PPR

Recall reads the ConductingArc rows of the selected generation whose
`source_id` or `peer_id` is a seed, `LIMIT 1024`, and runs the CSR solver over
the seeds plus the endpoints of those rows. Solver defaults are `α = 0.85`,
iterate-delta tolerance `1e-4` and at most 64 iterations; every role has
weight 1 (docs/06 §4, §5). The solve has no deadline: its input is bounded and
a deadline would make the same input produce different output.

PPR is absent when no extraction generation is selected or nothing seeds it;
diagnostics then report `ppr_used: false` and recall uses channel ranks alone.

The `graph.envelope` RPC and its ordered ConductingArc probes are a separate
diagnostic surface ([06-envelope-ppr](06-envelope-ppr.md)). Recall doesn't
call it.

## 6. Fusion and assembly

### relevance — RRF

```text
  relevance(x) = Σ_L  w_L / (60 + rank_L(x))
                 L ∈ { vector .25, bm25 .25, ppr .30, session .15, identity .05 }
                 weights of absent lists are redistributed proportionally to the rest
```

RRF uses ranks only, because cosine, Lucene and PPR mass have no common unit.
`rank_L` starts at 1. Episodes fuse the `identity`, `bm25` and `session` lists.
Facts fuse `bm25`, `vector` and a `ppr` list: the candidate Facts with positive
PPR mass, by mass DESC then id ASC. With normalized nonnegative weights,
`0 < relevance <= 1/61` for a present item. The weights are assumptions, not
measured values.

### Mass and score

```text
  A_Episode = R(elapsed_days, S)
  A_Fact    = max_source R(elapsed_days, S · sigma_fact)   sigma_fact = 30
  m(x)      = m0(x) · A_x
  U_Episode = cached utility anchor decayed to now          (docs/04 §7)
  U_Fact    = mean of U_Episode over its source Episodes
  score(x)  = relevance(x) · max(m(x), ε)^γ · (1 + beta · U_x)
              γ = 0.5, ε = 0.02, beta = 0.25
```

`S` and the last-hit time come from the Episode's HitCache row; an Episode
with no row uses its initial stability and `ingested_at`. Episode-only
accessibility refreshes sibling Facts that share a source. The max selects the
highest retention, not the most recent source. `U_Fact` is a source-mean
heuristic. The utility multiplier is in [0.75, 1.25]. Policy and validity are
hard filters, never score adjustments. Served `score`, `relevance`, `mass` and
`utility` are rounded to six decimals so the rendered context doesn't drift
with the wall clock.

### Filters and ordering

1. **Kind**: results are Facts and Episodes. Entities are not results; the
   `entities` array is always empty.
2. **valid(T)**: a Fact or Episode with an effective known INVALIDATES at T is
   never a primary. When the invalidating element is served, what it
   invalidated is attached under its `provenance.supersedes`: at most 8,
   `time_utc DESC, id ASC`. Policy-denied content there sets
   `supersedes_redacted` with a `supersedes_withheld` warning, and more than 8
   adds a `supersedes_incomplete` warning.
3. **Policy**: a Fact is dropped when any of its source Episodes (at most 16)
   is not servable. Historical provenance exceptions bypass time, never
   suppression (docs/03 §3).
4. **Ordering**: `score DESC, relevance DESC, mass DESC, id ASC`, then
   companion completion and greedy packing below. `limit` counts included
   primary bundles only; ranks are assigned **after** packing as contiguous
   0-based positions.

### Result item

```json
{
  "id": "01920000-0000-7000-8000-000000000001",
  "kind": "Fact", "schema": "anamnesis.claim/1", "epistemic": "derived",
  "content": "Mira prefers tea.",
  "time": {"utc": 1700000000000, "precision": "day"},
  "score": 0.01008, "relevance": 0.016, "mass": 0.36,
  "utility": 0.2, "rank": 0,
  "sources": ["01920000-0000-7000-8000-000000000002"],
  "window": {"source": "01920000-0000-7000-8000-000000000002", "before": [], "after": []},
  "provenance": {
    "derived_from": [{"id": "01920000-0000-7000-8000-000000000002", "kind": "Episode", "visible_at_T": true}],
    "supersedes": [], "supersedes_redacted": false,
    "contrasts": [], "warnings": []
  },
  "channels": ["vector", "bm25", "ppr"]
}
```

Here `score = .016 × sqrt(.36) × (1 + .25 × utility)` (rounded). The schema
is `RpcRecallItem` in `packages/protocol/src/rpc.ts`. An Episode item has
exactly one source, itself, and no `window`. `provenance.derived_from` is the
bounded Episode source list under the time-only exception (docs/03 §3).
Client-supplied sources and ranks are never trusted for credit; the stored
Usage selection is authoritative.

### Occurrences and bounded conflict bundles (D46)

Each occurrence's extracted assertion is a separate immutable Fact, even when
another Fact says the same thing. Repetition is not a truth or confidence
boost, and recall doesn't group occurrences.

For each prospective Fact primary, recall reads its `CONTRASTS` peers in the
selected generation with `time_utc <= T`, ordered by peer id, `LIMIT 4`, and
keeps the peers that pass the current-policy source check.
`provenance.contrasts` lists their IDs and their full items are in top-level
`companions`. A companion has no rank and receives no outcome credit.
Completion is one hop. No winner is inferred from confidence or recency. The
selected peers and any warnings are mandatory bundle content: if the complete
bundle doesn't fit the budget, the primary is skipped.

Occurrence grouping, a counted conflict scan and its completeness flags are
[09-roadmap](09-roadmap.md) targets.

### Exact output budget (D44)

`limit` is a nonnegative integer, default **10**, maximum **64**. Request
examples:

```json
{"query":"tea preference","limit":10,"budget":{"unit":"utf8_bytes","limit":4096}}
```

```json
{"query":"tea preference","limit":10,"budget":{"unit":"tokens","limit":1024,"tokenizer_id":"cl100k_base@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}
```

The tokenizer ID above is illustrative and is rejected unless that exact
installed version and digest exists. Units are `utf8_bytes`,
`unicode_scalars` or `tokens`; the limit is a nonnegative safe integer.
Unknown units, malformed Unicode and token budgets with a missing, unknown or
unpinned tokenizer ID are rejected. `tokenizer_id` is allowed only for
`tokens`. Nothing is estimated and nothing falls back to bytes. Installing a
tokenizer is described in
[embedding-recall](../app/anamnesis/embedding-recall.md).

`context_text` is rendered by `canonical-jsonl-v1`: LF-separated records, no
trailing LF, each the canonical JSON of one final result item with its content
in full. Primaries are walked in rank order; each emits its own record if not
yet emitted, then its not-yet-emitted companions. Each item ID is emitted
once. Structured `results` and `companions` render to exactly this text.

Packing is greedy over the sorted primary bundles. For each bundle, build the
actual deduplicated prospective `context_text` and count it exactly. Include
the bundle only if the whole text fits the budget and the full serialized
response fits the 1 MiB RPC cap. Skip an oversized bundle and consider later
ones until `limit` primaries are included or candidates end. Claims, sources,
supersedes entries and warnings are never truncated to fit. A tokenizer may
merge across record boundaries, so the whole prospective string is counted,
not a sum of per-item counts. Budget 0 or limit 0 yields `results: []`,
`companions: []`, `context_text: ""` and `used_budget: 0`.

Each primary Fact carries `window: {source, before, after}`. Its source
Episode anchors up to `window` eligible same-session Episodes on either side,
closest first by `ingest_seq`. They are optional Episode companions with zero
relevance and score. Packing first tries the full window, then drops optional
companions farthest first until the budget, the response cap and the
256-companion cap fit. `window.before` and `window.after` list only delivered
IDs. Mandatory contrast companions and warnings are never stripped.

`used_budget` counts the exact final `context_text` including separator LFs:
UTF-8 length, Unicode scalar count (not UTF-16 code units), or the pinned
tokenizer's token count. JSON transport escaping and diagnostics are outside
this count and inside the 1 MiB response cap. Without `budget`, the server
uses its configured output cap in `utf8_bytes` (default 65,536,
`ANAMNESIS_RECALL_DEFAULT_BYTES`, at most 1 MiB) and echoes it. For the raw
string represented by JSON `"A\né🙂"` the counters are 8 UTF-8 bytes and 4
scalars.

### `epistemic` — producer and derivation distance

Every result names the process that wrote it. The value is derived from
`schema` at assembly; nothing is stored. It describes derivation distance, not
truth, reliability or a calibrated probability (D41).

| `epistemic` | Producer | Result kind |
|---|---|---|
| `observed` | ingest: the verbatim original | Episode |
| `derived` | extraction: a model's reading of one or more Episodes | Fact |

Every derived item bottoms out in Episodes through `provenance.derived_from`
and `sources`. Original text may be false and an extracted claim may be
faithful; no reliability order follows from this field.

## 7. Consistency — structure_revision

Policy read, candidate reads, cached dynamics, companion completion, packing
and Usage creation run in one transaction under the Meta write barrier. A
response is assembled from one view. Recall doesn't read `structure_revision`
twice and has no torn mode. Mass and utility inputs are read once, at
assembly.

### Policy revision barrier (D43)

Recall and policy commands serialize on the same barrier, so once a policy
command acknowledges, no later response is published under its predecessor
(docs/02 §1). Historical T can't bypass today's policy. `report_outcome`
revalidates every credited source in its own write transaction. The policy
rules are in docs/01 §3.2.

## 8. Degradation ladder

Nothing degrades into a silent partial result. A retrieval failure drops a
whole channel, a bundle that can't be completed is skipped whole, and policy
or storage failures reject without context.

| Situation | Behavior | Reported as |
|---|---|---|
| No embedding provider | vector channel absent | `vector_reason: not_configured` |
| Query embedding fails | vector channel absent | `vector_reason: provider_unavailable`, `provider_rejected`, `profile_mismatch`, `invalid_vector` or `input_too_large` |
| Empty Lucene query | BM25 absent | `channels_used` lacks `bm25` |
| No selected generation, or no candidates | PPR absent | `ppr_used: false` |
| Bundle doesn't fit the budget | that primary is skipped, later ones are tried | `skipped_bundles` |
| Policy authority unavailable | reject without context | `policy_unavailable` |
| Graph or index failure, Neo4j unavailable, Usage can't be persisted | reject without context | `storage_unavailable` or the failing error |

No per-channel timeout exists. A budget-empty recall still persists a Usage
and can receive a verdict. No successful recall has a null `usage_id`.

## 9. Diagnostics and budget

```json
{"diagnostics": {
  "pipeline": "derived-hybrid-v1",
  "now": 1725000000123, "T": 1725000000000, "policy_revision": 7,
  "channels_used": ["vector", "bm25", "session", "ppr"],
  "vector_reason": "available",
  "embedding_profile_id": "16d404a70ca92beccbe06fae1c1bc400d924223a09c498c7f01278b0f795405b",
  "candidate_count": 97, "skipped_bundles": 0,
  "ppr_used": true, "identity_mode": "exact_episode_id"
}}
```

These are bounded machine values, never source text. `vector_reason` is
`not_configured`, `not_requested`, `provider_unavailable`,
`provider_rejected`, `profile_mismatch`, `invalid_vector`, `input_too_large`
or `available`. Usage holds no copy of the diagnostics.

No latency number in this set is a production measurement. The recall
harness and its BM25 baseline are in `scripts/eval/`.

## 10. Usage and explicit outcomes (D62)

Each successful recall, including an empty one, creates a Usage and returns
its `usage_id`. What a Usage stores is docs/01 §3.1. `report_outcome` is the
only way credit reaches an Episode; its admission rules are docs/04 §6 and its
credit rule docs/04 §5.

## 11. Determinism

The same captured inputs produce the same ordered context: the solver uses a
sorted CSR, ties break by ID, and the iteration cap and tolerance are fixed
(docs/06 §7). `structure_revision` is not a replay token. An ANN index
rebuild, a new vector, a concurrent Hit or a different `now` can change a
result without changing it. Usage captures the ordered selected primaries,
their source Episodes and the policy revision. It doesn't keep the response or
the inputs needed to rerun ranking.
