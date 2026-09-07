# 05 — Recall

Recall is semantically read-only and LLM-free: it never creates memory or
reinforces accessibility merely by returning context. It does persist a durable
RecallReceipt control record before delivery (§10). Determinism is conditional
on the captured serving inputs and degradation state, not graph state alone
(§11). This is the normative future pipeline; current code supports originals
and fulltext only (docs/09). D43-D51 in [10-decision-log](10-decision-log.md)
supersede the affected D40/D42 contracts.

```text
  recall(query, session?, T = now, limit = 10, budget?)
    │
    ├─ ① pin         recall_id · T · now · R0 · policy_revision · config_version · active[*]
    │
    ├─ ② candidates  vector(64) ── BM25(64) ── session(32) ── identity(anchors + profile 16)
    │                all with visible(T), visible_gen and current policy eligibility
    │
    ├─ ③ seeds       channel RRF → hub damping → top 128 → normalize
    │
    ├─ ④ envelope    one Neo4j tx, ≤ 2,000 nodes / 20,000 links       ┐
    ├─ ⑤ PPR         TypeScript, CSR, α = 0.85                        ┘ docs/06
    │
    ├─ ⑥ fusion      RRF( vector, BM25, session, identity, PPR ) = relevance
    │
    ├─ ⑦ assembly    exact m(now), U · score · valid(T) · policy · occurrence/conflict bundles · budget
    │
    ├─ ⑧ consistency structural retry/torn (§7); policy barrier → retry/reject, never torn-policy
    │
    └─ ⑨ response    durable receipt → results[≤ limit] · companions · context_text · diagnostics
                     (auto mode) → asynchronous audit-only exposure Hits
```

## 1. Pin

The following are fixed within one request attempt; a consistency retry
recaptures serving revisions/selectors but preserves the request's T and now.

| Value | Meaning |
|---|---|
| `recall_id` | UUIDv7. Key for commit, Hits and logs |
| `T` | Snapshot time. Defaults to now. For questions about the past ("what was I doing in 2023") the caller supplies it |
| `now` | Server ms. Reference for forgetting (docs/04 §4) |
| `R0` | `structure_revision` at start |
| `active[*]` | The three stream selectors. A switch mid-request is caught by §7 |
| `policy_revision` | Current suppression policy, regardless of historical T; checked at delivery and commit |
| `config_version` | Ranking, prior/calibration, rendering, budget/tokenizer and receipt-retention versions |
| pinned data versions | The active generation's `fact_language_policy` with its prompt digest and `extractor_profile_id`, the `grouping_version`, the adjudication `judge_profile_id`, and the `embedding_profile_id` behind the vector channel. All are recorded on the receipt (D48–D51) |
| request options | Session, `limit`, validated `budget`, client mode and authenticated principal |
| query embedding | One call to the embedding service, on the caller's verbatim query inside the active profile's pinned template. On failure the vector channel is absent |

The query text is used **as the caller wrote it** (D48). BM25 runs on that
exact string and the vector channel embeds the same string wrapped only by the
active embedding profile's frozen query template. The daemon never translates
the query, never issues a second generated query and never fuses a translated
variant; the offline comparison behind that default is in
[research/retrieval-fusion](research/retrieval-fusion.md), which measured 20/20
versus 19/20 Hit@1 on a small development set and therefore settles nothing
beyond "no mandatory translation".

## 2. Candidate channels

| Channel | Source | Size | Ranking |
|---|---|---|---|
| `vector` | global `vec_episode_<indexhex>` plus active-generation `vec_fact_g<g>_<indexhex>` — `queryNodes`; active-generation `vec_rel_g<g>_<indexhex>` — `queryRelationships` | 64 nodes + 16 relationships (≤ 32 endpoints) | cosine DESC, id ASC |
| `bm25` | global Episode fulltext plus `fts_fact_g<active>` (cjk) | 64 | Lucene score DESC, id ASC |
| `session` | last 32 Episodes plus top 2 non-synthesis Facts per Episode through composite index seeks | ≤ 32 + 64 | Episodes: time_utc DESC, ingest_seq DESC; Facts: time_utc DESC, id ASC; merged by time, kind, id |
| `identity` | user and agent Entity anchors + profile cache (dreaming, top-16 Facts) | ≤ 18 | score DESC, Fact.id ASC |

Candidate total ≤ 96 + 64 + 96 + 18 = **274**. Every later stage is bounded
by this number plus the envelope (§6), except separately bounded assembly
companions and provenance. Policy/control Episodes and RecallReceipt records
are never semantic candidates.

- Active selectors choose generation-scoped indexes before top-k, so hidden
  generations cannot crowd the result. Channel queries also put
  **`visible(T)` in the WHERE clause**. HNSW over-fetches `k_fetch = 4 × k`
  for time and current-policy predicates; if fewer than k survive, we proceed
  with what we have. Here k is an internal channel bound, not an RPC field.
  Denied index entries can temporarily crowd bounded over-fetch until the
  reconciliation rebuild; they cannot be served or conduct.
- Relationship-vector hits map to both endpoints. Within the vector channel,
  each endpoint keeps the maximum score across its direct node hit and all
  incident relationship hits; endpoint IDs then rank by `score DESC, id ASC`.
- Candidate index procedures have `k_fetch ≤ 256` and a 50 ms transaction
  timeout per channel. Timeout drops that whole channel. The session channel
  performs 32 bounded index seeks and reads at most two Fact rows per Episode;
  synthesis Facts have no `primary_episode_id` and cannot inflate the scan.
- `valid(T)` is not applied here (docs/03 §7). Invalid Facts still conduct
  only if current policy permits them. Policy eligibility applies to nodes,
  links, resolved entities, raw Episode text for content-scope policies, and
  every supporting source before candidates or conduction. A denied source
  cannot support a visible derived result; do not merely drop that source from
  the authority list and keep the result.
- Entity candidates and returned anchors need an allowed MENTIONS witness
  that is itself visible at historical T. Require the generation/policy-pinned
  earliest_allowed_from threshold <= T, not merely the pre-policy
  visible_from_utc threshold (docs/06 §2). Rebuild that cache before the Entity
  serves; missing/stale/null proof excludes it without scanning witnesses or
  caching arbitrary T. An allowed future mention cannot rescue a denied past
  mention. Apply the same rule before Entity conduction and final assembly.
- The session channel is what catches "the thing I mentioned a moment ago".
  The identity channel is a weak bias that lets spreading start around the
  self regardless of the query.
- Global original Episodes and active-generation Facts share these same
  channels and caps whatever language they are stored in. There is no language
  quota, no per-language channel and no translated candidate set. A Fact whose
  source is missing a vector because its embedding entry is BLOCKED or
  resolved as skipped is simply absent from the vector channel; BM25, session
  and PPR still reach it (docs/01 §4).
- The lineage gate covers the source Episode itself, not only its Facts. An
  assistant Episode with unknown or incomplete lineage, and every output
  derived from it, is stored but ineligible for semantic candidates, for
  synthesis and for invalidation. A synthesis whose bounded support union is
  incomplete or truncated is ineligible on the same rule (docs/01 §3.3).

## 3. Seeds

Seed mass of candidate x (all channel RRF ranks start at **1**; a missing
item contributes zero, and an absent/empty channel has its weight removed):

```text
  raw(x)  = Σ_c  w_c / (k_rrf + rank_c(x))         k_rrf = 60
            w_vector = 0.35, w_bm25 = 0.35, w_session = 0.2, w_identity = 0.1
            weights of absent channels are redistributed proportionally to the rest
  deg_cap(x) = DegreeProbe(x) = min(deg_physical(x), 256)
  hub(x)  = 1 / log₂(2 + deg_cap(x))               capped-degree approximation
  affinity(x) = raw(x) · hub(x)
  S           = top 128 candidates by affinity DESC, id ASC
  s(x)        = affinity(x) / Σ_{y∈S} affinity(y)   for x ∈ S; Σ s = 1
```

DegreeProbe is the canonical ConductingArc cache probe in docs/06 §2:
seek the composite RANGE index (source_id, link_id), read at most the first
256 endpoint rows in link_id ASC order, then count only those bounded rows.
The cache has one row per distinct endpoint of each physical five-role link;
parallel links remain distinct. Below 256 the physical degree is exact under
complete, consistent coverage; 256 is saturated,
not an exact hub degree. No time, generation or policy filter precedes the
probe limit. Hidden generations and denied links can saturate it without
conducting, so this is explicitly an approximation to eligible-degree damping.
It intentionally gives all physical hubs the same damping. ConductingArc is
nonsemantic access metadata, never authority, candidate, conductor or output.
It is maintained atomically with physical links and rebuilt from the retained
graph with complete publication by generation (docs/01 §5). Capture each
candidate's ordered probe rows/count, coverage and saturation state; hidden
backfill can change them without a structure_revision change, so replay uses
the captured state.

At most 274 candidate probes inspect 274 × 256 = 70,144 ConductingArc rows in a
bounded seed transaction (50 ms). Timeout, missing/incomplete ConductingArc
coverage or unavailable composite index/ordered probe plan drops PPR entirely,
not individual seeds; channel-only fusion remains available. No native
adjacency fallback, filtered refill or second adjacency expansion is allowed.
Seed damping only counts these raw rows; it does not resolve physical links.
During envelope construction non-hubs resolve only captured rows using
per-role unique link-ID indexes and verify physical endpoints, role and
generation; stale rows cannot conduct.
Selection happens **before** normalization. The top 128 become the PPR
personalization vector `s`; the remaining candidates do not seed but stay in
the channel lists used in ⑥. If `S` is empty, PPR is absent.

Note that these seed weights are a different set from the fusion weights in §6
— seeding decides where spreading starts; fusion decides what is returned.

Why hub damping: anchors such as "me" or "the company" appear in every channel
and have huge degree; without damping, PPR mass disperses over their thousands
of neighbors and query specificity is lost.

## 4–5. Envelope and PPR

[06-envelope-ppr](06-envelope-ppr.md). Only the contract needed here:

- Input: at most 128 normalized policy-eligible seeds and their captured capped
  degree probes, T, active[*], pinned policy/config, Entity witness thresholds
  and envelope-cache state. Output: p over the
  envelope nodes (`Σ p = 1`),
  or absent.
- If the envelope transaction does not finish within **100 ms** (config), the
  PPR channel is **dropped entirely.** Running PPR on a partial envelope
  biases results silently and is not reproducible.
- PPR computation itself has no deadline — it is bounded in size and takes a
  few ms, and a deadline would make the same input produce different output.

## 6. Fusion and assembly

### relevance — RRF

```text
  relevance(x) = Σ_L  w_L / (60 + rank_L(x))
                 L ∈ { vector .25, bm25 .25, ppr .30, session .15, identity .05 }
                 weights of absent lists are redistributed proportionally to the rest
  ppr list     = envelope nodes by p DESC, id ASC (only the top 256 are ranked)
```

RRF ignores the scale of channel scores and uses ranks only, because cosine,
Lucene and PPR mass have no common unit. `rank_L` starts at 1. With normalized
nonnegative weights, `0 < relevance <= 1/61 ≈ 0.016393442623` for a present
item. All weights are assumptions to calibrate, not measured efficacy.

### Mass and score

For the candidate union (all lists ∪ envelope top 256; ≤ 274 + 256 = **530**
elements), first retain returnable Facts/Episodes and at most eight Entity
anchors. Communities and other non-result nodes are discarded **before**
source resolution. The source Episodes' cache `(s, t_last_hit)` is then read
in one Cypher batch for the remaining Facts/Episodes (≤ 530 × 16 materialized
source Episode IDs); Entity anchors use immutable `m0`. Exact `m(now)` follows
(docs/04 §3).

```text
  A_Episode = R(elapsed_days, S)
  A_Fact    = max_source R(elapsed_days, S · sigma_fact)   sigma_fact = 30
  m(x)      = m0(x) · A_x
  U_Episode = (nu · mu0 + Σ w · reward) / (nu + Σ w)       nu = 4, mu0 = 0
  U_Fact    = mean of U_Episode over its materialized source_episode_ids
  score(x)  = relevance(x) · max(m(x), ε)^γ · (1 + beta · U_x)
              γ = 0.5, ε = 0.02, beta = 0.25; 0 <= beta < 1
```

Episode-only accessibility intentionally refreshes sibling Facts sharing a
source. The max selects highest retention, not necessarily the most recent
source; there is no per-Fact selectivity or independent-source inference.
`U_Fact` is an explicitly coupled source-mean heuristic. Only confirmed
`recall_hit` adoption changes S and t_last_hit; outcome changes U only.
Exposure, re_mention and promotion are audit-only for accessibility (D45,
docs/04). At the defaults the utility multiplier is in [0.75, 1.25].

`m0 = confidence × prior(sub_kind) × prior(modality)` is immutable for a Fact.
Confidence measures source/support-faithfulness, not truth. Syntheses require
explicit modality judged from synthesis content, never an arbitrary default
or a product of uncalibrated marginal confidences. Prior changes create a new
derived generation; Hit replay never overwrites m0. Policy and validity are
hard filters, not confidence adjustments. Mass is not a hard final-score
threshold, but **does gate envelope membership through bounded truncation**;
a memory excluded there has no PPR opportunity (docs/06).

### Filters and ordering

1. **Kind**: results are Facts and Episodes. Entities and Communities are not
   results. Entity anchors alone are returned separately in `entities`
   (top 8); Communities do not appear in that block.
2. **valid(T)**: invalid Facts **and superseded Episode revisions** leave the
   results. If the Fact that invalidated one is in the results, the invalidated
   Fact is attached under `provenance.supersedes`.
3. **Policy**: recheck primary items, anchors, companions and all source and
   provenance text against current policy. Historical provenance exceptions
   bypass time, never suppression. A denied supporting source suppresses the
   derived result; denied supersedes text is replaced by a redacted indicator.
4. **Ordering**: `score DESC, relevance DESC, mass DESC, id ASC`, then the
   occurrence grouping, bounded companion completion and greedy budget below.
   `limit` counts included primary bundles only; output ranks are assigned
   **after** packing as contiguous 0-based positions.

### Result item

```json
{
  "id": "01920000-0000-7000-8000-000000000001",
  "kind": "Fact", "schema": "anamnesis.claim/1", "sub_kind": "preference",
  "epistemic": "extracted", "modality": "asserted", "confidence": 0.9,
  "content": "Mira prefers tea.",
  "content_language": "en",
  "time": {"utc": 1700000000000, "precision": "day"},
  "score": 0.01008, "relevance": 0.016, "mass": 0.36,
  "utility": 0.2, "rank": 0,
  "sources": ["01920000-0000-7000-8000-000000000002"],
  "provenance": {
    "derived_from": [{"id": "01920000-0000-7000-8000-000000000002", "kind": "Episode", "visible_at_T": true}],
    "supersedes": [], "contrasts": [],
    "conflict_included_count": 0, "conflict_total": 0,
    "conflict_truncated": false, "conflict_redacted": false, "warnings": [],
    "echo_state": "direct", "echo_depth": 0,
    "corroboration_root_count": 1, "echo_lineage_truncated": false,
    "operator_corrected": false
  },
  "channels": ["vector", "bm25", "session", "identity", "ppr"]
}
```

Here `score = .016 × sqrt(.36) × (1 + .25 × utility)` (rounded).
`modality` is required on all Facts, including syntheses, and absent on original
Episodes. `content_language` is required on every Fact and carries the stored
source language (`mul` for materially multilingual prose, `und` when the
evidence is nonlinguistic); the daemon never returns a translated rendering in
its place (D48). `provenance.derived_from` contains the exact bounded Episode
source snapshot, under the time-only exception (docs/03 §3).

The lineage fields are provenance labels only. `echo_state`, `echo_depth`,
`corroboration_root_count` and `echo_lineage_truncated` never enter
`relevance`, `mass`, `utility`, `score` or ordering, and a high root count
never elects a conflict winner (D49). `operator_corrected` is true when this
result came from an authenticated repair of an adjudicator mistake
(docs/03 §5); it marks provenance, not extra authority. Client-supplied
sources or ranks are never trusted at commit; the durable receipt is
authority.

### Occurrences and bounded conflict bundles (D46)

Every original and each occurrence's extracted assertion/provenance is retained
as a separate immutable Fact, even if semantically duplicate. DERIVED_FROM and
RELATES_TO preserve those relations; re_mention is optional audit, not a truth
or confidence boost. Assembly may group **only the bounded ranked candidate
set**, using a versioned exact key preserving subject, predicate, effective
time/scope and modality. Unresolved key fields disable grouping; this is not a
global semantic-merge guarantee. The highest ordered item is representative;
return `occurrence_ids` (representative first, then ID ASC, at most 16),
`occurrence_total` (within this candidate set) and `occurrences_truncated`.
The representative's sources, not an unbounded occurrence union, remain its
attribution set. Different reports, times or modalities are not duplicates.

For each prospective primary, complete its conflict bundle **before budget**:

- Probe the indexed **raw** CONTRASTS adjacency for this primary/generation
  in peer ID ASC order with **LIMIT 65**: inspect at most 64 distinct peers;
  the 65th row is only a `has_more` sentinel. The adjacency index is keyed by
  `(primary_id, generation, peer_id)` and stores one peer row per undirected
  pair; symmetric lookup rows are rebuildable from immutable CONTRASTS, not
  a cache per historical T. Use bounded source/support lookups to check
  visibility, validity and current policy on each inspected peer; select the
  first **4** eligible peers in ID ASC order. Peers need not be candidates.
  No refill or recursive peer expansion occurs. Across at most 530 primary
  candidates this is at most 33,920 peer inspections plus 530 sentinel rows,
  each with the existing 16-source/16-support caps. An unavailable adjacency
  index or failed check skips the bundle with `conflict_unavailable`, never
  an apparently conflict-free result.
- `provenance.contrasts` contains those selected peer IDs; their full result
  content and source provenance are in top-level `companions`. A companion has
  no primary rank and receives no independent feedback credit. Completion is
  one-hop, not recursive companion expansion. Total contrast companions are
  at most `4 × limit <= 256`, further bounded by packing and the RPC cap.
  With at most 64 primaries this is at most 320 full primary/companion items,
  2,560 supersedes entries (8 per full item) and 5,120 direct source references
  (16 per full item), before cross-item deduplication and the hard byte cap.
- `conflict_included_count` is the number of selected peers (0..4).
  `conflict_truncated=true` if more than four inspected peers are eligible
  **or** the raw 65th row exists; then `conflict_total=null`. Otherwise the
  raw probe exhausted all peers, and `conflict_total` is the exact eligible
  count (equal to conflict_included_count). It never counts policy-hidden
  peers. Internal receipt diagnostics separately record `conflict_inspected`
  (0..64) and `conflict_has_more`; these raw counts are not ordinary response
  metadata because they could reveal hidden-peer counts.
- `conflict_redacted=true` only when a policy-hidden peer was actually
  observed among the inspected 64. Expose no hidden ID/text/count. A false
  redacted flag does not prove no hidden peers exist when truncated. Mandatory
  rendered `provenance.warnings` entries have `{code, content}`: code
  `conflict_incomplete` when truncated, `conflict_withheld` when redacted,
  and `supersedes_withheld` when supersedes text is redacted. Content is
  versioned renderer text describing that limitation, never hidden IDs/text.
  These warnings are mandatory context, not diagnostics; unknown uninspected
  remainder is not agreement.
  A raw sentinel never produces an exact total or a hidden-peer claim.
- At most eight direct outgoing INVALIDATES targets supply `supersedes`
  content per primary/companion (docs/01); no recursive invalidation walk.
  Denied target text/IDs are replaced with `supersedes_redacted=true`.
  Source provenance is at most 16 Episode references per full item; no raw
  Episode text is mandatory merely to show a source reference.
- Every selected peer, source reference, supersedes entry and incomplete or
  redacted conflict warning is mandatory bundle content. No winner is inferred
  from confidence or recency. If the complete bundle does not fit, skip the
  primary. This is a bounded both-sides contract, not an unconditional guarantee.

D42's intra-Episode exception is only explicit same-speaker, same-time,
same-scope self-correction. Those predicates are now decided from the bounded
stored fields in docs/02 §5.3, and the two scope predicates there are
different. The intra-Episode rule uses `same_scope_l1b`: equal non-null
`correction_scope_text` with equal resolved subjects, predicate and
attribution, while the corrected value and time fields are expected to differ.
Grouping uses `same_scope_group`, which requires `scope_complete=true` and a
byte-identical RFC-8785 `scope`. Both also require equal non-null
`speaker_key` inside one `(origin_source, origin_actor)` namespace, plus the
same immutable Episode ID for L1b or the exact `time_key` across Episodes.
Assembly never substitutes one predicate for the other. The grouping key that
assembly may use is
`grouping_version = "anamnesis.duplicate-group/1"` over subject keys,
predicate key, time key, scope key and modality, and it remains a local
comparison inside this bounded candidate set, never a global identity claim
(D49). Unresolved competing assertions remain CONTRASTS; a later report does
not automatically defeat an earlier one. Evidence validation proves a literal
quote and nonempty UTF-8 span boundaries, not entailment or absence of
hallucination.

A repeated occurrence is still never corroboration: occurrence count, root
count and echo depth do not change confidence, ranking or conflict resolution,
and an assistant echo of delivered text adds no independent root.

### Exact output budget (D44)

`limit` is a nonnegative safe integer (default **10**, hard maximum **64**;
configuration may lower but not raise that maximum). Reject out-of-range
values; `k` is not an RPC alias. Optional request examples:

```json
{"query":"tea preference","limit":10,"budget":{"unit":"utf8_bytes","limit":4096}}
```

```json
{"query":"tea preference","limit":10,"budget":{"unit":"tokens","limit":1024,"tokenizer_id":"cl100k_base@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}
```

The second tokenizer ID is illustrative and must be rejected unless that exact
installed version/digest exists. Units are `utf8_bytes`, `unicode_scalars` or
`tokens`; budget limit is a nonnegative safe integer. Reject fractional,
negative, nonfinite/overflow limits, unknown units, unpaired Unicode surrogates,
and token budgets with missing/unknown/unpinned tokenizer IDs. `tokenizer_id`
is allowed only for `tokens`. Never estimate or silently fall back to bytes.

Rendering version 1 constructs `context_text` as LF-separated full-item
records, no trailing LF. Each record is RFC-8785 canonical JSON of the final
result item, with content in full. Walk final primaries in rank order: emit
the primary if its ID is not yet emitted, then its not-yet-emitted contrast
peers in ID order, using the primary record for any peer also in results and
otherwise the companion record. Source/contrast/supersedes fields and
warnings above are part of the records, not optional diagnostics. Serialize
embedded newlines as JSON escapes, not separator LFs. Emit each item ID once;
if an earlier companion becomes a later primary, remove it from the
companion-only array and re-render from the final structured set with its
actual rank, without duplicating content. This JSON-lines text is
the canonical human/model context renderer; structured `results` and
`companions` must render to exactly the same text. No hidden title or prose
prefix is added. `entities` is bounded metadata, not additional context text.

Greedily try each sorted primary bundle, construct the actual **deduplicated
prospective** context_text with prospective ranks, and count it exactly. Include
only if the whole text and full serialized RPC response fit their caps; skip
oversized bundles and consider later ones until `limit` primaries are included
or candidates end. Never truncate claims, sources, supersedes or contradiction
warnings to fit. Primary ranks and receipt attribution name only included
primaries, never pre-budget candidates. A token tokenizer may merge across
record boundaries: count the whole prospective string, not a sum of per-item
estimates. Deduplicated companions are separately bounded and are not additional
primaries. Budget 0 or limit 0 yields `results: []`, `companions: []`,
`context_text: ""`, `used_budget: 0`.

`used_budget` counts the exact final context_text including separator LFs:
UTF-8 encoded length, Unicode scalar count (not UTF-16 code units), or the
pinned tokenizer's token count. JSON transport escaping, diagnostics and other
transport fields are excluded from this count but included in the hard **1 MiB
RPC response** cap. The prospective full response includes `used_budget`,
effective budget, tokenizer/renderer metadata, recall_id, companions,
entities and diagnostics; its UTF-8 length includes duplicated context in
structured fields and transport escapes. Diagnostics have a hard encoded cap
of 16 KiB; use bounded numeric counters/enums, never unbounded logs. Reject
an oversized metadata-only response rather than silently omitting required
fields. Reserve fixed maximum space for not-yet-known timing/counter values,
then verify actual bytes before publication.

Before invoking a tokenizer, skip any prospective bundle whose complete
context's UTF-8 text alone exceeds the response cap. Tokenization receives
only this bounded text,
never payload files, unbounded source concatenations or the caller's nominal
budget limit as an allocation size; at most 530 bounded bundle trials are
permitted. Count the installed pinned tokenizer exactly, with no estimate
fallback. Without `budget`, use the configured server output cap
(`utf8_bytes`, default 65,536, configurable up to the 1 MiB hard cap); echo
that effective budget and rendering/tokenizer version.
For the raw string represented by JSON `"A\né🙂"` the counters are 8 UTF-8
bytes and 4 scalars; rendered JSON escaping is counted after rendering,
not before it.

### `epistemic` — producer and derivation distance

Every result names the process that wrote it. The value is a closed enum,
derived at assembly time from `schema` (docs/01 §1); nothing is stored.
It describes provenance/derivation distance, not truth, reliability or a
calibrated probability.

| `epistemic` | Producer | Schemas |
|---|---|---|
| `observed` | ingest — verbatim original, nothing inferred | `anamnesis.original-message/1`, `anamnesis.original-document/1`, `anamnesis.correction/1` |
| `extracted` | extraction — a model's reading of one or more Episodes | `anamnesis.claim/1`, `anamnesis.mapping/1` |
| `synthesized` | dreaming — a model's combination of extracted Facts | `anamnesis.synthesis/1` |

The schema registry is open-ended and grows with the extractors; `epistemic`
is fixed at three values, so a caller can distinguish derivation processes
without tracking every schema. The three are ordered by distance from the
source: `observed` is what
was said, `extracted` is what a model claims was meant, `synthesized` is what a
model claims across several such claims. Each step down still bottoms out in
Episodes through `provenance.derived_from` and `sources` — the field is a
summary of that chain, not a substitute for it. Original text may be false;
a synthesis may be source-faithful. No reliability ordering follows solely
from this enum. Callers may use it for phrasing or provenance filtering.

## 7. Consistency — structure_revision

```text
  read R0 → ②③④⑤⑥⑦ (including companion completion) → read R1
    R0 == R1  → proceed
    R0 != R1  → retry once from ② (R0 := R1)
                  different again → proceed, diagnostics.torn = true
```

- torn means the candidates and the envelope may have seen different
  revisions. It does not mean the result is wrong; it means reproducibility is
  not guaranteed.
- Hits, cache writes and hidden-generation/non-selected-model embedding
  backfill do not bump the revision (docs/02 §1). Global Episode or ACTIVE
  derived coverage for the active profile does, because it changes vector
  candidates, and so does an embedding skip that releases ACTIVE coverage.
- An operator correction that creates ACTIVE structural output bumps the
  revision like any other structural write; an accepted adjudication proposal
  that has not been consumed yet changes nothing that is served.
- Mass and utility inputs are read **once**, in ⑦. If a commit lands
  mid-assembly, this response finishes with the captured values. Cache writes
  do not bump structure_revision, so the revision alone is not a replay token.

### Policy revision barrier (D43)

Suppression policy comes only from authenticated explicit `policy.set` and
`policy.revoke` RPCs (docs/02), never instructions executed from ingested text.
The immutable `anamnesis.memory-policy/1` Episode events rebuild active policy;
control Episodes stay outside memory search. Selector fields
`subject_entity_id`, `schema`, `sub_kind`, `modality`, `literal` combine with
AND and require at least one field. At most 256 active policies and at most
512 Unicode scalars per nonempty literal keep evaluation bounded (docs/01-02).
Literal matching is NFC Unicode, case-sensitive substring, never regex.
`scope: derived` checks canonical
claims and resolved entities before derived writes/Hits; `scope: content`
also checks raw Episode text and suppresses ordinary original recall.
Structured-only selectors cannot guarantee suppression of unextracted original
text; report that limitation rather than claiming erasure.

Recall pins policy_revision, checks it again under the serving barrier before
receipt persistence and response publication, and serializes that publication
against policy command acknowledgement. A changed revision retries the whole
recall once under current policy; another change returns `policy_changed` with
no context/feedback. Policy checks may never degrade to `torn=true`. Commit
revalidates current policy against receipt items and sources in its write
transaction; a changed revision during validation retries/rejects atomically,
never writes partial feedback. A now-denied receipt cannot authorize Hits.

Background reconciliation removes denied derived index/cache entries by
rebuilding; originals remain intact. Revoke does not fabricate Facts suppressed
at extraction: re-extraction is explicit. Historical T cannot bypass today's
policy. Backups and privileged raw operator access are outside ordinary
suppression; there is no `gc --erase` or GDPR erasure guarantee.

## 8. Degradation ladder

Nothing degrades into a silent partial result. Retrieval failures drop whole
channels; required bundle failures skip whole primary bundles; policy and
receipt failures reject rather than expose unverified context.

| Situation | Behavior | diagnostics |
|---|---|---|
| Embedding service failure (query embedding) | vector channel absent. Seeds from bm25, session, identity | `channels_used` lacks vector |
| Active profile has a BLOCKED embedding head | vector channel serves the prior contiguous prefix; entries past the hole have no vector and are reached through bm25, session and PPR only | the mandatory `embedding_profile_id` plus the complete `embedding_coverages` array below, whose blocked row or rows name their own `stream`, `generation`, cursors and omission digest |
| Source resolved as `RESOLVED_NO_VECTOR` | that source is permanently absent from this model's vector channel; other channels unaffected | 〃 |
| Fulltext error | bm25 absent | 〃 |
| Query exceeds the profile context, or the returned vector fails the profile's dimension or norm check | whole vector channel absent; never a truncated or renormalized query | `channel_reason: query_rejected` / `profile_mismatch` |
| Candidate channel tx > 50 ms | that channel is absent | `channel_reason: timeout` |
| Both vector and bm25 absent | seeds from session and identity only → PPR still runs | 〃 |
| Zero candidates | no envelope, empty result | `reason: no_candidates` |
| Seed probe tx > 50 ms / ConductingArc missing, incomplete or indexed access unavailable at any probe stage | PPR absent; channel-only fusion, never native adjacency fallback | `ppr_used: false, ppr_reason: seed_probe_timeout` / `degree_probe_unavailable` |
| Entity witness proof unavailable | exclude affected Entity at all stages; no witness scan | `entity_witness_unavailable` |
| Envelope tx > 100 ms | PPR list absent. Channel RRF only | `ppr_used: false, ppr_reason: envelope_timeout` |
| Envelope limit truncation | normal (truncation is defined behavior) | `envelope: {nodes, links, truncated_links}` |
| Structural revision mismatch twice | proceed with captured mixed structure | `torn: true` |
| Policy revision changes twice / policy state unavailable | reject without context | `policy_changed` / `policy_unavailable` |
| Required conflict completion unavailable | skip affected primary, continue packing | `conflict_unavailable` |
| Durable receipt write fails | reject without context | `receipt_unavailable` |
| Neo4j cold start | wait ≤ warmup_wait (20 s), then empty success | `reason: neo4j_unavailable` |
| Neo4j down | empty success | 〃 |

An unavailable-store empty success has `results: []`, `companions: []`,
`entities: []`, `context_text: ""`, `used_budget: 0`, `recall_id: null` and
explicit diagnostics; it conveys no item evidence and cannot be committed.
Successful available-store recalls, including budget-empty recalls, persist a
receipt for receipt-level outcomes. Contract, authentication, policy-barrier
and durability failures are explicit RPC errors, not silent partial success.
Commit and policy commands return retryable `storage_unavailable` when Neo4j
is unavailable. Distinguish `policy_unavailable` (required current-policy cache
missing) and `receipt_unavailable` (receipt persistence failed). No memory
content, including entities or provenance, is returned without both current
policy authority and a durable receipt. `recall_id` is the receipt key; there
is no separate public `receipt_id` field.

## 9. Diagnostics and budget

```json
{"diagnostics": {
  "recall_id": "…", "T": 1725000000000, "now": 1725000000123,
  "structure_revision": 4021, "policy_revision": 7,
  "config_version": "recall-v1", "torn": false,
  "channels_used": ["vector", "bm25", "session", "identity", "ppr"],
  "embedding_profile_id": "16d404a70ca92beccbe06fae1c1bc400d924223a09c498c7f01278b0f795405b",
  "embedding_coverages": [
    {"stream": "episode", "generation": 0, "health": "BLOCKED",
     "covered_ingest_seq": 84120, "required_ingest_seq": 84137, "lag": 17,
     "omission_digest": "3f2a9c61d0b74e58a1c9f0e2b6d4837c5ae10f92bb73c8d4e6015a7f92c3b8d0"},
    {"stream": "extraction", "generation": 42, "health": "BLOCKED",
     "covered_ingest_seq": 83904, "required_ingest_seq": 84102, "lag": 198,
     "omission_digest": "c7d1084b6e2f5a390bd47c1e8f6205a3d9b0e74126cf83a5d0e91b7c4632af18"}
  ],
  "ppr_used": true, "seeds": 97,
  "degree_probe": {"cap": 256, "seed_damping": "capped_physical", "state_captured": true},
  "envelope": {"nodes": 1412, "links": 9930, "hops": 2, "truncated_links": 0, "hubs_expanded": 2},
  "ppr": {"iterations": 23, "iterate_delta_l1": 8.1e-5, "fixed_point_error_bound_l1": 0.000459},
  "timings_ms": {"embed": 11, "candidates": 9, "envelope": 31, "ppr": 3, "assemble": 6, "receipt": 2, "total": 64}
}}
```

`embedding_profile_id` and `embedding_coverages` are bounded machine values,
never source text. Whenever recall selects the vector channel, both are
recorded in diagnostics and on the durable receipt exactly as served. The
array carries one row per applicable source partition of the active profile:
always `(episode, 0)`, plus `(extraction, active[extraction])` when an active
extraction generation exists, so it holds one or two rows, distinct and keyed
by `(stream, generation)`, sorted `episode` before `extraction` and then by
generation.

```text
embedding_coverages[1..2] = [{
  stream: episode | extraction,
  generation,
  health: HEALTHY | BLOCKED,
  covered_ingest_seq,
  required_ingest_seq,
  lag,
  omission_digest
}]
```

`health`, `covered_ingest_seq` and `omission_digest` come from that
partition's stored `EmbeddingCoverage` row (docs/01 §4).
`required_ingest_seq` is the same-attempt current `Meta.ingest_seq` for the
Episode row and the active extraction generation's current
`Generation.covered_ingest_seq` for the extraction row; `lag` is the exact
nonnegative difference `required_ingest_seq - covered_ingest_seq`. Generation,
both cursors and lag are nonnegative safe integers, and `omission_digest` is
64 lowercase SHA-256 hex over that row's resolved-no-vector set, so a replay
can prove which prefix each stream served without exposing what was omitted.
Every applicable row is persisted, including healthy rows and two
simultaneously blocked rows with different cursors and omission digests; an
anonymous scalar, or a choice between the Episode and extraction row, is
invalid. A missing applicable coverage row prevents vector-channel selection
instead of producing a partial array. The receipt also retains the
`EmbeddingResolution` and `EmbeddingQualification` identities applicable to
any source resolved as `RESOLVED_NO_VECTOR` in the delivered set.

Internal receipt/bench diagnostics record ConductingArc coverage state,
seed/envelope ordered probe rows/counts, stale-row exclusions,
saturated-source counts and bounded link/HubArc pool reads, plus witness-cache
versions and unavailable state. Raw physical counts/IDs can reveal hidden
generations or denied links: keep them out of ordinary response metadata;
the response reports only the cap, approximation mode and capture status.
Per attempt, seed inspections are ≤ 70,144, envelope probe inspections are
≤ 708,608, and envelope probe-plus-pool reads are ≤ 1,417,216: total structural
input ≤ 1,487,360 before separately bounded authority lookups (docs/06 §1).
Consistency retries repeat these per-attempt budgets, not an unbounded refill.

Target: **p50 < 100 ms, p95 < 250 ms** (including embedding, 1M-element
graph, local M-series); these are targets, not benchmark results. Neo4j work
has wall-clock guards: candidate
channel and seed-probe timeouts of 50 ms and an envelope timeout of 100 ms;
candidate timeouts drop that channel, seed/envelope timeouts drop PPR.
The bounded TypeScript PPR loop has no deadline.

## 10. Durable impressions and feedback (D45)

Before context delivery, persist a separate append-only **RecallReceipt**
control record, keyed by recall_id and bound to the authenticated client. It is
not a semantic Episode, candidate, embedding input or source of ordinary
recall. Record the actual included primary IDs and 0-based ranks, companion
IDs, immutable source snapshots, bounded derived snapshots needed after
generation GC, selected/absent channels and reasons, candidate/ranking state,
policy/config/generation versions, effective budget, used_budget, renderer and
tokenizer versions, and SHA-256 result/context digests. It also stores an
immutable
`selection_digest = sha256(RFC-8785(ordered array of at most 64 delivered
{element_id, root_episode_ids, echo_depth, complete} records))`. That stored
value is what a later assistant Episode's `EchoLineage` copies into
`context_digests`, so lineage never depends on recomputing a selection after
the feedback window closes (docs/01 §3.3). Original text need not be
duplicated: its immutable Episode IDs suffice. A write failure prevents
delivery with a committable recall_id. A failed socket send is delivery-unknown, never adoption;
record transport status separately and never manufacture a negative label.

Receipts survive restart and remain immutable within a configured explicit
`receipt_ttl_ms` feedback window (default 3,600,000 ms). `expires_at` is fixed
as `created_at + receipt_ttl_ms` at creation and exposed to the client;
`now >= expires_at` rejects commit, including retries, as `receipt_expired`. Unknown
receipts reject as `unknown_recall`. Expiry permits retention cleanup, not
silent acceptance. Retained Episode Hit outcome events store exact ranks and
attribution sufficient to rebuild utility even after receipt expiry.

Auto mode appends audit-only `exposure` for the top **included** three primary
results after response delivery, through the same policy-checked commit path.
Failures are reported operationally; they do not change the delivered response.
Exposure does not change S or t_last_hit. Receipt mode accepts authenticated
`commit {recall_id, adopted?, reward?}`; auto-mode explicit commit is rejected.
Reward must be finite and in [-1,1], not clamped. Missing reward differs from
zero: zero is an observed outcome and enters the utility denominator.

At least one of adopted or reward must be present; otherwise `empty_commit`.
Adoption IDs must be distinct included primary IDs, not companions or discarded
items; duplicate IDs and malformed/null fields reject before attribution.
Adoption is exactly once per recall/source Episode; outcome exactly once per
recall. Identical retries are no-ops; conflicting reward or attribution on a
repeated outcome is rejected, never partly applied. The outcome-bearing
request's `adopted` field is authoritative: present uses that set, absent uses
the delivered primary set, not an earlier adoption-only call's set. An
explicitly empty adopted set stays empty. Outcome finalizes its attribution;
later adoption cannot rewrite it. Invalid IDs, policy denial or conflicting retries reject
the whole commit before any Hit/cache write.

For selected set J, preserve **original delivered ranks**, not subset ranks:

```text
  a_j = (1 / (rank_j + 1)) / Σ_{l in J} (1 / (rank_l + 1))
  b_je = 1 / |sources(j)|                   e in sources(j)
  w_e = Σ_{j in J, e in sources(j)} a_j · b_je
  Σ_e w_e = 1                              if J is nonempty
  U_e = (4 · 0 + Σ_outcomes w_e · reward) / (4 + Σ_outcomes w_e)
```

Merge shared sources without an extra cap that loses credit. Empty J produces
no item-attributed outcome Hits but retains the receipt-level outcome.
For ranks 0 and 1, a = (2/3, 1/3). If their sources are {E1,E2} and {E2},
w = {E1: 1/3, E2: 2/3}, summing to 1. Reward -1 gives U_E1=-1/13 and
U_E2=-1/7 from the default prior. This is one whole-recall utility proxy,
not multiple independent truth labels or causal credit assignments.

Outcome-only never changes accessibility or mass. Adoption plus a negative
outcome has the same accessibility as adoption-only, with utility determined
by the weighted mean; compare those cases, not adoption+negative against no
event. Non-positive reward need not lower U if U was already more negative.
Only recall_hit uses the capped positive, dimensionless stability formula in
docs/04, so monotonicity is weak at S_max.

## 11. Determinism

The same **captured serving inputs** produce the same ordered context within
the pinned numeric/runtime contract (D47, docs/06 §7). Capture query and exact
embedding/model; the `embedding_profile_id` and the complete
`embedding_coverages` array actually served, each row carrying its
`stream`, `generation`, `health`, `covered_ingest_seq`,
`required_ingest_seq`, `lag` and `omission_digest`, plus the
`EmbeddingResolution` and `EmbeddingQualification` identities applicable to
the delivered set; the `fact_language_policy`, `grouping_version` and
`judge_profile_id` of the active generation, plus any correction record
applied to a delivered item; T/now/session/options; ordered candidate IDs,
scores and channel availability; selected index/coverage state; selectors and
structural reads; ConductingArc completeness and stage/source ordered degree-probe
rows/counts, stale-row exclusions and saturation decisions;
earliest_allowed_from thresholds with generation/policy versions; mass/utility
snapshots; m_cache and HubArc/profile snapshots actually used; envelope
nodes/arcs/seeds; conflict/occurrence completion; policy and
config/prior versions; renderer/tokenizer; and timeout/degradation decisions.
ANN index rebuilds, hidden-generation adjacency backfill, cache maintenance,
concurrent Hits and wall-clock channel drops can change results without
changing structure_revision. A revision is therefore **not** a full replay token.

Ordering follows docs/06 §7, including intentional ID DESC envelope truncation
and ID ASC result ties. PPR has a fixed iteration cap and iterate-delta stop,
not a deadline. Exact replay injects captured now and inputs rather than
rerunning an approximate index or timeout race. Receipt replay reconstructs
what was delivered within bounded retained snapshots; a full online-pipeline
reproduction additionally requires the pinned candidate/index/cache capture.
Diagnostics distinguish these replay levels and structural torn results.
Neither historical replay nor stored receipts permit current-policy bypass
on ordinary serving. Privileged offline audit is separately controlled.
