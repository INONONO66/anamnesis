# 01 — Storage

All graph data lives in one Neo4j (Community 5.26+, Docker, localhost-only).
Logically there are three memory layers plus a retained control ledger; each
permits a different kind of write.

```text
  originals  CREATE-only. No update, no delete. Loss here is irreversible
             Episode (including policy events) · Payload (metadata) · Hit
             originals-layer links: HAS_PAYLOAD · HIT_OF · Episode→Episode INVALIDATES (revision)
  derived    CREATE-only nodes/links inside a lifecycle-managed generation. Regenerable from
             originals plus the retained content-free InvalidationEvidence ledger and each
             rebuild's explicit old/new target mappings; originals alone do not suffice (§4)
             Fact · Entity · Community · derived links · embedding
  caches     SET allowed. Can be dropped and regenerated at any time
             hit/utility cache · active policy · EntityWitness · session topology · ConductingArc · ConductingArcCoverage · HubArc · ProfileCache · m_cache · Outbox · OriginHead · selectors
  control    append-only RecallReceipt impressions and feedback acceptance records;
             durable until explicit receipt retention expiry, not semantic Episodes
             append-only InvalidationEvidence metadata, retained while its
             invalidation outcome can affect a supported historical snapshot
```

Two things live on the filesystem outside Neo4j (§6): **payload bytes**
(`objects/`, durable — part of the data authority together with the Neo4j
database) and the **spool** (`spool/`, a transient queue that is empty
whenever Neo4j is healthy). Authority = Neo4j + `objects/`, nothing else
(§9).

## 1. Element

Every memory element carries the common `:Element` label plus exactly one
kind label. Global queries (fulltext, time cut, audit) go through `:Element`;
kind-specific queries go through the kind label.

```text
(:Element:Episode)    original. A user message or a document revision. Has an event time
(:Element:Fact)       derived statement. Time = "when the claim took effect"
(:Element:Entity)     anchor for a person, thing or concept. No event time; cached visibility threshold
(:Element:Community)  topic-set summary. No event time; generation-built visibility threshold
```

### Common properties

| Property | Kinds | Meaning |
|---|---|---|
| `id` | all | UUIDv7. Never a primary ranking signal; only a final deterministic tie-break. Truncation uses `id DESC` after semantic keys to avoid an old-first bias; result order uses `id ASC` |
| `schema` | all | `anamnesis.<kind>/<n>`. The only notion of "type" |
| `content` | all | UTF-8 normalized natural language; Episode ≤ 64 KiB, derived element ≤ 8 KiB |
| `m0` | all | Intrinsic mass in [0, 1]. Assigned once at creation, immutable (docs/04 §1) |
| `properties` | all | Schema-specific canonical JSON, encoded size ≤ 32 KiB |
| `time_value`, `time_utc`, `time_precision` | Episode, Fact | Event time (docs/03 §1) |
| `origin_source/session/actor/record` | Episode | Source identification |
| `session_key` | Episode | `sha256(origin_source, origin_session)`; namespaces session order across adapters |
| `origin_key` | Episode | **Logical** source identity (`sha256(source, session, actor, record)`). Indexed, **not unique** — every revision of the same document shares it |
| `source_revision` | Episode | Opaque adapter-issued token, stable across retries and unique for each revision of one `origin_key` |
| `revision_key` | Episode | `sha256(origin_key, source_revision)`. **Unique.** Identity of one revision occurrence, including A→B→A reverts |
| `previous_revision_key` | Episode | Explicit predecessor for a revision; null only for the first occurrence |
| `ingest_seq` | Episode | Globally monotonic integer allocated in the remember transaction, last of the statements that do not depend on it; unique build/catch-up cursor. Gapless: an aborted remember consumes no number |
| `ingested_at` | Episode | Server ms, written once at CREATE. **Not used in snapshot computation** — audit and spool-drain ordering only (docs/03 §1) |
| `payload_hash` | Episode | Payload reference (optional) |
| `digest` | Episode | SHA-256 of canonical `{schema, content, properties, time, payload_hash, previous_revision_key}` for integrity and retry conflict detection |
| `generation` | Fact, Entity, Community, derived links | Immutable owning generation (§4) |
| `idem_key` | Fact | SHA-256 of the full canonical Fact identity below. Unique |
| `entity_key` | Entity | `sha256(generation, normalized_name, entity_kind)`. Unique; create-new Entity retries are no-ops |
| `visible_from_utc` | Entity, Community | Temporal visibility threshold, one property comparison. Entity is a cache; Community is fixed at build. Policy-independent: an Entity additionally needs a current-policy witness from the `EntityWitness` cache (docs/03 §3) |
| `source_extraction_generation`, `source_covered_ingest_seq` | Community | Extraction snapshot used to build this Community generation |
| `source_structure_revision`, `source_export_digest` | Community | Exact serving-view revision and ordered ID/arc/threshold export hash |
| `source_episode_ids` | Fact | Sorted immutable list of 1–16 original Episode IDs used for mass and Hit attribution |
| `primary_episode_id` | non-synthesis Fact | Episode whose extraction/correction created this Fact; indexed for bounded session recall |
| `source_count_total`, `sources_truncated` | Fact | Audit fields recording the authority-set size before its deterministic cap |
| `max_source_ingest_seq` | Fact | Maximum ingest sequence in `source_episode_ids`; Community snapshot filter |
| `support_fact_ids` | synthesis Fact | 1–16 non-synthesis Facts whose validity the synthesis depends on |
| `sub_kind` | Fact | `fact / state / event / preference / procedure / decision / summary`. Input to the forgetting prior (docs/04 §1) |
| `modality` | every Fact, including synthesis | `asserted / reported / hedged / intended / hypothetical` — the speech act of this content (docs/02 §5.1). Required, meaning-bearing: part of identity, input to the forgetting prior, returned on recall |
| `confidence` | Fact | Judge's belief in [0,1] that the claim is what the source says. Stored, input to `m₀`, **not** part of identity — model nondeterminism on a scalar must not fork Facts |
| `prior_version`, `calibration_version`, `judge_version` | Fact | Immutable versions used to assign confidence and `m0`; generation configuration pins them. Raw validated judge output is retained in bounded `properties.audit` |
| `span` | primary `DERIVED_FROM` link (Fact → Episode) | `[start, end)` UTF-8 byte offsets into the Episode's content the claim rests on; validated at write, optional when the claim has no single locus (docs/02 §5 W2) |

### Schema registry

| schema | Label | Content |
|---|---|---|
| `anamnesis.original-message/1` | Episode | Conversation message |
| `anamnesis.original-document/1` | Episode | Document or file. One Episode per revision |
| `anamnesis.correction/1` | Episode | An explicit correction uttered by the user (the original behind docs/03 §5) |
| `anamnesis.memory-policy/1` | Episode | Authenticated deny/revoke control event (§3.2). Never a semantic search or extraction input |
| `anamnesis.claim/1` | Fact | An extracted claim. Invalidation events are claims too — the only special thing about them is an outgoing INVALIDATES edge |
| `anamnesis.mapping/1` | Fact | An actor ↔ person mapping claim |
| `anamnesis.synthesis/1` | Fact | A higher-level fact combining several Facts while retaining bounded Episode authority |
| `anamnesis.entity/1` | Entity | Anchor |
| `anamnesis.community/1` | Community | Topic set. Owns members through HAS_MEMBER |

For a larger document, `objects/` keeps the complete bytes while the adapter
stores a deterministic UTF-8 excerpt within the Episode content cap and sets
`properties.content_truncated=true`. Local extraction may stream bounded
decoded sections in later jobs. A remote endpoint receives such sections only
under the separate `allow_remote_payload_egress` contract (docs/02 §10);
neither path enlarges one RPC/LLM request.

### Contract: every Fact has bounded Episode authority

Every Fact materializes a non-empty authority set of original Episodes:

```text
  authority candidates
    extracted Fact     = [input Episode]
    replacement Fact   = [correction Episode] (reserved)
                       + authority(replaced Fact)
    synthesis Fact     = ∪ authority(member Facts)

  selection = top 16 unique candidates, except replacement:
              reserve correction Episode + top 15 other unique candidates
  source_episode_ids = selection sorted by Episode.time_utc DESC, Episode.id ASC
```

The same transaction creates one direct `DERIVED_FROM` link from the Fact to
each selected Episode. Additional Fact→Fact `DERIVED_FROM` links preserve
semantic provenance but are never traversed for mass or Hit attribution.
`source_count_total` records the pre-cap cardinality and
`sources_truncated=true` reports information loss explicitly.

`verify` reports `orphan-fact` unless the list contains 1–16 existing Episode
IDs and exactly matches the direct Fact→Episode links. This makes every
authority lookup total and bounded.

The correction Episode is never eligible for truncation in a replacement.
A synthesis additionally stores 1–16 `support_fact_ids`, chosen from
non-synthesis members by `m0 DESC, time_utc DESC, id ASC`. It is valid only while every
support Fact is valid; dreaming replaces an invalid synthesis rather than
nesting synthesis on synthesis.

Fact identity covers every immutable field that can change meaning:

```text
  idem_key = sha256(RFC-8785 canonical JSON of {
    generation, schema, content, properties, time, sub_kind, modality, primary_episode_id,
    max_source_ingest_seq,
    sorted(entity_ids), sorted(source_episode_ids), sorted(support_fact_ids)
  })
```

Here `properties` means the canonical meaning-bearing properties only (the
stored properties object with the reserved `audit` member omitted); raw
judge output, `confidence`, audit versions, and byte spans are retained but
excluded from identity. Otherwise nesting confidence inside raw output would
silently defeat its exclusion. A generation pins prior/calibration/judge
versions; changing `m0` priors requires a new derived generation, never Hit
replay or a SET of `m0`. Synthesis confidence measures faithfulness to its
support bundle, not world truth or a product of uncalibrated source scores;
its modality is judged from its own content (D42, D45, D47).

### Occurrence provenance (D46)

Semantic duplication never suppresses an occurrence's extracted assertion.
Each source occurrence receives its own immutable Fact, direct Episode
authority, modality, time and optional span, even when another Fact says the
same thing. Link it to that Fact with existing `RELATES_TO` or semantic
`DERIVED_FROM`; no new merge relation or automatic confidence/truth boost.
Exact retries within one occurrence still collide on `idem_key`. Assembly
may group duplicates with a representative and explicit occurrence IDs and
truncation metadata, but must preserve subject/predicate/time/modality;
storage does not promise a global semantic equivalence key (docs/05, D46).

## 2. Payload — outside Neo4j

Original bytes do not go into the graph database. To avoid property-store
bloat, page-cache pollution and dump growth, they are stored as
content-addressed files.

```text
~/.anamnesis/objects/<sha256[0:2]>/<sha256>        bytes (write-once, fsync)
(:Payload {hash, size, media_type})                 metadata node. No bytes
(:Element:Episode {payload_hash}) -[:HAS_PAYLOAD]-> (:Payload)
```

- The daemon's bounded `object.begin/chunk/commit` RPC writes the file first
  (temp name → fsync → rename → fsync directory); `remember` later supplies
  only its hash. Every hash used in a path must match `^[0-9a-f]{64}$`.
  A file without a node is a gc candidate (with the spool
  exception in §9); a node without a file is reported by `verify` as
  `missing-payload`.
- Because the file always exists before any transaction that references it,
  a backup can copy the exact hash manifest after taking the offline Neo4j
  dump (§9).

## 3. Hit ledger — attached to Episodes

```text
(:Hit {id, t, kind, kappa_eff, namespace, idem_key}) -[:HIT_OF]-> (:Element:Episode)
```

| Property | Meaning |
|---|---|
| `id` | UUIDv7 (server-issued) |
| `t` | Server time, ms epoch. **Not an event time** — forgetting runs on the now axis (docs/04 §4) |
| `kind` | `recall_hit / re_mention / promotion / exposure / outcome` |
| `kappa_eff` | Nonnegative adoption reinforcement coefficient (`recall_hit` only); zero for audit-only kinds. Outcome attribution uses `weight` and `reward`, not signed reinforcement |
| `weight`, `reward` | Outcome only: conserved Episode credit `weight >= 0` and finite signed `reward` in [-1,1], with receipt rank/source attribution retained |
| `attribution` | Bounded immutable contributing result IDs, original zero-based ranks and source shares; enough to audit the merged Episode weight |
| `namespace` | Recall UUID or `extract:…` / `dream:…` producer namespace (audit) |
| `idem_key` | `sha256(namespace, episode_id, kind)`. Unique — a retry is a no-op |

Hits **never point at Facts or Communities.** When a derived element is
adopted, the hit is resolved to its source Episodes (docs/04 §5). Reason: a
generation switch replaces derived IDs wholesale; an immutable ledger pointing
at derived IDs would reset forgetting state on every switch
([10-decision-log](10-decision-log.md) D1). Every Hit, whatever its producer,
is written through the single commit path (docs/04 §6).

Only confirmed `recall_hit` updates Episode `s` and `t_last_hit`.
`re_mention`, `promotion`, and `exposure` are audit-only; `outcome` updates a
separate rebuildable utility cache, never mass, stability or the accessibility
clock. `hit_count` counts all ledger entries for replay agreement, not adoption
count. Episode `S0` is initialized from its original immutable `m0` at ingestion.
Derived accessibility takes the maximum source retention, not the most recently
hit source. This deliberately refreshes sibling Facts sharing an Episode;
Episode-only attribution cannot claim per-Fact selectivity (D45).

### 3.1 Durable RecallReceipt control records (D45, D47)

`RecallReceipt` is not an `Element`, Episode, searchable memory, or graph
conductor. An append-only impression is durably written before publishing a
normal recall response, including auto mode. The sole unavailable-store
exception is a diagnostic-only empty response with `recall_id:null`, no
committable receipt and no memory content (docs/02 §9). It records `recall_id`, authenticated
client binding, creation/expiry times, delivered primary and companion IDs,
actual zero-based primary ranks, immutable source Episode snapshots, bounded
derived result snapshots, policy/config/generation versions, captured channel
and ranking state, budget and exact result/context digests (docs/05). It does
not duplicate complete raw Episode text. A recorded impression means response
publication was attempted, not proof that the peer consumed socket bytes.

Append-only `RecallFeedback` acceptance records freeze adopted IDs and outcome
attribution; they never modify the impression. Adoption is exactly once per
`(recall_id, source_episode_id)`; outcome acceptance is exactly once per
`recall_id`, atomically with all its Episode outcome Hits. Identical retries
are no-ops; conflicting duplicate reward is rejected. Empty attribution still
retains a receipt-level outcome, not a Hit without an Episode. Expired or
unknown receipts reject feedback, never silently accept it. Receipt TTL is
an explicit positive `receipt_ttl_ms` server configuration (default 3,600,000
ms), recorded as `expires_at = created_at + receipt_ttl_ms` on each receipt
and exposed to the client. `now >= expires_at` rejects even retries; it is a
feedback window, not a negative label. Expiry permits retention cleanup of receipts/acceptance records only,
not their durable Hit events. Utility remains rebuildable from retained
outcome Hits after the receipt expires.

For the outcome-bearing request's adopted set, if present, otherwise the
delivered primary set (an earlier adoption-only call does not supply a missing
field):

```text
  a_j = (1 / (rank_j + 1)) / sum_l(1 / (rank_l + 1))   rank base = 0
  b_je = 1 / |sources(j)|                            e in sources(j)
  w_e = sum_j a_j * b_je                             sum_e w_e = 1 if nonempty
  U(e) = (nu * mu0 + sum_h weight_h * reward_h) / (nu + sum_h weight_h)
  nu = 4, mu0 = 0                                    illustrative defaults
```

No extra cap discards outcome credit. A supplied empty adopted set means no
item attribution; missing is distinct from empty, just as reward zero differs
from no reward. Fact utility is the mean source utility, an explicitly coupled
heuristic, not independent evidence. One whole-recall verdict is one utility
proxy, not many independent truth labels (docs/04).

### 3.2 Suppression policy authority (D43)

Only authenticated explicit `policy.set` / `policy.revoke` commands append
`anamnesis.memory-policy/1` Episodes. Ingested text is data: neither the
extractor nor dreaming may execute a policy instruction found in it. Each
event's `properties` has `policy_id`, `action: deny | revoke`, `selector`, and
`scope: derived | content`. The selector has at least one of
`subject_entity_id`, `schema`, `sub_kind`, `modality`, `literal`; supplied
fields are ANDed. `literal` is a nonempty NFC Unicode, case-sensitive
substring of at most 512 Unicode scalars after normalization, never regex.
There are at most 256 active denies. Exceeding either hard limit rejects the
set atomically before any Episode/cache/revision change; identical retries
remain no-ops and revoke remains allowed at capacity. The daemon assigns
control origin/revision identity,
ingest sequence and server event time; revoke copies the target deny's
selector/scope, so audit remains self-contained. A policy ID identifies one
immutable deny; repeating the same command is idempotent, changing its
selector/scope is a conflict. A revoked deny needs a new ID to be reissued.

The active-policy cache is rebuilt by folding events in `ingest_seq` order.
`Meta.policy_revision` advances atomically with each effective change and
cache publication. Entity-selector bindings must remain resolvable across
generation changes: retain the bound Entity's immutable normalized-name/kind
snapshot in policy event audit metadata and rebuild at most 256 resolved
aliases per policy per generation before cutover. Alias lookup inspects at
most 257 ordered entries (limit+1); overflow or ambiguity rejects a new set
atomically, or blocks an existing policy's generation cutover with current
serving policy intact. Never truncate aliases and silently weaken a deny.
Revocation remains available in either state. Matching also uses that snapshot
before a new Entity is written, so
suppression need not recreate a denied anchor. Ambiguous binding blocks cutover,
never silently disables a deny; the policy does not promise global entity
equivalence. Cache loss blocks ordinary serving until policy replay
completes. Policy/control Episodes retain audit metadata but are excluded
from semantic fulltext/vector/session search, extraction, and PPR; generation
and embedding sequencers advance over them as deterministic no-ops.

`derived` scope matches canonical claims and resolved entities before any
derived writes or Hits. `content` additionally matches original Episode
content and suppresses matching originals from ordinary recall. A supplied
structured selector field absent on the tested raw Episode evaluates to false, so the
AND selector is a no-match; never infer a missing subject, sub_kind or modality
from its prose. The RPC reports this limit rather than promising full
original-text suppression for a structured selector. An Episode has its own
schema, not the schema of a potential future claim. Content matching is over
stored capped Episode text only (at most 64 KiB), with no automatic payload
scanning and no arbitrary bytes from an unexamined payload or backup.

Suppression is a hard filter on extraction, remention, dreaming, candidate
retrieval, conduction, result assembly, and all companion/provenance text.
A denied Episode cannot support a visible derived result; a denied Fact cannot
support a visible synthesis. Do not remove one denied source from immutable
authority and serve the remainder. Derived summaries/profile caches affected
by a deny stay unavailable until rebuilt from allowed inputs; Entity anchors
need an allowed witness, not only a pre-policy visibility threshold. The
witness is the rebuildable `EntityWitness {generation, policy_revision,
entity_id, earliest_allowed_from}` cache row: `earliest_allowed_from` is the
minimum event time of the Entity's MENTIONS sources in that generation that
the named policy revision allows, or null when none is allowed. Rows are keyed
by generation and policy revision, never by a request `T`; a request compares
`earliest_allowed_from <= T` for its pinned pair. A missing or unavailable row
excludes the Entity rather than falling back to `visible_from_utc`. An
active-generation append that adds an allowed mention lowers the current
revision's row in the same transaction, exactly as it lowers
`visible_from_utc`. Rows for a superseded policy revision are dropped once no in-flight request pins them
(docs/03 §3).

Policy publication takes an immediate serving barrier (docs/02 §1), including
recall and feedback already in flight. Background reconciliation rebuilds
derived serving generations, indexes and caches without denied content; it
never deletes originals or their established invalidation outcomes.
Reconciliation is a serving-view rebuild, **not semantic re-extraction**:
copy retained allowed assertions with explicit old/new target mappings and
preserve validity metadata even when an invalidator is omitted (§4).
Revocation removes the serving deny, but does not fabricate
Facts skipped during suppression: re-extraction is explicit. Historical `T`
never bypasses current policy. Privileged raw operator access and existing
backups are outside suppression. There is no `gc --erase` or GDPR erasure
guarantee; suppression is not deletion.

## 4. Derived layer and generations

The derived layer is split into three streams.

```text
  extraction   Fact · Entity · MENTIONS · RELATES_TO · DERIVED_FROM · Fact→Fact INVALIDATES · CONTRASTS
               selector active[extraction] = integer generation
  community    Community · HAS_MEMBER
               selector active[community]  = integer generation
  embedding    embedding_<model_id> property + vector index
               selector active[embedding]  = model_id string. No generation — a property is present or absent
```

Why separate streams: when dreaming rebuilds communities there is no reason to
re-extract Facts, and swapping the embedding model does not change extraction
output. Switching one stream's selector never touches another stream.

### Originals-layer links have no generation

`HAS_PAYLOAD`, `HIT_OF` and `Episode → Episode INVALIDATES`
are created by remember and commit, never by a pipeline. They carry no
`generation`, are CREATE-only like their endpoints, and are always
generation-visible.

### Generation lifecycle and visibility

Every derived element and derived link belongs to one integer `generation`.
Individual nodes and links are immutable; a generation is appendable only in
the lifecycle states that say so.

```text
  BUILDING     backfill plus dual-tail; hidden from recall
  ACTIVE       selected for recall; appendable for newly committed Episodes
  CATCHING_UP  inactive rollback target being advanced; hidden
  INACTIVE     complete only through its recorded covered_ingest_seq
  RETIRED      no operational reference; eligible for gc after retention

  visible_gen(x, stream) = x has no generation
                        || x.generation = active[stream]
```

`(:Generation {stream, generation, state, next_ingest_seq,
covered_ingest_seq, source_extraction_generation?})` is unique by
`(stream,generation)`. A target sequencer permits only its
`next_ingest_seq` job to read candidates and commit; retry cannot be overtaken.
`(:EmbeddingCoverage {stream, generation, model_id, covered_ingest_seq})` is
unique by `(stream,generation,model_id)` and advances contiguously.
An in-flight model swap owns one
`(:EmbeddingBuild {model_id,state})` plus immutable
`(:EmbeddingBuildSource {model_id,stream,generation,high_watermark})` rows.

Generation partitioning is physical and Neo4j-realizable:

```text
  extraction node generation 43  → technical label :ExtractionG43
  community node generation 7    → technical label :CommunityG7
  Fact/Entity fulltext + vector  → indexes scoped to :ExtractionG43
  RELATES_TO vector              → generation-specific property
                                    embedding_g43_<model> on fixed RELATES_TO type
```

The label/property names are generated only from validated integer generation
IDs. Recall chooses the active label/index before top-k; hidden generations
cannot crowd active candidates. GC drops a RETIRED generation's indexes before
deleting its nodes/relationships.

Extraction-candidate fulltext indexes set
`fulltext.eventually_consistent=false`; sequence N+1 cannot run until N's
transaction and synchronous index update commit. Async embeddings are never an
extraction-judge candidate source.

### Atomic full-generation build

A replacement extraction generation is built completely while the previous
generation continues serving recall. `Meta.ingest_seq` is incremented in the
same transaction that creates each Episode, giving one immutable total order
independent of event-time backfill.

This extraction workflow is distinct from policy reconciliation: reconciliation
does not call the judge again to decide whether an existing assertion or its
invalidation was correct. Both workflows obey the validity-preservation
activation gate below; suppression is never a reason to discard a marker.

```text
  1. In one transaction, open target 43 as BUILDING, capture
     source_high_watermark = Meta.ingest_seq and initialize its sequencer;
     active remains 42
  2. Enqueue only Episodes in (covered_ingest_seq, source_high_watermark] in
     ingest_seq ASC order. Outbox has a unique constraint on
     (stage, target_generation, ingest_seq, model_key), so crash/resume is a no-op:
       · every rebuild Outbox entry carries target_generation = 43
       · candidate reads use already-built generation 43 plus original Episodes
       · extraction-stream derived endpoints and links are generation 43
       · DERIVED_FROM may additionally target original Episodes
  3. Because state and watermark were captured atomically, every later
     remember receives ingest_seq > source_high_watermark and exclusively
     enters through dual-tail: one extraction entry for ACTIVE and every
     BUILDING/CATCHING_UP extraction generation
  4. The target sequencer commits strictly in ingest_seq order. Three
     automatic failures pause that exact head entry as BLOCKED; later entries
     cannot pass it. Manual retry resumes the same sequence. Advance
     covered_ingest_seq only after the head commits or deterministically
     produces no output
  5. Acquire the write queue as a short cutover barrier. While no remember can
     allocate another ingest_seq, verify:
       covered_ingest_seq = Meta.ingest_seq, no target work is in flight,
       target_embedding_model = null,
       EmbeddingCoverage(target, selected_model).covered_ingest_seq
         = covered_ingest_seq,
       EmbeddingCoverage(episode, 0, selected_model).covered_ingest_seq
         = Meta.ingest_seq,
       all generation-scoped indexes ONLINE, authority and links valid,
       ConductingArc coverage COMPLETE for every retained physical partition,
       invalidation marker coverage and target mappings complete,
       source-validity/invalidation outcomes preserved at every supported T
  6. In that same transaction transition BUILDING→ACTIVE (or
     CATCHING_UP→ACTIVE), mark 42 INACTIVE,
     set active[extraction] := 43 and increment structure_revision once
```

BUILDING writes and index population are invisible and do **not** increment
`structure_revision`; active-generation appends do. The barrier closes the
tail race without holding a lock across an LLM call. Event-time backfills
receive a later `ingest_seq` and are dual-tailed like any other remember.

Because `generation` is part of every derived `idem_key`, re-creating "the same"
Fact or link in generation 43 does not collide with its generation-42
predecessor — the two coexist and only the selected serving generation is
visible.

### Invalidation markers survive suppression and reconciliation (D43, D46)

Creating an INVALIDATES edge atomically appends content-free
`InvalidationEvidence {id, target_id, effective_time_utc, generation,
source_episode_ids}` to the non-serving control ledger. `generation=0` denotes
original Episode revision evidence; derived evidence records its owning
generation. This is an audit projection of the existing INVALIDATES decision,
not a new semantic relation, original deletion, or an inferred invalidation.
Before reconciling older data, backfill this metadata from retained edges and
authority under the generation barrier. Evidence stores no invalidator text
or invalidator Fact ID. Original target/source IDs are private audit metadata.

Rebuild `InvalidationMarker {target_id, generation, effective_time_utc,
evidence_id}` cache rows from that retained evidence and the build's explicit
old/new target mappings. For reconciliation, mapping means the same assertion
occurrence, time, modality, entity bindings and Episode authority in the new
generation, not a new semantic similarity judgment. Episode target IDs remain
unchanged. An ambiguous/missing mapping for a retained assertion blocks
activation. Evidence and mapping audit records needed for marker replay are
not receipt-TTL data and cannot be garbage-collected with a retired generation;
GC refuses to remove their last retained reconstruction authority.

Markers supply only an indexed existence predicate to validity. They are not
Elements, candidates, PPR conductors, embeddings, provenance companions or
ordinary RPC output: neither marker text nor IDs may be returned. The denied
invalidator's content can therefore leave serving indexes without reviving its
target. Cache loss fails closed until marker replay completes; policy revoke
does not remove established invalidation evidence.

Before any new generation activates, verify that every retained assertion's
source-live and invalidation predicates agree with the pinned predecessor for
every supported T, independent of policy visibility. These predicates are step
functions: compare canonical target mappings and sorted effective-time
boundaries (including the interval before the first boundary and equality at
each boundary), rather than sampling T. New explicit semantic corrections may
create new assertions under docs/03 §5; they cannot erase a retained target's
invalidation history. Missing authority, marker coverage, or an unprovable
equivalence rejects cutover and leaves the previous policy-filtered generation
serving. No response may treat unknown validity as valid.

### Cross-stream compatibility

Extraction links (`MENTIONS`, `RELATES_TO`, Fact→Fact `DERIVED_FROM`,
`INVALIDATES`, `CONTRASTS`) and all their derived endpoints share one
extraction generation. A `DERIVED_FROM` endpoint may instead be an original
Episode, which has no generation.

A Community generation records `source_extraction_generation`. Its
`HAS_MEMBER` links and Community endpoints use the community generation;
their Fact/Entity member endpoints must use the pinned extraction generation.
Therefore:

```text
  visible_community(c, g_c, g_e)
    = c.generation = g_c
   && Generation(community,g_c).source_extraction_generation = g_e

  visible_has_member(l, g_c, g_e)
    = l.generation = g_c
   && l.from.generation = g_c
   && l.to.generation = g_e
```

An extraction cutover atomically sets `active[community] = null`. Community
and profile channels remain absent until dreaming builds against the new
active extraction snapshot and switches its selector; old cross-stream
endpoints are never followed.

### Rollback and gc

Rollback is a catch-up operation, not a blind selector SET. In one transaction
an INACTIVE generation enters CATCHING_UP and captures
`rollback_high_watermark=Meta.ingest_seq`; history enqueue covers only
`(covered_ingest_seq,rollback_high_watermark]`, while later remembers
dual-tail above it under the same unique Outbox key. After extraction and
embedding coverage catch up, it crosses the same cutover barrier before
becoming ACTIVE. Serving continues on the current generation until then.

GC refuses every ACTIVE, BUILDING or CATCHING_UP generation and the
configured rollback target. Only RETIRED generations past retention are
deletable. Deleting hidden generation data does not change the serving view
and does not increment `structure_revision`.

### The embedding stream

Embeddings are per-model properties and indexes.

```text
  embedding_<model_id>          e.g. embedding_bge_m3_1024
  vector index vec_<model_id>   dimension and similarity depend on the model
  active[embedding] = <model_id>
```

Model swap is a backlog-plus-dual-tail build, not a blind requeue:

1. Acquire the same lifecycle/write-queue barrier and require no extraction
   generation is BUILDING or CATCHING_UP. In one transaction create the
   BUILDING EmbeddingBuild, set
   `target_embedding_model=<new>`, and capture immutable source rows for
   global Episodes at `Meta.ingest_seq` and every ACTIVE extraction generation
   at its `covered_ingest_seq`; keep the old model active.
2. Enqueue `<new>` jobs through every captured source high watermark, using
   the model-aware Outbox key.
3. Every later remember and every ACTIVE extraction commit enqueues embedding
   jobs for both `active_embedding_model` and `target_embedding_model`; target
   jobs are therefore an exclusive dual tail above the captured watermarks.
4. Under the write-queue barrier, require global Episode coverage through
   current `Meta.ingest_seq`, every ACTIVE extraction generation's model-scoped
   coverage through its current cursor, and all target-model indexes ONLINE.
   Then switch active, clear the target and increment `structure_revision` in
   one transaction.

Opening a rebuild/rollback extraction generation is refused while
`target_embedding_model` is set. Therefore the vector-bearing generation set
cannot change between the model build's atomic capture and activation.

The previous property and index are removed by
`gc --embedding <model_id>` under the same retention rule (previous model +
30 days). `RELATES_TO.content` is embedded under the same rule.

## 5. Link

Relationships are real Neo4j relationship types, not nodes. The seven roles
are not extended — variety in relationships is absorbed by the natural
language in `RELATES_TO.content`.

```jsonc
{
  "id": "0192f3b2-…",
  "from": "<element-id>", "to": "<element-id>",
  "role": "DERIVED_FROM",
  "content": "This claim was extracted from that message.",
  "span": [128, 191],                      // primary Fact→Episode link only; byte offsets the claim rests on
  "generation": 42,                        // absent on originals-layer links
  "idem_key": "…"
}
```

`content` is UTF-8 normalized text of at most 8 KiB, checked at write like
any derived element's content; a longer link content rejects the write, it's
never truncated. The cap bounds what `RELATES_TO` embedding and provenance
assembly must carry per link.

Links carry **no per-link weight.** PPR transition strength is a per-role
constant `w_role` in `config.jsonc`; each retained visible row normalizes its
role weights (docs/06 §4). This keeps the TypeScript and GDS transition
matrices identical ([10-decision-log](10-decision-log.md) D24).

Every INVALIDATES relationship copies `target_id` and the source's
`effective_time_utc`. Fact invalidation also carries `generation`. These
immutable fields support a relationship-index seek for `valid(T)` without
expanding all incoming invalidators. One Fact may create at most eight
outgoing INVALIDATES links.

For bounded conflict completion, each endpoint has a rebuildable indexed
`ConflictAdjacency {generation, fact_id, peer_id, link_id}` row per CONTRASTS
peer, maintained atomically with link creation. The peer ID is the deterministic
order key; reverse lookup is explicit rather than an unbounded expansion.
Read at most 65 raw rows in peer-ID order: inspect the first 64 with bounded
visibility, validity and policy checks; the 65th is only a has-more sentinel.
Return the first four eligible peers. There is no eligibility cache for every
possible T and no unbounded filtered scan or COUNT (docs/05 §6).
Unavailable adjacency/checks reject the bundle as `conflict_unavailable`.
`conflict_included_count` is the actual selected count. If more than four
inspected peers are eligible or a sentinel exists, `conflict_truncated=true`
and `conflict_total=null`; otherwise the exhausted scan supplies the exact
eligible total. An actually inspected policy-hidden peer sets
`conflict_redacted=true`, without its ID, text or hidden count. Uninspected
peers are unknown, not certified absent; incomplete warnings remain mandatory.

| Role | Direction | Layer | Meaning | Conducts PPR |
|---|---|---|---|---|
| `NEXT_EPISODE` | Episode → next Episode | cache | Rebuildable same-session total order. Rewired on backfill | yes |
| `MENTIONS` | Episode\|Fact → Entity | derived | What it is about | yes |
| `RELATES_TO` | Fact\|Entity ↔ Fact\|Entity | derived | Free natural-language relation | yes |
| `HAS_MEMBER` | Community → Entity\|Fact | derived (community) | Topic membership; captures `member_visible_from_utc` at build | yes |
| `DERIVED_FROM` | Fact → Episode\|Fact | derived | Provenance chain. Provenance is snapshot-exempt, **never policy-exempt** (docs/03 §3) | yes |
| `INVALIDATES` | Fact → Fact / Episode → Episode | derived / originals | The target is not valid from this event's time onward | no |
| `CONTRASTS` | Fact ↔ Fact | derived | An unresolved contradiction, preserved | no |

PPR uses the five conducting roles **bidirectionally**. The stored direction is
for the semantic model.

### ConductingArc — bounded physical access cache

The mandatory rebuildable cache is a node, not another semantic relationship:

```text
  (:ConductingArc {source_id, link_id, peer_id, role,
                   generation?, source_extraction_generation?})
  (:ConductingArcCoverage {stream, generation, state})
      stream = cache | extraction | community; cache generation = 0
      state = COMPLETE | UNAVAILABLE
```

For each retained physical NEXT_EPISODE, MENTIONS, RELATES_TO, HAS_MEMBER or
DERIVED_FROM relationship, create exactly one row per distinct endpoint:
source_id is that endpoint and peer_id is the other. Parallel relationships
have distinct link_id values and remain distinct rows, even for the same peer.
All link IDs are daemon-issued immutable UUIDs, distinct across roles; the
unique (source_id, link_id) key also rejects cross-role identity collisions.
The current protocol rejects self-links at write time. Defensive reconstruction
of an accepted legacy self-loop emits only one endpoint row, never two, and
verify reports the self-link contract violation; fixtures must not imply that
ordinary writes accept self-links. Nonconducting roles get no rows.

Copy role and generation from the relationship; generation is absent for
NEXT_EPISODE. HAS_MEMBER additionally copies its Community generation's
source_extraction_generation. Rows include hidden, inactive and retired
relationships until physical deletion, regardless of time or current policy.
They contain no semantic content and are neither authority, Element,
candidate, conductor, embedding input nor ordinary output. Physical links and
their endpoints remain authoritative; cache rows merely locate them.

DegreeProbe seeks source_id in the composite range index
(source_id, link_id), iterates link_id ASC and takes the first 256 rows
**before** any role, time, generation, visibility or policy filter, counting
only that captured list. This is a single ordered node-index access path
across all five roles, not a native adjacency expansion or degree COUNT.
Non-hubs resolve only the captured <256 rows through the appropriate role's
unique relationship id index. Verify physical existence, both endpoint IDs,
role and copied generation fields before use; stale rows are discarded with
no refill and counted in captured diagnostics. They still occupy the raw
probe slots. HubArc consumers perform the same bounded link-ID verification
for their at most 32 tuples. There is no second adjacency expansion (docs/06).

Every physical link CREATE/DELETE, NEXT_EPISODE topology rewire and generation
GC creates/deletes its endpoint rows in the **same transaction**, including
hidden-generation writes. Link deletion also removes matching HubArcs.
Generation opening publishes COMPLETE empty coverage atomically before its
first link append; subsequent appends preserve complete coverage atomically.
Generation cutover requires complete coverage, and GC removes a partition's
coverage record only after its last physical link and cache row are gone.

Maintenance/restore reconstructs ConductingArc solely from the retained
physical graph, not policy-filtered serving exports or HubArc shortlists.
Mark affected partitions UNAVAILABLE under the write-queue barrier before
clearing/rebuilding; a resumable paged rebuild may run offline from recall,
then catch up under that barrier and verify complete endpoint coverage before
atomically publishing COMPLETE. No partial generation is advertised complete.
Recall requires COMPLETE coverage for **all retained partitions**, including
hidden generations (they affect physical saturation), plus ONLINE indexes.
Maintain Meta.conducting_arc_ready as the aggregate completeness gate in the
same publication/lifecycle transactions. Recall reads this single pinned
Meta field, never enumerates an unbounded generation registry. Missing/false
means unavailable; startup/rebuild verifies all partitions before setting it
true, and any detected coverage loss clears it before another PPR attempt.
An empty source under complete coverage is degree zero; missing coverage or
unavailable rows/indexes instead drops the whole PPR channel with
degree_probe_unavailable. No native adjacency fallback is allowed. A cache
repair alone does not bump structure_revision; capture coverage state and
ordered rows for replay. A detected missing row invalidates coverage until
repaired, never certifies a smaller physical degree.

### Lattice — allowed (from, role, to)

```text
  Episode        --NEXT_EPISODE--> Episode
  Episode|Fact   --MENTIONS------> Entity
  Fact|Entity    --RELATES_TO----> Fact|Entity
  Community      --HAS_MEMBER----> Entity|Fact
  Fact           --DERIVED_FROM--> Episode|Fact
  Fact           --INVALIDATES---> Fact
  Episode        --INVALIDATES---> Episode           (revision, written by remember only)
  Fact           --CONTRASTS-----> Fact
```

Anything outside the lattice is a contract violation and is rejected at write
time.

### Idempotency key

```text
  derived links     idem_key = sha256(from, to, role, content, generation)
  originals links   idem_key = sha256(from, to, role)
  session topology  idem_key = sha256(session_key, predecessor, successor)
```

A per-role unique constraint. Re-running extraction within a generation never
creates the same link twice; a new generation legitimately creates its own
copy.

## 6. Filesystem

```text
~/.anamnesis/                 mode 0700
├── sock                      UDS, mode 0600 (docs/02 §10)
├── socket.token              32-byte UDS capability, mode 0600
├── daemon.lock/              atomic-mkdir lease + owner nonce/heartbeat
├── config.jsonc              calibration parameters and modes (docs/04 §9)
├── neo4j.auth                per-install random Neo4j password, mode 0600
├── objects/                  Payload bytes (§2). Part of the authority
├── spool/                    transient remember() queue while Neo4j is unavailable (docs/02 §4)
├── tmp/dream/                bounded disposable GDS exports
├── neo4j/                    container volume (data/, dumps/)
└── compose.yaml              container definition managed by the CLI
```

The parent directory also holds the fixed, root-hash-namespaced writer pointer
and backup/restore activation journals; they remain discoverable while the
data root is renamed (docs/02).

## 7. Indexes and constraints

```text
unique    Element.id · Episode.revision_key · Episode.ingest_seq · Fact.idem_key · Entity.entity_key · Payload.hash · Hit.idem_key
          · RecallReceipt.recall_id · RecallFeedback.id · RecallOutcome.recall_id
          · ActivePolicy.policy_id · ConflictAdjacency(generation, fact_id, peer_id)
          · EntityWitness(generation, policy_revision, entity_id)
          · InvalidationEvidence.id
          · InvalidationMarker(generation, target_id, evidence_id)
          <role>.idem_key (7) · <role>.id (7; indexed per-role link-ID resolution)
          · ConductingArc(source_id, link_id)
          · ConductingArcCoverage(stream, generation)
          · Outbox(stage, target_generation, ingest_seq, model_key)
          · EmbeddingCoverage(stream, generation, model_id)
          · EmbeddingBuild.model_id
          · EmbeddingBuildSource(model_id, stream, generation)
          · HubArc(hub_id, link_id) · OriginHead.origin_key · Meta.key (single node, key = 'meta')
range     Episode.origin_key · Episode.ingest_seq · Element.time_utc · Element.schema · Element.generation
          composite Episode(session_key, time_utc, ingest_seq)
          composite Fact(generation, primary_episode_id, time_utc, id)
          composite HubArc(hub_id, rank) · Outbox.processed_at
          composite ConductingArc(source_id, link_id)
          composite Fact-INVALIDATES(target_id, generation, effective_time_utc, id)
          composite Episode-INVALIDATES(target_id, effective_time_utc, id)
          composite InvalidationMarker(target_id, generation, effective_time_utc, evidence_id)
          composite ConflictAdjacency(generation, fact_id, peer_id)
          RecallReceipt.expires_at
fulltext  Episode.content (global) · Fact/Entity content (one index per generation)
vector    Episode.embedding_<model> (global) · :ExtractionG<N>.embedding_<model>
           · RELATES_TO.embedding_g<N>_<model>
           (one node/relationship index per generation and model)
```

The global Episode memory indexes use a semantic-only technical label that
excludes policy/control Episodes; an audit query may still use `:Element`.
Active policy filtering is required even before reconciliation finishes.
Outcome acceptance records carry both `RecallFeedback` and `RecallOutcome`
labels; the unique recall ID prevents duplicate outcome acceptance. Adoption
may arrive in multiple feedback records; unique Episode Hit keys enforce its
per-recall/source exact-once rule without forbidding later adoption batches.

Every ingest-derived Outbox sets every unique-key component:
`target_generation=0` for global Episode work and `model_key='-'` for
non-embedding stages. Re-queuing a different embedding model uses
`model_key=model_id`, so it cannot collide with an older model's completed
entry.

The ConductingArc uniqueness constraint's backing RANGE index may satisfy
the composite range requirement; do not create an equivalent duplicate index.
For source_id equality it must supply link_id ASC directly, with LIMIT 256
before collection/counting and any non-index filter/sort. Per-role id
uniqueness likewise supplies the relationship RANGE indexes used by bounded
link resolution. EXPLAIN/PROFILE must prove these access paths (docs/07).
No O(1) claim is made for a native degree COUNT. DegreeProbe counts only the
captured ConductingArc rows: <256 is exact under complete, consistent coverage;
256 saturates and uses only HubArc traversal. Seed damping uses that same
capped physical value, min(deg,256), never an unbounded count (docs/05–06).

## 8. Immutability discipline

Neo4j Community has no database triggers. The discipline is two-fold.

1. **The daemon is the only write path.** Bolt is bound to 127.0.0.1, the
   password is per-install random, and only the daemon holds a connection
   (docs/02 §10).
2. **An exhaustive list of every SET/DELETE in the code** — anything outside
   this list is rejected in review.

```text
SET allowed
  Episode.{s, t_last_hit, hit_count}           hit cache (docs/04)
  Episode.{utility_weight, utility_reward_sum} utility cache from outcome Hits only
  Element.m_cache                              mass snapshot for ordering, refreshed by maintenance (docs/06 §2)
  Entity.visible_from_utc                      min visible mention time, updated on active backfill
  EntityWitness.earliest_allowed_from          min allowed mention time for its (generation, policy_revision), same rule
  ConductingArcCoverage.state                  rebuild availability/publication barrier
  Element.embedding_<model>                    backfill; write-once null → vector
  RELATES_TO.embedding_g<N>_<model>             generation-partitioned relationship vector
  Outbox.{state, attempts, next_retry_at,
          error, processed_at}                 sequencer/blocked-head cursor
  OriginHead.revision_key                      CAS head cache, rebuilt from the immutable revision chain
  Generation.{state, next_ingest_seq,
               covered_ingest_seq}              build/cutover lifecycle
  EmbeddingCoverage.covered_ingest_seq           model-scoped contiguous embedding cursor
  EmbeddingBuild.state                           BUILDING→ACTIVE/INACTIVE lifecycle
  Meta.{structure_revision, policy_revision, ingest_seq,
         last_server_time, writer_epoch, active_*,
         target_embedding_model,
         conducting_arc_ready}                   serving selectors, model build, clock, fencing and cache gate
CREATE/DELETE allowed in caches
  session NEXT_EPISODE                         local rewire or full topology rebuild
  ConductingArc                                atomic physical link create/delete/rewire/GC; retained-graph rebuild
  ConductingArcCoverage                        per-partition completeness publication and lifecycle
  HubArc                                       maintenance rebuild
  ProfileCache                                 dreaming rebuild
  ActivePolicy                                 fold immutable policy Episode events
  EntityWitness                                per (generation, policy_revision) earliest allowed mention time
  ConflictAdjacency                            bounded conflict completion and policy-aware summaries
  InvalidationMarker                           replay retained content-free evidence/target mappings
DELETE allowed in retained control state
  expired RecallReceipt / RecallFeedback        explicit receipt TTL; never removes Hit outcomes
DELETE allowed in derived/authority-adjacent state
  gc --derived                                 derived output of retired generations (§4)
  gc --embedding                               retired embedding property + index (§4)
  gc --objects                                 unreferenced Payload files (§9)
```

Integrity is checked by `verify` (every Episode digest + Payload existence and
hash + bounded Fact authority/list↔link agreement + Hit ledger ↔ cache replay
agreement + utility sufficient-statistic replay + policy event/cache agreement
+ unexpired receipt/feedback exact-once constraints and captured attribution
+ invalidation evidence/marker replay and generation validity preservation
+ ConductingArc endpoint coverage, unique identities, role/generation/peer
agreement with every retained conducting link, exclusion of nonconducting
roles, ONLINE ordered access indexes and COMPLETE publication state).

ConductingArc verification is a maintenance scan of the retained graph, not
a recall-time full-degree scan. Missing/extra/stale rows or false completeness
mark the affected coverage UNAVAILABLE and are reported; rebuild before PPR
serves again. Rebuild and verify must include parallel links, both endpoints,
hidden generations, topology rewires and retired-generation GC (§5).

## 9. Durability, backup, restore

### Authority

The data authority is exactly two things: the Neo4j database and
`objects/`. The spool is a queue: after a successful drain its contents are
redundant with Neo4j, and its files are deleted once every line is marked done
and `verify` has confirmed the drained Episodes (default: 7 days after
completion). Nothing that is only in the spool is considered stored — remember
returns `spooled: true`, not `created: true`, so that callers know.

### Write ordering

| Step | Guarantee |
|---|---|
| objects write | fsync both temps; rename data first; rename metadata sidecar last as commit marker; fsync directory. A hash is committed iff its valid sidecar and matching data both exist |
| spool append | framed record `[u32be length][canonical JSON][32 raw SHA-256 bytes]` → `fsync` → **then** ack; `fsync(dir)` on journal creation |
| spool drain | Neo4j transaction commits → append the same framed/checksummed cursor to `.done` → `fsync`; a crash before the cursor replays and `revision_key` is a no-op |
| Neo4j | its own WAL. Every remember/commit/extract is one transaction, so there is no partially applied state |

Recovery scans each spool journal and cursor journal sequentially. Only an
incomplete final frame is truncated. Any checksum mismatch, including the
last complete frame, fails closed and quarantines the **whole journal**
because boundaries after corruption are not trusted. No cursor may advance
into or beyond that journal; valid-looking later records remain quarantined
until explicit `anamnesis spool repair` exports verified frames for operator
review and re-import. On ENOSPC or short append, truncate back to the prior
verified offset, fsync, and return failure before any later append or ack.
Cursor entries include `{offset, record_hash}` and may advance only to a
verified record boundary. Journal rename/removal is followed by
parent-directory fsync.

Object startup recovery counts every temp byte toward the global quota,
and deletes expired temps. The object-maintenance owner handles committed-path
states explicitly:

| State | Outcome |
|---|---|
| data, no sidecar | uncommitted orphan; retain for the one-hour floor, then delete only after node/spool recheck |
| sidecar, no data | move sidecar to `objects/quarantine/`; report `object_corrupt`; refuse that hash |
| sidecar + wrong size/hash data | move both to quarantine; report `object_corrupt`; refuse that hash |
| sidecar + matching full SHA-256 data | committed and reusable |

`object.begin` performs the full data SHA-256 recheck before returning
`present:true`; daily `verify --scope objects` does the same for every
committed pair. `object.commit` re-checks the marker under the object lease.
ENOSPC removes/truncates the current temp and returns `resource_exhausted`
before publication.

### gc --objects safety

A payload file is deleted only if **no** `(:Payload {hash})` node references it
**and** no undrained spool line references it. gc reads the spool's pending
lines before deciding. A file younger than 1 hour is never deleted (covers the
window between the objects write and its transaction).

All `objects/` mutations run through the daemon's write queue. `gc --objects`
takes an exclusive object lease against upload, spool append/drain, backup
and restore, then re-checks both predicates immediately before each unlink.
Restore operates only in an empty staging root while holding that root's
daemon lock. The age floor is defense in depth, not the synchronization
mechanism.

The object lease is cross-process: atomic mkdir at the fixed parent path
`~/.anamnesis-object.<root_hash>.lock/`, with owner `fs_epoch` and PID. It is
never stolen while that PID is live. Every holder reasserts the fixed
`writer.current` epoch immediately before a live-root mutation; restore uses a
different empty root and the parent activation lock.

### Backup

Backup and restore share the atomic sibling
`~/.anamnesis-operation.lock/`. Backup refuses when
`~/.anamnesis-restore.state` exists; restore refuses while the live-root
`backup.state` is not cleared. A backup destination must not exist—v0.1 has no
overwrite/force mode—so a prior complete or partial archive is never reused.

```text
  anamnesis backup <destination>
    PREPARE
      1. acquire the operation lock; require a nonexistent destination, create
         it as 0700; write the discoverable live-root
         ~/.anamnesis/backup.state = {operation_id, destination, phase: PREPARE}
      2. daemon acquires the backup gate; briefly take the object lease, finish
         in-flight publications, drain accepted remembers and require
         spool_pending=0
    CUTOFF
      3. under the write queue, pause every Neo4j writer (commit, extraction,
         generation, embedding, maintenance, gc, policy and receipt publication);
         redirect new remember to spool
      4. record structure_revision, ingest_seq, schema/Neo4j versions and the
         policy_revision, receipt retention configuration and exact live container
         image digest plus committed Payload hash manifest;
         fsync backup.state=CUTOFF, then release the object lease. Upload and
         spool append may proceed; GC and Neo4j writers remain paused
    DUMP
      5. fsync backup.state=STOPPING; stop the Neo4j service; fsync
         backup.state=DB_STOPPED
      6. run a one-shot admin container using the recorded **exact image
         digest** against the stopped volume: neo4j-admin database dump;
         fsync dump and backup.state=DUMPED
      7. restart Neo4j and pass health checks; fsync backup.state=DB_STARTED;
         only now resume writers and spool drain
    COPY
      8. copy exactly each manifested object data+metadata sidecar, config.jsonc
         and neo4j.auth; write SHA-256 and size for every archive member
      9. verify dump metadata and every checksum
     10. fsync files and destination; atomically publish backup.complete last;
         fsync the destination parent; clear backup.state
```

Neo4j Community 5.26 requires the database to be offline for `database dump`.
Recall is unavailable during stop/dump/start; remember remains available via
the fsynced live spool. Those post-cutoff spool entries are intentionally not
part of this point-in-time backup and drain into the live database after
restart. Copying objects after restart is safe because the manifest is fixed
at the cutoff and object files are immutable.

The fixed live-root `backup.state` and destination operation state are atomic
temp→fsync→rename→directory-fsync journals. On CLI
or daemon restart, every state before DB_STARTED first inspects and, if
needed, restarts Neo4j before releasing the gate; DUMPED preserves the dump.
DB_STARTED/COPYING may resume copying from the manifest. Spool drain cannot
resume while the journal is CUTOFF, STOPPING or DB_STOPPED. No incomplete
destination contains `backup.complete`, whose body pins the manifest hash.

### Restore

```text
  anamnesis restore <backup>
    1. acquire the operation lock; resume/recover an existing restore journal
       instead of starting another, and refuse an uncleared backup operation;
       before touching live state, require backup.complete; verify every
       checksum, schema/Neo4j compatibility, archive paths and free space
    2. create collision-free siblings on the live root's same filesystem:
         ~/.anamnesis.restore.<operation_id>/    (empty 0700 staging)
         ~/.anamnesis.rollback.<operation_id>/   (reserved rollback name)
       verify equal st_dev; never load over the live root
    3. with the matching one-shot admin container, check the dump and load it
       into the staging Neo4j volume
    4. restore exactly the manifest's object data+sidecars, config.jsonc and
       neo4j.auth; reject missing and extra authority files
    5. start staging on isolated ports; run verify --scope all; stop and fsync
    6. activation acquires the sibling ~/.anamnesis-activate.lock/ and writes
       ~/.anamnesis-restore.state = {operation_id, paths, phase: STAGED_VERIFIED}
    7. revoke/stop the live writer (§10). Write and fsync
       phase=WILL_RENAME_LIVE **before** rename live→rollback; rename; fsync
       parent; write and fsync phase=LIVE_RENAMED
    8. write and fsync phase=WILL_PROMOTE **before** rename staging→live;
       rename; fsync parent; write and fsync phase=STAGING_PROMOTED
    9. write and fsync phase=WILL_START; start and verify restored live;
       write and fsync phase=STARTED, then remove journal/lock and fsync parent
```

The old root, including its live spool, is preserved as rollback state.
Post-backup live spool entries are not replayed into the point-in-time restore
unless the operator later requests a separate, verified import. A failed
preflight or staging verify leaves the live root untouched. Because staging,
live and rollback are siblings with equal `st_dev`, activation cannot fail
with `EXDEV`.

The activation journal is at a fixed sibling path, so recovery does not depend
on which root currently owns the canonical name. Every destructive operation
has a write-ahead phase. Recovery uses this exhaustive table:

| Journal phase | Required path interpretation and action |
|---|---|
| STAGED_VERIFIED | live+staging exist, rollback reserved empty → resume stop/fence |
| WILL_RENAME_LIVE | live exists → rename not run, retry it; live absent + rollback exists → rename completed, advance |
| LIVE_RENAMED | rollback+staging exist, live absent → proceed to promotion |
| WILL_PROMOTE | staging exists + live absent → retry promotion; staging absent + live exists → promotion completed, advance |
| STAGING_PROMOTED / WILL_START | live+rollback exist, staging absent → start/verify live |
| STARTED | healthy live exists → clear journal; unhealthy → execute rollback below |
| WILL_FENCE_PROMOTED | live+rollback exist → idempotently revoke its fs_epoch, stop/kill services and verify PID/socket/pointer absent |
| PROMOTED_FENCED | live+rollback exist and fence predicate holds → proceed to quarantine |
| WILL_QUARANTINE_PROMOTED | live exists + failed absent → retry live→failed; live absent + failed exists → advance |
| PROMOTED_QUARANTINED | failed+rollback exist, live absent → proceed to rollback promotion |
| WILL_ROLLBACK | rollback exists + live absent → retry rollback→live; rollback absent + live exists → advance |
| ROLLBACK_PROMOTED | old live+failed exist → start/verify old live |
| ROLLED_BACK | healthy old live exists → clear journal; unhealthy → fail closed for operator repair |

Any path combination outside the table fails closed without deleting or
renaming another path. On startup failure, activation first writes
WILL_FENCE_PROMOTED, re-revokes the promoted daemon's fs_epoch, performs the
same bounded stop/kill and PID/socket/pointer checks as step 7, then writes
PROMOTED_FENCED. Only then does it write and fsync
WILL_QUARANTINE_PROMOTED **before** renaming promoted live to
`.failed.<operation_id>`; after rename+parent fsync it writes
PROMOTED_QUARANTINED. It then writes WILL_ROLLBACK before rollback→live,
renames+fsyncs, writes ROLLBACK_PROMOTED, starts/verifies the old root, and
writes ROLLED_BACK. Recovery applies the table to both rename boundaries.

Collision-free names are reserved before any rename, and the activation
process—not either daemon—holds the parent lock throughout handoff.

`socket.token` is
runtime access state, not backed up; first start of the activated root creates
a fresh 0600 token.

Caches are rebuilt on demand (`anamnesis rebuild --hit-cache`, maintenance
job for `m_cache` and shortlists); they are not part of what must be restored.
ConductingArc is reconstructed from restored physical links and published
complete by partition before PPR becomes available (§5), never replaced with
a native adjacency fallback.
