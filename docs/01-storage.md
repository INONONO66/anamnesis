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
             append-only EchoLineage provenance rows, AdjudicationAttempt/Proposal/
             Review/Correction records and EmbeddingResolution/Qualification records;
             retained regeneration authority, never receipt-TTL data (§4)
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
| `origin_role` | Episode | `user \| assistant \| tool \| document \| operator`, supplied by the authenticated adapter. Immutable; extraction never infers it from prose (D49) |
| `lineage_digest` | Episode | SHA-256 of the canonical `EchoLineage` body written in the same transaction; part of the version-2 Episode digest only (§3.3, §1 *Episode digest versions*) |
| `session_key` | Episode | `sha256(origin_source, origin_session)`; namespaces session order across adapters |
| `origin_key` | Episode | **Logical** source identity (`sha256(source, session, actor, record)`). Indexed, **not unique** — every revision of the same document shares it |
| `source_revision` | Episode | Opaque adapter-issued token, stable across retries and unique for each revision of one `origin_key` |
| `revision_key` | Episode | `sha256(origin_key, source_revision)`. **Unique.** Identity of one revision occurrence, including A→B→A reverts |
| `previous_revision_key` | Episode | Explicit predecessor for a revision; null only for the first occurrence |
| `ingest_seq` | Episode | Globally monotonic integer allocated in the remember transaction, last of the statements that do not depend on it; unique build/catch-up cursor. Gapless: an aborted remember consumes no number |
| `ingested_at` | Episode | Server ms, written once at CREATE. **Not used in snapshot computation** — audit and spool-drain ordering only (docs/03 §1) |
| `payload_hash` | Episode | Payload reference (optional) |
| `episode_digest_version` | Episode | Server-selected immutable digest discriminator. Absent on every Episode stored before D49 implementation, which means version 1; `2` on every Episode admitted afterwards; any other stored value is `unsupported_digest_version`. The caller never supplies or downgrades it (*Episode digest versions* below) |
| `digest` | Episode | SHA-256 for integrity and retry conflict detection, computed by the row's own version. Version 1 hashes the frozen insertion-ordered body `{schema, content, properties, time, payload_hash, previous_revision_key}`; version 2 hashes the RFC-8785 body `{episode_digest_version, schema, content, properties, time, payload_hash, previous_revision_key, origin_role, lineage_digest}`. Byte-identical to the version-2 body in docs/02 §3; under one `revision_key`, a changed version-1 field, or a changed version-2 role or lineage body, is `revision_conflict` |
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
| `content_language` | Fact | `^[a-z]{2,8}(-[a-z0-9]{1,8})*$`, at most 35 ASCII characters, or `mul` for materially multilingual prose, or `und` for nonlinguistic/insufficient evidence. Required, immutable, meaning-bearing (D48) |
| `speaker_key`, `subject_keys`, `time_key`, `duplicate_group_key` | Fact | `sha256(RFC-8785 {origin_source, origin_actor})` or null; 1–16 sorted resolved Entity IDs or null; the exact `{time_utc, time_precision}` pair; the local grouping key below when every component resolves (D49) |
| `predicate_text`, `scope`, `scope_complete` | Fact | NFC case-preserving source-language relation (1–256 scalars), the closed bounded scope object below, and whether every scope component resolved. Meaning-bearing (D49) |
| `echo_state`, `echo_of_element_id`, `echo_depth`, `echo_lineage_truncated` | Fact | `direct \| known_echo \| context_derived \| unknown`, the exact delivered result a known echo repeats, depth 0..8, and whether a bound truncated the lineage (§3.3) |
| `parent_recall_ids`, `corroboration_root_episode_ids` | Fact | Sorted immutable 0..4 parent receipt IDs and 0..16 root Episode IDs copied from the Episode's lineage row. Provenance accounting only; never authority, candidates or a score term |
| `modality` | every Fact, including synthesis | `asserted / reported / hedged / intended / hypothetical` — the speech act of this content (docs/02 §5.1). Required, meaning-bearing: part of identity, input to the forgetting prior, returned on recall |
| `confidence` | Fact | Judge's belief in [0,1] that the claim is what the source says. Stored, input to `m₀`, **not** part of identity — model nondeterminism on a scalar must not fork Facts |
| `prior_version`, `calibration_version`, `judge_version` | Fact | Immutable versions used to assign confidence and `m0`; generation configuration pins them. Raw validated judge output is retained in bounded `properties.audit` |
| `span` | primary `DERIVED_FROM` link (Fact → Episode) | `[start, end)` UTF-8 byte offsets into the Episode's content the claim rests on; validated at write, optional when the claim has no single locus (docs/02 §5 W2) |

### Episode digest versions (D49)

D49 changes Episode digest identity **prospectively**. It never rewrites,
reserializes or relabels an original, and this document set ships no
compatibility code and no data migration (docs/08, docs/09).

```text
  legacy Episode: episode_digest_version absent  => version 1
  new Episode:    episode_digest_version = 2     => version 2
  any other stored value                         => unsupported_digest_version
```

Version 1 is the frozen pre-D49 byte contract already used by stored
Episodes, not a body to reinterpret with the version-2 serializer:

```text
  legacy_v1_body = insertion-ordered {
    schema, content, properties, time, payload_hash, previous_revision_key
  }
  digest_v1 = sha256(UTF-8(ECMAScript JSON.stringify(legacy_v1_body)))
```

That insertion order is exact. `properties` is the validated properties object
(or `{}`) after removing its top-level `payload_hash` member, keeping the
existing ECMAScript `Object.entries` / `Object.fromEntries` ordering; `time`
is the existing `{value, precision}` object, or JSON `null` when absent or
when the schema carries no event time; an absent payload or predecessor value
is JSON `null`. A version-1 check calls that frozen
verifier and must not sort keys, apply RFC-8785, add a default or inject a
lineage field before comparing.

Every revision accepted after D49 implementation is version 2, including a new
`source_revision` whose older revisions are version 1:

```text
  digest_v2 = sha256(UTF-8(RFC-8785({
    episode_digest_version: 2,
    schema, content, properties, time, payload_hash, previous_revision_key,
    origin_role, lineage_digest
  })))
```

The stored row decides. An existing `revision_key` verifies under its own
version, and an absent value on that row means version 1, so a version-1 row
stays a no-op on exact retry and never gains `episode_digest_version`,
`origin_role`, `lineage_digest` or an `EchoLineage` row in place. A legacy row
without authenticated lineage stays legacy/unknown under §3.3; only a new
source revision can carry version-2 lineage. Digest version is not an
eligibility, policy, candidate or ranking input, and recognizing it changes no
Episode ID, revision chain, payload, event time or snapshot result.

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
    generation, schema, content, content_language, properties, time, sub_kind, modality,
    primary_episode_id, max_source_ingest_seq,
    echo_state, echo_of_element_id, echo_depth, echo_lineage_truncated,
    sorted(parent_recall_ids), sorted(corroboration_root_episode_ids),
    sorted(entity_ids), sorted(source_episode_ids), sorted(support_fact_ids)
  })
```

`properties` here includes the meaning-bearing `predicate_text`, `scope` and
`scope_complete` fields. An exact retry that repeats the same meaning under
different lineage is therefore a conflict, not a collision: lineage is
provenance, and provenance cannot be silently replaced. Generation mapping
copies language and lineage fields one-to-one.

Here `properties` means the canonical meaning-bearing properties only (the
stored properties object with the reserved `audit` member omitted); raw
judge output, `confidence`, audit versions, and byte spans are retained but
excluded from identity. Otherwise nesting confidence inside raw output would
silently defeat its exclusion. A generation pins prior/calibration/judge
versions; changing `m0` priors requires a new derived generation, never Hit
replay or a SET of `m0`. Synthesis confidence measures faithfulness to its
support bundle, not world truth or a product of uncalibrated source scores;
its modality is judged from its own content (D42, D45, D47).

### Grouping and scope fields (D49)

The authenticated adapter and the extractor materialize these immutable
fields; every ID/key array is distinct and sorted by unsigned UTF-8 byte
order, and quantities sort by `(unit UTF-8 bytes, value ASCII bytes)`:

```text
  speaker_key    = sha256(RFC-8785 {origin_source, origin_actor}), or null
  subject_keys   = 1..16 sorted resolved Entity IDs, or null.
                   There is no literal or normalized-string fallback:
                   an unresolved subject stores null and disables grouping
  predicate_text = NFC case-preserving source-language relation, 1..256 scalars
  scope = {
    object_keys:   0..16 sorted resolved Entity IDs,
    location_keys: 0..8  sorted resolved Entity IDs,
    quantities:    0..8  sorted distinct {value, unit}, where value is a
                   canonical non-exponent decimal of at most 64 ASCII chars
                   (no plus sign, no exponent, no integer leading zero, no
                   fractional trailing zero, and zero encoded exactly "0")
                   and unit is an NFC string of 1..64 scalars,
    condition:     null or NFC source-language text of at most 256 scalars,
    attribution_speaker_keys: 0..8 sorted speaker keys
  }
  scope_complete = boolean
  time_key       = {time_utc, time_precision}
  modality       = the required D42 enum
```

Unresolved subjects or predicates, unknown units, free qualifiers that do not
fit the closed object, and unresolved reported speakers all force
`scope_complete=false`. When every component resolves, assembly pins
`grouping_version = "anamnesis.duplicate-group/1"` and computes
`predicate_key = sha256(UTF-8 predicate_text)`,
`scope_key = sha256(RFC-8785 scope)` and
`duplicate_group_key = sha256(RFC-8785 {grouping_version, subject_keys,
predicate_key, time_key, scope_key, modality})`. Validators enforce shape,
bounds, sorting, decimal canonicalization and ID resolvability; they assert no
semantic equivalence, and the key never establishes global person, Entity or
predicate identity.

Two audit-only extractor fields ride alongside and are **not** Fact
properties: `corrects_local_claim_index` (null or an earlier index in 0..31)
and `correction_scope_text` (null or NFC source-language slot text, 1..256
scalars). They exist for the intra-Episode L1b correction rule in docs/02
§5.3, which uses a deliberately narrower scope predicate than grouping.

### Occurrence provenance (D46)

Semantic duplication never suppresses an occurrence's extracted assertion.
Each source occurrence receives its own immutable Fact, direct Episode
authority, modality, time and optional span, even when another Fact says the
same thing. Link it to that Fact with existing `RELATES_TO` or semantic
`DERIVED_FROM`; no new merge relation or automatic confidence/truth boost.
A translated or transliterated rendering is never a second occurrence: a
generated English projection is neither authority, claim content, evidence,
nor an Entity alias, and it is not persisted or indexed until a later
decision pins its schema, prompt and digest (D48).
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
and ranking state, budget and exact result/context digests (docs/05). It also
stores an immutable
`selection_digest = sha256(RFC-8785(ordered array of at most 64 delivered
{element_id, root_episode_ids, echo_depth, complete} records))`, which is the
exact value a later `EchoLineage` copies into `context_digests` (§3.3); without
it the lineage row would name a digest the receipt schema never retained. It
does not duplicate complete raw Episode text. A recorded impression means response
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

EchoLineage (§3.3) copies the bounded authority it needs out of a parent
receipt at `remember` time and stores it in its own retained control row.
Receipt TTL therefore still governs only the feedback window: a parent
receipt's later expiry cannot change a child Episode's recorded lineage, and
replay reads the lineage row rather than the receipt.

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

### 3.3 Echo lineage (D49)

An Episode that repeats what anamnesis just delivered is not new evidence.
The authenticated adapter declares `origin_role` and
`lineage_mode = direct | receipts`. Direct user, tool-observation, document
and operator input has no parents, depth 0 and its own Episode ID as its one
root; an assistant turn to which the adapter delivered no anamnesis context
uses the same form. An assistant turn that did receive context must use
`receipts` and supply 1..4 distinct `parent_recall_ids`, each snapshotting at
most 64 delivered results as
`{element_id, root_episode_ids[1..16], echo_depth, complete}`.

```text
  (:EchoLineage {episode_id, lineage_mode, parent_recall_ids[0..4],
                 context_digests[0..4], root_episode_ids[0..16],
                 echo_depth: 0..8, complete})
```

The daemon verifies the caller/client binding and appends this row atomically
with the Episode. `context_digests` copies the parent receipts' stored
`selection_digest` values, so lineage never depends on recomputing an expired
selection: each receipt persists
`selection_digest = sha256(RFC-8785(ordered array of at most 64 delivered
{element_id, root_episode_ids, echo_depth, complete} records))` at creation
(§3.1). Parent and digest arrays have equal length; each digest is paired with
its parent ID, then sorted by recall ID. Roots are the sorted distinct union of
the referenced snapshots' roots, and depth is `1 + max(item.echo_depth)`.
`complete=true` only when every referenced item is complete and neither cap
truncates the result. Parent receipts must still exist when `remember`
validates them; their later expiry cannot change the copied lineage. The
canonical body digest is part of the **version-2** Episode digest and unique by
`episode_id`: an exact retry is a no-op and a different body is
`idempotency_conflict`. Lineage rows exist only for version-2 Episodes; an
Episode stored under version 1 never gains one in place, stays legacy/unknown
here, and only a new source revision can carry version-2 lineage (§1).

Each Fact copies the bounded metadata in §1. A known echo copies the exact
delivered item's roots and stores `1 + item.echo_depth`; a context-derived
Fact copies the Episode's root union and depth. Neither adds the assistant
Episode as another root. A union over 16 roots keeps its first 16 IDs in
bytewise order and computed depth over 8 stores 8; either overflow sets
`complete=false`, `echo_state=unknown` and `echo_lineage_truncated=true`, and
no omitted ancestor is treated as a new root. With complete receipt lineage,
`known_echo` requires an L4 `DUPLICATE_OCCURRENCE` against an exact delivered
result ID whose roots and depth agree byte-for-byte across every parent
receipt that contains it; any other Fact from that Episode is
`context_derived`, and a disagreement is `unknown`. Historical assistant
input without authenticated receipt metadata is `unknown`, never presumed
independent.

A synthesis has no source Episode lineage row to copy, so it materializes one
from its exact 1..16 non-synthesis support Facts before Fact identity is
computed. With
`lineage_complete(f) = f.echo_state ≠ unknown ∧ ¬f.echo_lineage_truncated`,
let `P` be the sorted distinct union of the supports' stored
`parent_recall_ids` and `R` the sorted distinct union of their stored
`corroboration_root_episode_ids`, both in unsigned UTF-8 byte order. The
synthesis stores `first_4(P)`, `first_16(R)`,
`echo_depth = max(support.echo_depth)` with no added receipt hop, and
`echo_of_element_id = null`. It is `context_derived` with
`echo_lineage_truncated=false` **iff** every support is lineage-complete,
`|P| ≤ 4` and `|R| ≤ 16`; otherwise it stores the capped arrays, `unknown` and
`echo_lineage_truncated=true`. It never adds a support's primary or source
Episode as another root. `verify` and generation mapping recompute this exact
rule and require byte-identical results; the existing ban on
synthesis-on-synthesis support keeps it a one-level materialization rather
than an ancestry traversal.

Storage still preserves the echoed Fact, its actor, time, modality and its own
Episode provenance. Occurrence count and root count never alter confidence,
`m0`, mass, utility, rank or adjudication, and neither can elect a conflict
winner. The gate covers the source Episode itself: an assistant Episode with
unknown or incomplete lineage, and every output derived from it, is stored but
ineligible for semantic candidates, for synthesis and for invalidation
(`echo_lineage_unavailable`). An unknown synthesis is retained under the same
rule. Only a new source revision with complete bounded metadata changes that
state. A later user-authored statement
is a new direct occurrence; adopting a receipt is usage evidence, not
semantic corroboration. Lineage never supplies or changes Fact event time,
snapshot visibility, validity, Hit attribution or source authority, and no
online operation traverses parents: replay reads the materialized row and
policy checks at most 16 roots.

### 3.4 Adjudication control records (D50)

Shadow adjudication persists its premises so an accepted proposal can be
revalidated from its own stored shape rather than from current graph state:

```text
  (:AdjudicationAttempt {attempt_id, target_generation, episode_id,
                         source_head_revision_key, policy_revision,
                         candidate_digest, judge_profile_id, started_at,
                         finished_at, outcome, error_code?, error_digest?})
  (:AdjudicationProposal {proposal_id, target_generation, episode_id,
                          source_head_revision_key, policy_revision,
                          proposed_claim_digest, candidate_digest, verdict,
                          target_ids[0..8], evidence_ids[1..32],
                          effective_time_basis, reason, judge_profile_id})
```

`source_head_revision_key` is the exact `OriginHead` value captured before the
bounded candidate read and the model call, and `policy_revision` is the
integer captured at that same point; both are 64-lowercase-hex or nonnegative
safe integers respectively. The proposal copies them, plus its target,
Episode, candidate digest and judge profile, byte-for-byte from its successful
attempt and never samples current state to fill them. `proposal_id` equals
that `attempt_id`.

`proposed_claim_digest = sha256(UTF-8(RFC-8785(validated complete L1 claim
object)))`. The covered object is the one enumerated in docs/02 §5.2 and
includes content and `content_language`, sub-kind, modality, confidence, the
evidence quote/kind/span, the resolved `time_value`/`time_utc`/`time_precision`,
the complete validated Entity mentions in extraction order, `speaker_key`,
`subject_keys`, `predicate_text`, `scope`, `scope_complete`, and the local
correction fields `corrects_local_claim_index`, `correction_scope_text` and
`mode`. Every key is present and absent optional values are JSON `null`.
`AdjudicationCorrection` and the correction RPC reference exactly this digest.

W1 compares those stored values directly under the write lock (docs/02 §5).
`verify` checks the same chain: every proposal has its attempt, the persisted
premises agree, at most one terminal review exists per proposal, and no
consumption row exists without an ACCEPTED proposal.

## 4. Derived layer and generations

The derived layer is split into three streams.

```text
  extraction   Fact · Entity · MENTIONS · RELATES_TO · DERIVED_FROM · Fact→Fact INVALIDATES · CONTRASTS
               selector active[extraction] = integer generation
  community    Community · HAS_MEMBER
               selector active[community]  = integer generation
  embedding    embedding_m_<modelhex> property + vector index
               selector active[embedding]  = embedding_profile_id. No generation — a property is present or absent
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
`(:EmbeddingCoverage {stream, generation, embedding_model_id, ...})` is
unique by `(stream,generation,embedding_model_id)` and advances contiguously
over a terminal prefix. An in-flight profile swap owns one
`(:EmbeddingBuild {embedding_profile_id, state, ...})` plus immutable
`(:EmbeddingBuildSource {embedding_model_id,stream,generation,high_watermark})`
rows (see **The embedding stream** below).

An extraction generation's immutable configuration additionally pins
`fact_language_policy ∈ {source, en}`, the extraction prompt artifact digest,
`extractor_profile_id`, the grouping/scope schema version, the adjudication
`judge_profile_id` in use for its proposals, and the validator version.
Changing the language policy is a replacement generation with the normal
cutover and rollback path, never an in-place rewrite (D48). Cutover carries
forward every `AdjudicationCorrection` as an `AdjudicationCorrectionMap`
whose `{old_id, new_id}` pairs must match the same assertion occurrence:
exact primary and source Episode IDs, effective time, sub-kind, modality,
meaning-bearing properties and resolved Entity name/kind snapshots.
Exact-copy reconciliations also require content equality. A `source ↔ en`
generation may differ in content only under an explicit one-to-one
`TranslationMapping {mapping_id, from_generation, to_generation, from_fact_id,
to_fact_id, source_episode_ids[1..16], source_span, mapper_profile_id,
prompt_digest, decision_digest}`. That mapping is a versioned pairwise
judgment over the same exact source evidence, not a hash-based equivalence
claim and never a global Fact or Entity identity claim; a missing span, a
changed authority or frame, or any duplicate, fuzzy, one-to-many or
many-to-one mapping blocks activation (D48, D50).

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
       target_embedding_profile = null,
       EmbeddingCoverage(target, active model).covered_ingest_seq
         = covered_ingest_seq,
       EmbeddingCoverage(episode, 0, active model).covered_ingest_seq
         = Meta.ingest_seq,
       all generation-scoped indexes ONLINE, authority and links valid,
       ConductingArc coverage COMPLETE for every retained physical partition,
       invalidation marker coverage and target mappings complete,
       source-validity/invalidation outcomes preserved at every supported T,
       every AdjudicationCorrection carried by a complete correction map and
       every content-changing language pair carried by a one-to-one
       TranslationMapping
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

Embeddings are per-model properties and per-index vector indexes, and their
identity is three separate fingerprints (D51). Each is the SHA-256 of the
named canonical RFC-8785 object:

- `embedding_model_id` covers repository/revision, artifact file, SHA-256,
  size and quantization, tokenizer artifact and runtime, request
  serialization, pooling, dimension and normalization, document prefix, the
  exact query template, and context length.
- `vector_index_id` covers `embedding_model_id`, layout version, Neo4j version
  family and provider, dimension, similarity, index quantization and HNSW
  settings.
- `embedding_profile_id` covers the exact `(embedding_model_id,
  vector_index_id)` pair and is the value of `active[embedding]`.

A model-field change builds a new vector property and new indexes. An
index-only change may reuse vectors with the exact same `embedding_model_id`
but builds a new index; it never mutates an active index in place. Technical
tokens use the 64 lowercase hex characters with no `sha256:` prefix:

```text
  node property         embedding_m_<modelhex>
  RELATES_TO property   embedding_g<N>_m_<modelhex>
  physical indexes      vec_episode_<indexhex>
                        vec_fact_g<N>_<indexhex>
                        vec_rel_g<N>_<indexhex>
  active[embedding] = <embedding_profile_id>
```

The experimental baseline profile is the resolved Qwen Q8_0 artifact:

```text
embedding_model_id:
  711660f809e00490d029df7e65b19dfccc0dda52c81a8a93a5ab0fed796e13e9
vector_index_id:
  0dfe3d3a058d2ec25ed9fd9193227f94782bddc269e2fc66f148816eebef81a9
embedding_profile_id:
  16d404a70ca92beccbe06fae1c1bc400d924223a09c498c7f01278b0f795405b
artifact:
  repository Qwen/Qwen3-Embedding-0.6B-GGUF
  revision   370f27d7550e0def9b39c1f16d3fbaa13aa67728
  file       Qwen3-Embedding-0.6B-Q8_0.gguf, quantization Q8_0
  sha256     06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439
  size       639,150,592 bytes
runtime:  tokenizer from that GGUF; llama.cpp 0.4.0-dev build 10819,
  commit 6a1a922d269908a29cbd4b49c27e6a8e7fd10fae, CPU-only, 4 threads,
  n_gpu_layers=0, embeddings enabled, OpenAI-compatible serialization
encoding: context 4096, last-token pooling, 1024 dimensions, L2 normalized;
  Episode/Fact/Entity documents are exact normalized-content UTF-8 and
  RELATES_TO documents are exact content UTF-8, both without instruction;
  queries are the exact verbatim UTF-8 RecallRequest.query and use the frozen
  Qwen retrieval template
  "Instruct: Given a web search query, retrieve relevant passages that answer
   the query\nQuery:{query}"
index: layout anamnesis.neo4j-vector-layout/1; Neo4j 5.26.x, provider
  vector-2.0, cosine, 1024 dimensions, vector.quantization.enabled=false,
  vector.hnsw.m=16, vector.hnsw.ef_construction=100
```

Q8_0 weight quantization and Neo4j vector quantization are different settings:
the first is part of the artifact and the second is disabled. Q8_0/FP16 parity
is unmeasured, so an FP16 build necessarily has a different model and profile
ID. The query instruction is a frozen operational default, not a measured
optimum. Preflight tokenizes the complete endpoint-formatted request including
special and framing tokens; it never assumes a raw `/tokenize` count equals
the embedding endpoint's count. The client rejects zero or nonfinite output
and L2-normalizes every document and query vector before persistence or
search. Every index uses this exact options object:

```text
indexProvider: "vector-2.0"
indexConfig: {
  `vector.dimensions`: 1024,
  `vector.similarity_function`: "cosine",
  `vector.quantization.enabled`: false,
  `vector.hnsw.m`: 16,
  `vector.hnsw.ef_construction`: 100
}
```

Instantiate these DDL shapes with a validated integer `<N>` and lowercase hex
IDs; those schema tokens are never request text:

```cypher
CREATE VECTOR INDEX `vec_episode_<indexhex>` IF NOT EXISTS
FOR (n:Episode) ON (n.`embedding_m_<modelhex>`) OPTIONS <options-above>;
CREATE VECTOR INDEX `vec_fact_g<N>_<indexhex>` IF NOT EXISTS
FOR (n:ExtractionG<N>) ON (n.`embedding_m_<modelhex>`) OPTIONS <options-above>;
CREATE VECTOR INDEX `vec_rel_g<N>_<indexhex>` IF NOT EXISTS
FOR ()-[r:RELATES_TO]-() ON (r.`embedding_g<N>_m_<modelhex>`)
OPTIONS <options-above>;
```

The exact Neo4j patch and image digest is captured in build and qualification
receipts; a minor-version change requires a new index ID.

#### Per-entry work, failure and coverage

```text
  PENDING -> RUNNING
  RUNNING -> SUCCEEDED | NO_VECTOR_REQUIRED | RETRY_WAIT | BLOCKED
  RETRY_WAIT -> RUNNING
  BLOCKED -> PENDING                authenticated retry after repair
  BLOCKED -> RESOLVED_NO_VECTOR     authenticated operator skip
  PENDING | RUNNING | RETRY_WAIT | BLOCKED -> CANCELLED
                                    unreferenced target work only
```

`SUCCEEDED`, `NO_VECTOR_REQUIRED`, `RESOLVED_NO_VECTOR` and `CANCELLED` are
terminal; every unlisted transition rejects. `stream = episode | extraction`,
with `generation=0` only for `episode`. `EmbeddingWork` is unique by
`(embedding_model_id, stream, generation, ingest_seq, item_ordinal)`, where
`item_ordinal` is the deterministic `(kind_order, id)` position in 0..1023
(`Episode=0`, `Fact=1`, `Entity=2`, `RELATES_TO=3`, sentinel `=4`), and stores
`source_id?`, `input_digest`, state, `attempts_in_cycle`, `attempts_total`,
`retry_cycle` and `next_retry_at`. `input_digest` is SHA-256 of the exact
UTF-8 string presented as that endpoint input before JSON escaping. Each
immutable `EmbeddingAttempt {attempt_id, embedding_model_id, stream,
generation, ingest_seq, item_ordinal, source_id?, input_digest,
attempt_ordinal, started_at, finished_at, outcome, error_code, error_digest}`
carries no source text; `attempt_id` is SHA-256 of the canonical work key plus
`attempt_ordinal`. Outcome is `succeeded | no_vector_required |
transient_failure | permanent_failure | worker_lost | cancelled`, and error
code is one of `unavailable | timeout | rate_limited | server_error |
invalid_input | context_overflow | zero_norm | nonfinite | wrong_dimension |
profile_mismatch | cardinality_mismatch | malformed_response | client_error |
worker_lost | cancelled`. A worker-lease loss closes the RUNNING attempt as
`worker_lost` and follows the same retry rule; it never silently resets a
counter.

One sequencer per `(embedding_model_id, stream, generation)` publishes only
the current lexicographic `(ingest_seq, item_ordinal)` head. An ingest
sequence with no vector-bearing item has one ordinal-0 sentinel. A batch is a
contiguous prefix from that head and obeys docs/02's 256 KiB encoded outbound
body cap; preflight tokenizes every item, commits only the successful prefix,
and leaves every row after the first failure unpublished, so no later vector
appears across a hole. More than 1,024 vector-bearing outputs in one ingest
sequence rejects that producer transaction rather than omitting a tail.
`NO_VECTOR_REQUIRED` is permitted only for a schema excluded by contract (a
policy or control Episode, for instance) or an extraction sequence with no
vector-bearing output; it is not a failure escape hatch.

Retry covers only the transient `unavailable`, `timeout`, `rate_limited`
(429) and `server_error` (5xx) classes. Each automatic cycle has three
attempts with fixed, no-jitter delays of `[1000, 10000]` ms after attempts one
and two; a third transient failure becomes `BLOCKED`. Authenticated retry
increments `retry_cycle`, zeros `attempts_in_cycle` and opens another
identical cycle, while `attempts_total` and the immutable attempt rows never
reset. Invalid input, tokenizer or context overflow, zero, nonfinite or
wrong-dimension vectors, artifact/profile mismatch, response-cardinality
mismatch, `malformed_response` output and deterministic `client_error` (4xx)
responses become `BLOCKED` immediately. Never
truncate, chunk, silently skip, synthesize a zero vector or advance coverage
past a nonterminal entry.

`EmbeddingCoverage` stores `health = HEALTHY | BLOCKED`, `covered_ingest_seq`,
`resolved_no_vector_count` and
`omission_digest = sha256(RFC-8785(sorted [{stream, generation, ingest_seq,
item_ordinal, source_id, input_digest}]))` ordered lexicographically by the
first four fields. Its cursor is the greatest contiguous ingest sequence for
which every item is `SUCCEEDED`, `NO_VECTOR_REQUIRED` or
`RESOLVED_NO_VECTOR`. A BLOCKED head freezes publication and that model's
cursor while BM25 and session recall continue and the currently active profile
keeps serving through its prior prefix. Any coverage advance affecting the
ACTIVE profile, including publication released by a skip, increments
`structure_revision` in the same transaction; hidden-profile progress does
not. Status reports profile, stream/generation, ingest sequence, source ID,
attempt count, bounded error code and digest, and first/last failure time,
with no source text.

Whenever recall selects the vector channel, the durable `RecallReceipt`
records `embedding_profile_id` once plus one `embedding_coverages` row for
each applicable partition of the active profile: `(episode,0)` always, and
`(extraction, active[extraction])` when an active extraction generation
exists. Rows carry `stream`, `generation`, `health`, `covered_ingest_seq`,
`required_ingest_seq`, `lag` and `omission_digest`, so two simultaneously
BLOCKED partitions with different cursors and different omission digests stay
separately replayable (docs/05 §9).

#### Operator recovery

Recovery is authenticated and append-only. Every command writes
`EmbeddingResolution {operation_id, action: retry | skip | cancel,
embedding_profile_id?, embedding_model_id?, stream?, generation?, ingest_seq?,
item_ordinal?, source_id?, input_digest?, failure_digest?, reason, actor,
accepted_at}`; all IDs are server-validated and `reason` is 1..512 Unicode
scalars. `embedding.retry` moves the exact BLOCKED head to PENDING and cannot
alter source text or profile identity. `embedding.skip` moves one BLOCKED head
to `RESOLVED_NO_VECTOR`: it permits contiguous coverage but permanently
excludes that source from this model and every dependent profile's vector
channel, while BM25 and session paths remain. The default activation policy
permits zero skips; any nonzero bound is a new explicit qualification value,
never inferred from ONLINE status, and a skip that would exceed an ACTIVE
dependent profile's cumulative bound is rejected with the head left BLOCKED.
Input transformation, meaning a larger context, deterministic chunking, a
changed prefix, tokenizer or pooling, or a corrected artifact, requires a
**new model ID and profile** and a complete build; it is never an operator
mutation of one job. `embedding.cancel` marks a non-active target build
CANCELLED and detaches it from model work under the write-queue barrier:
shared model work continues for any other live profile, otherwise remaining
jobs become CANCELLED and an in-flight result loses its compare-and-swap.
Later GC removes only unreferenced property and index state. The active
profile cannot be cancelled.

Operation IDs are UUIDv7. The same ID with a byte-identical canonical body is
a no-op; a different body is `operation_conflict`. Retry and skip require the
named row to be the current BLOCKED tuple head, so stale or non-head
operations reject. Resolution records are retained control authority, included
in backup and `verify`, and are not removed by `gc --embedding`; this contract
defines no audit-retention deletion.

#### Build lifecycle and activation

```text
  EmbeddingBuild {embedding_profile_id, embedding_model_id, vector_index_id,
                  neo4j_version, neo4j_image_digest, deployment_mode, state,
                  episode_high_watermark, extraction_generation,
                  extraction_high_watermark, qualification_id?,
                  created_at, activated_at?}
  deployment_mode = development | production        immutable
  state = BUILDING | BLOCKED | ACTIVE | INACTIVE | CANCELLED | RETIRED
```

A BUILDING head failure moves the build to BLOCKED; a successful retry or skip
returns it to BUILDING once no head is blocked. An ACTIVE head failure leaves
the selector and build ACTIVE, marks coverage health BLOCKED, and serves the
prior prefix until recovery. Only BUILDING can become ACTIVE at cutover, and
the previous ACTIVE becomes INACTIVE atomically. BUILDING or BLOCKED may
become CANCELLED; INACTIVE may reopen as BUILDING for a caught-up rollback or
become RETIRED after retention; CANCELLED may become RETIRED for GC. ACTIVE
has no direct cancellation transition, at most one profile is ACTIVE, and
every unlisted transition rejects.

Opening a target build retains the write-queue capture, the exclusive
`target_embedding_profile`, the dual tail and mutual exclusion with an
extraction build or rollback:

1. Acquire the lifecycle/write-queue barrier and require no extraction
   generation is BUILDING or CATCHING_UP. In one transaction create the
   BUILDING `EmbeddingBuild`, set `target_embedding_profile=<new>`, and
   capture immutable source rows for global Episodes at `Meta.ingest_seq` and
   every ACTIVE extraction generation at its `covered_ingest_seq`; the old
   profile stays active.
2. Enqueue target-model jobs through every captured source high watermark,
   using the model-aware Outbox key.
3. Every later remember and every ACTIVE extraction commit enqueues embedding
   jobs for both the active and target models, so target jobs are an
   exclusive dual tail above the captured watermarks.
4. Under the write-queue barrier, require target-model Episode coverage equal
   to **current** `Meta.ingest_seq` and target coverage for the ACTIVE
   extraction generation equal to that generation's **current**
   `covered_ingest_seq`. Through those cursors, additionally require no
   PENDING, RUNNING, RETRY_WAIT or BLOCKED entries, every required physical
   index ONLINE with the exact profile settings, a qualification whose mode
   admits the requested deployment, and a skip count within policy. Then
   switch active, clear the target and increment `structure_revision` in one
   transaction.

`ONLINE` means only that an index can answer queries; it is not quality
approval. Development activation additionally requires
`EmbeddingQualification {qualification_id, embedding_profile_id,
mode: development | production, fixture_manifest_digest, result_digest,
thresholds_digest, max_resolved_no_vector, omission_digest, operator,
accepted_at}`, where `max_resolved_no_vector` is a nonnegative integer over
all required target-model rows through the activation watermarks and the
record carries the matching omission digest. The record is authenticated and
append-only. **Production activation is disabled** until a later qualification
references machine-validated manifests from the applicable M4/M5 gate and
supplies predeclared quality and resource thresholds; operator attestation or
ONLINE status alone cannot satisfy it, and this contract invents no
thresholds. Authenticated `gen {stream: embedding, action: qualify,
qualification: <exact record above>}` appends it, with the same replay and
conflict semantics as the other control operations. Qualification never
activates a profile by itself.

Failure leaves the old profile active; there is no partial activation.
Opening a rebuild or rollback extraction generation is refused while
`target_embedding_profile` is set, so the vector-bearing generation set cannot
change between the build's atomic capture and activation. The old profile
remains a rollback target for 30 days, and a rollback must catch it up and
cross the same barrier. The previous property and index are removed by
`gc --embedding <embedding_profile_id>` under the same retention rule; GC
refuses ACTIVE, BUILDING and BLOCKED profiles, rollback targets, and shared
model properties still referenced by any retained index. `RELATES_TO.content`
is embedded under the same rule.

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
          · EchoLineage.episode_id
          · AdjudicationAttempt.attempt_id · AdjudicationProposal.proposal_id
          · AdjudicationReview.review_id · AdjudicationConsumption.proposal_id
          · AdjudicationCorrection.correction_id
          · AdjudicationCorrectionMap(correction_id, to_generation)
          · TranslationMapping(from_generation, to_generation, from_fact_id)
          · TranslationMapping(from_generation, to_generation, to_fact_id)
          <role>.idem_key (7) · <role>.id (7; indexed per-role link-ID resolution)
          · ConductingArc(source_id, link_id)
          · ConductingArcCoverage(stream, generation)
          · Outbox(stage, target_generation, ingest_seq, model_key)
          · EmbeddingCoverage(stream, generation, embedding_model_id)
          · EmbeddingWork(embedding_model_id, stream, generation, ingest_seq, item_ordinal)
          · EmbeddingAttempt.attempt_id · EmbeddingResolution.operation_id
          · EmbeddingQualification.qualification_id
          · EmbeddingBuild.embedding_profile_id
          · EmbeddingBuildSource(embedding_model_id, stream, generation)
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
          composite EmbeddingWork(embedding_model_id, stream, generation, state, ingest_seq, item_ordinal)
          composite AdjudicationProposalState(target_generation, state, proposal_id)
          composite Fact(generation, duplicate_group_key)
          RecallReceipt.expires_at
fulltext  Episode.content (global) · Fact/Entity content (one index per generation)
vector    Episode.embedding_m_<modelhex> (global, vec_episode_<indexhex>)
           · :ExtractionG<N>.embedding_m_<modelhex> (vec_fact_g<N>_<indexhex>)
           · RELATES_TO.embedding_g<N>_m_<modelhex> (vec_rel_g<N>_<indexhex>)
           (one node/relationship index per generation and vector_index_id)
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
`model_key=embedding_model_id`, so it cannot collide with an older model's
completed entry.

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
  Element.embedding_m_<modelhex>               backfill; write-once null → vector
  RELATES_TO.embedding_g<N>_m_<modelhex>        generation-partitioned relationship vector
  Outbox.{state, attempts, next_retry_at,
          error, processed_at}                 sequencer/blocked-head cursor
  OriginHead.revision_key                      CAS head cache, rebuilt from the immutable revision chain
  Generation.{state, next_ingest_seq,
               covered_ingest_seq}              build/cutover lifecycle
  EmbeddingCoverage.{covered_ingest_seq, health,
         resolved_no_vector_count, omission_digest}
                                                 model-scoped contiguous embedding cursor and blocked state
  EmbeddingWork.{state, attempts_in_cycle,
         attempts_total, retry_cycle, next_retry_at}
                                                 per-entry embedding state machine (§4)
  EmbeddingBuild.{state, qualification_id, activated_at}
                                                 BUILDING→ACTIVE/INACTIVE/BLOCKED/CANCELLED/RETIRED lifecycle
  AdjudicationProposalState.state                SHADOW→ACCEPTED/REJECTED, rebuilt from append-only reviews
  Meta.{structure_revision, policy_revision, ingest_seq,
         last_server_time, writer_epoch, active_*,
         target_embedding_profile,
         conducting_arc_ready}                   serving selectors, profile build, clock, fencing and cache gate
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
  AdjudicationCorrectionMap                    per-build old/new correction mapping (§4)
  TranslationMapping                           per-build one-to-one language pairing (§4)
DELETE allowed in retained control state
  expired RecallReceipt / RecallFeedback        explicit receipt TTL; never removes Hit outcomes
                                                EchoLineage, adjudication attempt/proposal/review/
                                                correction and embedding resolution/qualification
                                                records are retained authority and are never deleted here
never SET on any Episode
  Episode.{episode_digest_version, digest,
           origin_role, lineage_digest}        version-1 rows stay byte-identical; verify, retry,
                                                journal replay, backup/restore and rebuild dispatch
                                                on the stored version and migrate nothing (§1)
DELETE allowed in derived/authority-adjacent state
  gc --derived                                 derived output of retired generations (§4)
  gc --embedding                               retired embedding property + index (§4)
  gc --objects                                 unreferenced Payload files (§9)
```

Integrity is checked by `verify` (every Episode digest recomputed under that
row's own `episode_digest_version`, with an absent value verified by the frozen
version-1 serializer and no SET of `episode_digest_version`, `digest`,
`origin_role` or `lineage_digest` on any Episode + Payload existence and
hash + bounded Fact authority/list↔link agreement + Hit ledger ↔ cache replay
agreement + utility sufficient-statistic replay + policy event/cache agreement
+ unexpired receipt/feedback exact-once constraints and captured attribution
+ invalidation evidence/marker replay and generation validity preservation
+ EchoLineage existence for version-2 Episodes only, no lineage row required or
  materialized for a version-1 Episode, digest agreement with its Episode, bounded parent/root
  arrays, `context_digests` equal to the parent receipts' stored
  `selection_digest` values, Fact lineage-copy agreement, and each synthesis
  reproducing the bounded support-union rule (§3.3)
+ adjudication attempt/proposal/review/consumption/correction chain integrity,
  every proposal's `source_head_revision_key`, `policy_revision` and
  `candidate_digest` equal to its attempt's, one terminal review per proposal,
  no consumption without an ACCEPTED proposal, and complete correction and
  translation maps for every activated generation
+ embedding work/attempt/coverage agreement: one terminal attempt row per
  attempt, coverage cursor equal to the contiguous terminal prefix, recorded
  omission digest matching the resolved-no-vector set, and every resolution
  and qualification record present
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
         image digest plus committed Payload hash manifest; record the highest
         stored `episode_digest_version` in use so a restore verifies each
         Episode under its own version and rewrites none; record the active
         embedding_profile_id with its model and index IDs, each model-scoped
         coverage cursor with its health and omission digest, and the active
         extraction generation's fact_language_policy, grouping_version and
         judge_profile_id, so a restore can prove it came back on the same
         contract;
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
