# 01 — Storage

All graph data lives in one Neo4j (Community 5.26+, Docker, localhost-only).
There are three memory layers plus a control ledger. Each permits a different
kind of write.

```text
  originals  CREATE-only. No update, no delete. Loss here is irreversible
             Episode · Payload (metadata) · Hit
             links: HAS_PAYLOAD · HIT_OF · Episode→Episode INVALIDATES (revision)
  derived    CREATE-only nodes and links inside an extraction generation (§4)
             Fact · Entity · derived links · Fact embedding vectors
  caches     SET allowed. Can be dropped and rebuilt at any time
             hit/utility cache · Entity witness properties · session topology
             Episode room/space · REPLIES_TO · ConductingArc · ConductingArcCoverage
  control    append-only Usage and UsageOutcome, policy events,
             Episode lineage properties, identity decisions
```

Embedding, extraction and materialization bookkeeping is not in the graph. It
lives in `embedding-state.json`, `extraction-state.json` and
`materialization-state.json` under the runtime root (§6).

Payload bytes live on the filesystem in `objects/`. Authority = Neo4j +
`objects/`, nothing else (§9).

## 1. Element

Every memory element carries the common `:Element` label plus exactly one
kind label. Global queries (fulltext, time cut, audit) go through `:Element`;
kind-specific queries go through the kind label.

```text
(:Element:Episode)    original. A user message or a document revision. Has an event time
(:Element:Fact)       derived statement. Time = "when the claim took effect"
(:Element:Entity)     anchor for a person, thing or concept. No event time
(:Element:Community)  reserved kind label. Nothing writes it (docs/09)
```

### Common properties

| Property | Kinds | Meaning |
|---|---|---|
| `id` | all | UUIDv7. Never a primary ranking signal; only a final deterministic tie-break. Truncation uses `id DESC` after semantic keys to avoid an old-first bias; result order uses `id ASC` |
| `schema` | all | `anamnesis.<kind>/<n>`. The only notion of "type" |
| `content` | all | UTF-8 normalized natural language; Episode ≤ 64 KiB, derived element ≤ 8 KiB |
| `m0` | all | Intrinsic mass in [0, 1]. Assigned once at creation, immutable (docs/04 §1) |
| `properties` | all | Schema-specific canonical JSON, encoded size ≤ 64 KiB |
| `time_value`, `time_utc`, `time_precision` | Episode, Fact | Event time (docs/03 §1) |
| `origin_source/session/actor/record` | Episode | Source identification |
| `room`, `space` | Episode | Recomputable cache properties outside `properties` and the digest. Room = string `properties.room`, otherwise `origin_session`; space = explicit room assignment, otherwise `unknown:<origin_source>` (D58) |
| `origin_role` | Episode | `user \| assistant \| tool \| document \| operator`, supplied by the authenticated adapter. Immutable; extraction never infers it from prose (D49) |
| `lineage_mode`, `parent_recall_ids`, `context_digests`, `root_episode_ids`, `echo_depth`, `lineage_complete`, `lineage_digest` | Episode | Bounded lineage body and its canonical SHA-256, stored on the Episode in the same transaction; the digest is part of version-2 Episode identity (§3.3, §1 *Episode digest versions*) |
| `session_key` | Episode | `sha256(origin_source, origin_session)`; namespaces session order across adapters |
| `origin_key` | Episode | **Logical** source identity (`sha256(source, session, actor, record)`). Indexed, **not unique** — every revision of the same document shares it |
| `source_revision` | Episode | Opaque adapter-issued token, stable across retries and unique for each revision of one `origin_key` |
| `revision_key` | Episode | `sha256(origin_key, source_revision)`. **Unique.** Identity of one revision occurrence, including A→B→A reverts |
| `previous_revision_key` | Episode | Explicit predecessor for a revision; null only for the first occurrence. The origin head it is compared against is the newest Episode of the `origin_key` by `ingest_seq`, resolved by the `episode_origin_head` composite index inside the writer-fenced remember transaction; no separate head record exists |
| `ingest_seq` | Episode | Globally monotonic integer allocated in the remember transaction, last of the statements that do not depend on it; unique build/catch-up cursor. Gapless: an aborted remember consumes no number |
| `ingested_at` | Episode | Server ms, written once at CREATE. **Not used in snapshot computation** — audit only (docs/03 §1) |
| `payload_hash` | Episode | Payload reference (optional) |
| `episode_digest_version` | Episode | Server-selected immutable digest discriminator. Absent on every Episode stored before D49 implementation, which means version 1; `2` on every Episode admitted afterwards; any other stored value is `unsupported_digest_version`. The caller never supplies or downgrades it (*Episode digest versions* below) |
| `digest` | Episode | SHA-256 for integrity and retry conflict detection, computed by the row's own version. Version 1 hashes the frozen insertion-ordered body `{schema, content, properties, time, payload_hash, previous_revision_key}`; version 2 hashes the RFC-8785 body `{episode_digest_version, schema, content, properties, time, payload_hash, previous_revision_key, origin_role, lineage_digest}`. Byte-identical to the version-2 body in docs/02 §3; under one `revision_key`, a changed version-1 field, or a changed version-2 role or lineage body, is `revision_conflict` |
| `generation` | Fact, Entity, derived links | Immutable owning generation (§4) |
| `idem_key` | Fact | SHA-256 of the full canonical Fact identity below. Fact creation also uses the unique `fact_identity` key `(generation, meaning_digest, primary_episode_id)` with `MERGE` |
| `entity_key` | Entity | `sha256(generation, normalized_name, entity_kind)`. Unique; create-new Entity retries are no-ops |
| `witness_generation`, `witness_policy_revision` | Entity | Indexed generation and policy-revision witness marker, written with an allowed mention (§3.2) |
| `source_episode_ids` | Fact | Sorted immutable list of 1–16 original Episode IDs used for mass and Hit attribution |
| `primary_episode_id` | non-synthesis Fact | Episode whose extraction/correction created this Fact; indexed for bounded session recall |
| `source_count_total`, `sources_truncated` | Fact | Audit fields recording the authority-set size before its deterministic cap |
| `max_source_ingest_seq` | Fact | Maximum ingest sequence in `source_episode_ids` |
| `support_fact_ids` | synthesis Fact | Reserved claim field. No synthesis writer exists (docs/09) |
| `sub_kind` | Fact | `fact / state / event / preference / procedure / decision / summary`. Input to the forgetting prior (docs/04 §1) |
| `content_language` | Fact | `^[a-z]{2,8}(-[a-z0-9]{1,8})*$`, at most 35 ASCII characters, or `mul` for materially multilingual prose, or `und` for nonlinguistic/insufficient evidence. Required, immutable, meaning-bearing (D48) |
| `speaker_key`, `subject_keys` | Fact | `sha256(RFC-8785 {origin_source, origin_actor})` or null; 1–16 sorted resolved Entity IDs or null (D49) |
| `predicate_text`, `scope`, `scope_complete` | Fact | NFC case-preserving source-language relation (1–256 scalars), the closed bounded scope object below, and whether every scope component resolved. Meaning-bearing (D49) |
| `echo_state`, `echo_of_element_id`, `echo_depth`, `echo_lineage_truncated` | Fact | `direct \| known_echo \| context_derived \| unknown`, the exact delivered result a known echo repeats, depth 0..8, and whether a bound truncated the lineage (§3.3) |
| `parent_recall_ids`, `corroboration_root_episode_ids` | Fact | Sorted immutable 0..4 parent Usage IDs and 0..16 root Episode IDs copied from the Episode's lineage row. Provenance accounting only; never authority, candidates or a score term |
| `modality` | every Fact, including synthesis | `asserted / reported / hedged / intended / hypothetical` — the speech act of this content (docs/02 §5.1). Required, meaning-bearing: part of identity, input to the forgetting prior, returned on recall |
| `confidence` | Fact | Judge's belief in [0,1] that the claim is what the source says. Stored, input to `m₀`, **not** part of identity — model nondeterminism on a scalar must not fork Facts |
| `span` | primary `DERIVED_FROM` link (Fact → Episode) | `[start, end)` UTF-8 byte offsets into the Episode's content the claim rests on; validated at write, optional when the claim has no single locus (docs/02 §5 W2) |

### Episode digest versions (D49)

**Implemented compatibility exception (G004 recovery):** the existing Engine,
journal/import and metadata-free RPC APIs remain pre-lineage/unknown, not v2.
Stored `digest_format=null` retains insertion-order bytes; stored
`rfc8785-v1` retains its canonical pre-lineage bytes. Only explicit authenticated
role/lineage admission creates `episode_digest_version=2` with the distinct
`episode-rfc8785-v2` format. This implementation supersedes the all-new-v2 and
insertion-only assumptions below; no stored original is rewritten. See the
[implemented custody, retry and compatibility contract](../app/anamnesis/episode-lineage.md).

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
`origin_role`, `lineage_digest` or lineage properties in place. A legacy row
without authenticated lineage stays legacy/unknown under §3.3; only a new
source revision can carry version-2 lineage. Digest version is not an
eligibility, policy, candidate or ranking input, and recognizing it changes no
Episode ID, revision chain, payload, event time or snapshot result.

### Schema registry

| schema | Label | Content |
|---|---|---|
| `anamnesis.original-message/1` | Episode | Conversation message |
| `anamnesis.original-document/1` | Episode | Document or file. One Episode per revision |
| `anamnesis.claim/1` | Fact | An extracted claim. Invalidation events are claims too — the only special thing about them is an outgoing INVALIDATES edge |
| `anamnesis.mapping/1` | Fact | Registered ID. No writer |
| `anamnesis.synthesis/1` | Fact | Registered ID. No writer (docs/09) |
| `anamnesis.entity/1` | Entity | Anchor |
| `anamnesis.community/1` | Community | Registered ID. No writer (docs/09) |

`remember` accepts only the two Episode schemas. A stored
`anamnesis.memory-policy/1` Element from before the policy rewrite is legacy
data: startup counts it and never bootstraps a fresh policy authority over it
(§3.2).

### Contract: every Fact has bounded Episode authority

Every Fact materializes a non-empty authority set of original Episodes:

```text
  authority candidates
    extracted Fact     = [input Episode]
    replacement Fact   = [correction Episode] (reserved)
                       + authority(replaced Fact)

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
replay or a SET of `m0` (D42, D47).

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
  modality       = the required D42 enum
```

Unresolved subjects or predicates, unknown units, free qualifiers that do not
fit the closed object, and unresolved reported speakers all force
`scope_complete=false`. Validators enforce shape, bounds, sorting, decimal
canonicalization and ID resolvability. They assert no semantic equivalence.
No grouping key is computed from these fields; occurrence grouping is a
docs/09 target.

### Occurrence provenance (D46)

Semantic duplication never suppresses an occurrence's extracted assertion.
Each source occurrence gets its own immutable Fact with direct Episode
authority, modality, time and optional span, even when another Fact says the
same thing. Repetition adds no confidence and merges nothing. Exact retries within one occurrence collide on
the Fact identity key. Recall does not group occurrences
([09-roadmap](09-roadmap.md)).

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
  A node without a file is a missing payload and fails integrity checks.
- Because the file always exists before any transaction that references it,
  a backup can copy the exact hash manifest after taking the offline Neo4j
  dump (§9).

## 3. Hit ledger — attached to Episodes

Only explicit outcomes create Hits. The credit rule is docs/04 §5.

Hit stores id, episode_id, usage_id, t, kind, credit and idem_key. HIT_OF links
to the Episode. HitCache stores s, t_last_hit, hit_count, u_anchor and u_at.

### 3.1 Usage and UsageOutcome (D62)

Each successful recall, including an empty result, creates a Usage and returns
its server-issued usage_id. Usage stores the ordered {id,sources} selection,
created_at, policy_revision, lineage rows and their selection_digest, and the
connection binding. A Usage has no TTL or copy of the response or query.

`report_outcome` creates one UsageOutcome and its Episode Hits atomically.
Its admission rules and error codes are docs/04 §6.

### 3.2 Suppression policy authority (D43)

Policy is an authenticated command. Text found in ingested data is never
executed as policy.

`policy.set {policy_id, selector, scope: "content"}` and
`policy.revoke {policy_id}` append immutable `PolicyEvent` nodes (unique
`revision` and `key`) and advance `Meta.policy_revision` in the same
transaction. The evaluator is `episode-source-v1`, recorded on
`(:PolicyAuthority {key:'installation'})`. A selector names an exact
`episode_id`, an `origin_source`, or both (ANDed). The schema accepts no
other selector field and no other scope, and no payload is scanned. A policy
ID identifies one immutable deny: repeating the same command is a no-op, and
a revoked deny needs a new ID to be reissued. The active set is the fold of
the events in revision order.

Suppression is a hard filter. A denied Episode is not returned, and it can't
support a visible derived result: a Fact is dropped when any of its
`DERIVED_FROM` source Episodes is denied. The same check runs in extraction
before text leaves the database, in recall candidates, companions and
provenance text, in projection, in embedding recovery and status, and in
`report_outcome`, which refuses with `policy_denied` when a credited source
is now denied. Historical `T` never bypasses current policy.

Entity anchors need an allowed witness. The witness is the indexed
`Entity.{witness_generation, witness_policy_revision}` marker set when a
validated Fact mentions it. Cutover checks the marker for each distinct Entity
the generation's Facts mention, at the pinned generation and policy revision;
an absent or mismatched marker blocks that cutover.

Policy publication and response publication share one Meta write barrier
(docs/02 §1). Missing or incompatible policy authority is never overwritten:
only a database with no Elements, no `PolicyEvent` and no legacy
`anamnesis.memory-policy/1` Element bootstraps revision zero. With the
authority unavailable, requests fail with `policy_unavailable`.

Suppression is not deletion. Originals stay on disk, existing backups keep
them, and privileged raw operator access is outside policy. There is no erase
operation. Revocation removes the deny; it doesn't re-extract what was skipped
while the deny was active.

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
  (:Episode {lineage_mode, parent_recall_ids[0..4],
             context_digests[0..4], root_episode_ids[0..16],
             echo_depth: 0..8, lineage_complete, lineage_digest})
```

The daemon verifies the caller/client binding and writes these properties
atomically on the Episode. `context_digests` copies the parent Usage nodes’ stored
`selection_digest` values, so lineage never depends on recomputing an old
selection: each Usage persists
`selection_digest = sha256(RFC-8785(ordered array of at most 64 delivered
{element_id, root_episode_ids, echo_depth, complete} records))` at creation
(§3.1). Parent and digest arrays have equal length; each digest is paired with
its parent ID, then sorted by recall ID. Roots are the sorted distinct union of
the referenced snapshots' roots, and depth is `1 + max(item.echo_depth)`.
`lineage_complete=true` only when every referenced item is complete and neither cap
truncates the result. Parent Usage nodes must still exist when `remember`
validates them; later parent loss cannot change the copied lineage. The
canonical body digest (`lineage_digest`) is part of the **version-2** Episode
digest: an exact retry is a no-op and a changed body under one revision key is
`revision_conflict`. Lineage properties exist only for version-2 Episodes; an
Episode stored under version 1 never gains them in place, stays legacy/unknown
here, and only a new source revision can carry version-2 lineage (§1).

Each Fact copies the bounded metadata in §1. Fact identity is the unique
`(generation, meaning_digest, primary_episode_id)` tuple (`fact_identity`),
admitted with `MERGE`, so replay does not require a custody node. A known echo
copies the exact
delivered item's roots and stores `1 + item.echo_depth`; a context-derived
Fact copies the Episode's root union and depth. Neither adds the assistant
Episode as another root. A union over 16 roots keeps its first 16 IDs in
bytewise order and computed depth over 8 stores 8; either overflow sets
`complete=false`, `echo_state=unknown` and `echo_lineage_truncated=true`, and
no omitted ancestor is treated as a new root. With complete parent-Usage lineage,
`known_echo` requires an L4 `DUPLICATE_OCCURRENCE` against an exact delivered
result ID whose roots and depth agree byte-for-byte across every parent
Usage that contains it; any other Fact from that Episode is
`context_derived`, and a disagreement is `unknown`. Historical assistant
input without authenticated lineage metadata is `unknown`, never presumed
independent.

Storage still preserves the echoed Fact, its actor, time, modality and its own
Episode provenance. Occurrence count and root count never alter confidence,
`m0`, mass, utility or rank, and neither can elect a conflict
winner. The gate covers the source Episode itself: an assistant Episode with
unknown or incomplete lineage, and every output derived from it, is stored but
ineligible for semantic candidates and for invalidation
(`echo_lineage_unavailable`). Only a new source revision with complete bounded metadata changes that
state. A later user-authored statement
is a new direct occurrence; reporting an outcome is usage evidence, not
semantic corroboration. Lineage never supplies or changes Fact event time,
snapshot visibility, validity, Hit attribution or source authority, and no
online operation traverses parents: replay reads the materialized Episode
properties and policy checks at most 16 roots.

## 4. Derived layer and generations

The derived layer is one extraction stream (Fact, Entity, MENTIONS,
RELATES_TO, DERIVED_FROM, Fact→Fact INVALIDATES, CONTRASTS) plus Fact
embedding vectors. No community stream exists
([09-roadmap](09-roadmap.md)).

### Originals-layer links have no generation

`HAS_PAYLOAD`, `HIT_OF` and `Episode → Episode INVALIDATES` are created by
`remember` and `report_outcome`, never by a pipeline. They carry no
`generation` and are CREATE-only like their endpoints.

### Extraction generation lifecycle

Every derived element and derived link carries one `generation`. Nodes and
links are immutable once written.

```text
  (:ExtractionGeneration {id, state, covered_ingest_seq, body})
      state = active | catching_up | cutover | retired
  (:ExtractionCoverage {key, generation_id, partition, covered_ingest_seq, body})
      partition = episodes | active_extraction
  (:Meta {key:'extraction_selector', generation_id, selector_version})
```

Recall, projection and the relation judge read only the selected generation.
A generation pins its extraction configuration, including
`fact_language_policy ∈ {source, en}` (D48). Changing that configuration
means a new generation, never an in-place rewrite. Because `generation` is
part of Fact, Entity and link identity, the same claim in two generations is
two rows, and only the selected one serves.

Coverage is the only cursor. The scheduler reads Episodes above
`covered_ingest_seq` in ingest order and seals each source's terminal outcome
into coverage: a materialized pipeline, or a content-free omission for a
failed or cancelled stage. Coverage moves forward only.

Cutover is one transaction under the Meta writer fence. It requires a
`catching_up` or already `active` target, caught-up coverage, the ordered
access indexes ONLINE (`ordered_probe_unavailable` otherwise),
`Meta.conducting_arc_ready` with COMPLETE ConductingArc coverage
(`degree_probe_unavailable` otherwise), the Entity witness check of §3.2 and,
when an embedding provider is configured, a vector for every Fact of the
target generation. A generation with zero Facts satisfies the vector
requirement. Cutover then marks the target `active`, points the selector at
it and increments `selector_version`.

Rollback reopens a `retired` generation as `catching_up` with a fresh source
high watermark. It serves again only after it catches up and crosses the same
cutover.

Generation admission, cutover and rollback are Engine and Store operations
that the scheduler drives. No RPC exposes them and nothing garbage-collects a
retired generation. The pipeline contract is in
[extraction-audit](../app/anamnesis/extraction-audit.md) and docs/02 §5.

### The embedding stream

Only Facts are embedded (D54, D56). A vector is an `EmbeddingVector` node,
also labelled `Embedding_<profileId>`, with `fact_id`, `profile_id`,
`input_revision`, `input_digest`, `operation_id` and `vector`. `profileId`
is the SHA-256 of the canonical profile JSON
(`packages/core/src/embedding.ts`) and is stored as
`(:EmbeddingProfile {id, body})`. Each profile has its own cosine index,
`vec_fact_<profileId>` over `(v:Embedding_<profileId>) ON (v.vector)`, so two
profiles can't overwrite one another. On startup a vector index over that
label under any other name is dropped and recreated under this name.

The runtime sets the profile's `model_incarnation` to
`sha256([baseUrl, model, dimensions])`. That is a configuration fingerprint,
not proof of frozen weights. The provider must echo `model` and
`model_incarnation` or the call fails with `profile_mismatch`.

The lane scans every generation for Facts with no vector under the configured
profile. Its attempt and quarantine state is `embedding-state.json`, not the
graph. An attempt ends `succeeded`, `quarantined` or `deferred`, with reason
`provider_unavailable`, `provider_unavailable_exhausted`,
`provider_rejected`, `profile_mismatch`, `invalid_vector`,
`input_too_large` or `stale_input`. Only `provider_unavailable` defers; the
ninth transient failure in a row quarantines as
`provider_unavailable_exhausted`. An input is never truncated, chunked or
zero-filled to produce a vector.

Recovery is authenticated and explicit:
`embedding.recover {operation_id, fact_id}`,
`embedding.requeue {limit ≤ 1000, reasons?}` and
`embedding.status {operation_id}`. The provider wire format, backoff and
idempotency rules are in
[embedding-recall](../app/anamnesis/embedding-recall.md). The dated probe of
one local model is in
[research/embedding-runtime](research/embedding-runtime.md). Entity and
relationship vectors don't exist ([09-roadmap](09-roadmap.md)).

### Markdown projection (G10, #268)

The projection is a read-only, human-readable view of the extraction stream:
every Entity of the writable generation that has at least one VALID Fact at
T, with those Facts listed under it. It never writes to Neo4j. A Fact is
valid exactly as recall's `FACT_VALID` predicate says (`known_by = T`, no
effective known INVALIDATES, the Fact not itself invalidated) and timed no
later than T (`time_utc` null or `<= T`), and it's dropped when any of its DERIVED_FROM source Episodes is selected by an active
`content` deny policy, using the same selector match as recall
(`selector.episode_id` against the Episode id, `selector.source` against
`origin_source`). Facts are ordered by `time_utc` DESC then id ASC; Entities
by name ASC then id ASC. The daemon serves it as the `projection {T?}` RPC
(`capabilities.projection: true`) through `ProjectionStore.snapshotTx`.

`anamnesis-ops projection write <dir>` regenerates the files. One markdown
file per Entity, named `<slug(name)>-<last 12 hex of id>.md` where the
slug lowercases the name and collapses every run outside `[a-z0-9]` into
`-`, trimmed; the suffix is the UUIDv7 random tail, which keeps names unique
(the leading hex of a UUIDv7 is a timestamp shared by ids minted in the same
minute, so it would not). The body is
`# <name>`, a blank line, then one `- <content> (<time value or undated>)
[<fact id>]` line per Fact, then a blank line and the footer
`<!-- anamnesis projection; generated from graph at T=<T>; do not edit -->`.
T is the request T (`latest` when none is sent; the command sends none), not
the wall clock, so two runs over an unchanged graph produce byte-identical
files; `generated_at` is reported on stdout only. Files in `<dir>` matching
`*-<12 hex>.md` that no longer correspond to a projected Entity are removed;
other files are left alone. The command prints two JSON lines:
`{generated_at}` then `{entities, facts, written, removed, dir}`.

The projection is an output, never an input: the daemon doesn't read the
directory back, nothing in the graph references it, and editing a file
changes nothing until the next write overwrites it.

### Identity layer (D59)

The identity layer answers which speaker accounts are the same person. It is
not extraction output and adds no Element kind. Its nodes never conduct PPR,
recall does not read them in this version, and `OF_PERSON` is not a Link role.

```text
  Account          {key, source, actor, names, mine, episodes, tool_episodes,
                    sample_first, sample_last}                         cache, unique key
  AccountSession   {account_key, session_key, session, episodes, first_utc, last_utc}
                                                                      cache, unique pair
  DirectoryEntry   {source, actor, names, handles, emails}             operator table
  IdentityDecision {id, a, b, verdict, confidence, authority, reason,
                    model, evidence_digest, decided_at}                CREATE-only control
  IdentityState    {key:'identity', covered_ingest_seq, generation, built_digest}
  Person           {id, generation, name, owner, aliases, sources}     one generation
  (:Account)-[:OF_PERSON {generation}]->(:Person)
```

`Account.key = tupleHash([origin_source, origin_actor])`. The identity lane
(docs/02 §5.5) reads Episodes above `covered_ingest_seq` in ingest order, at
most 500 per page, and folds them into Accounts and AccountSessions in the
transaction that advances coverage. Names are distinct
`properties.speaker_name` strings (NFC, at most 256 UTF-8 bytes, first 32
kept). `mine` records `properties.mine === true`. `sample_first` and
`sample_last` keep the first four and latest four Episodes whose content has
more than five characters. At every plan an account is the owner's when
`mine` holds, actor is `user`, or `source:actor` appears in
`ANAMNESIS_IDENTITY_OWNER_ACCOUNTS`; it is a bot when actor is `assistant` or
more than half its Episodes have `origin_role = tool`.

Candidates share an in-memory blocking key from a name, directory name or
handle (NFKC, lower case, whitespace, punctuation and symbols removed), or an
exact lower-cased directory email. Bracketed and outside name parts are keys
of their own; keys shorter than two characters are dropped. A key shared by
more than 12 accounts is skipped. Bots never pair.

Rules are recomputed at every plan and never stored: two owner accounts are
`same`; a shared directory email is `same`; two accounts with overlapping
activity intervals in one session are `different`. Other pairs go to the
judge when configured. Stored decisions are CREATE-only and have authority
`judge` or `operator` only. Their `evidence_digest` is SHA-256 over sorted
names, session keys and directory rows. A pair is judged again only when its
latest verdict is `unsure` and that digest changed. The latest operator
decision overrides rules and judge decisions.

Grouping applies cannot-links before merge edges. Operator decisions have
highest precedence, then rules, then judge decisions. Judge `same` needs
confidence at least 0.85; judge `different` needs at least 0.5. An edge joins
two groups only when no cannot-link connects them. Person id is UUIDv5 of the
smallest member key; its name is the name with the greatest Episode-weighted
use, ties by code point order.

A build writes every Person and `OF_PERSON` of generation `g + 1` and moves
`IdentityState.generation` in the same transaction; a following transaction
deletes old generations. `built_digest` is SHA-256 of the planned persons
(ids, names, members, aliases and sources). Account and Episode counts are
read through `OF_PERSON`, not stored on Person.

The scan reads Episodes regardless of policy because accounts and decisions
are operator data. Episode text leaves the database for judging only after
the current extraction policy check; denied samples are dropped. Account,
AccountSession and Person regenerate from Episodes. `IdentityDecision` and
`DirectoryEntry` remain as regeneration authority.

Operator surface:

```sh
node dist/anamnesis-ops.mjs identity status
node dist/anamnesis-ops.mjs identity rebuild [--accounts]
node dist/anamnesis-ops.mjs identity correct slack:U1 kakao:42 same|different [--reason TEXT]
node dist/anamnesis-ops.mjs identity directory users.jsonl
node dist/anamnesis-ops.mjs people list [--query TEXT]
node dist/anamnesis-ops.mjs people show <person-id|source:actor>
```

Directory JSONL rows hold `source`, `actor` and optional `names`, `handles`
and `emails`; a row replaces that account's entry. Linking extracted Entities
to Persons, adding Person names to extraction, and Person recall are later work.

## 5. Link

Relationships are real Neo4j relationship types, not nodes. The eight roles
are closed; D58 adds the structural `REPLIES_TO` cache. Semantic variety is
absorbed by the natural language in `RELATES_TO.content`.

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

PPR transition strength is a per-role weight, default 1, and the solver
rejects an arc that carries its own weight (docs/06 §4). The protocol's
`MemoryLink.weight` field is not read by the solver. D24's promise that links
never carry a weight was released on 2026-10-23
([10-decision-log](10-decision-log.md#d24--role-weights-only-each-retained-row-normalizes-itself)).

Every INVALIDATES relationship copies `target_id` and the source's
`effective_time_utc`. Fact invalidation also carries `generation`. These
immutable fields support a relationship-index seek for `valid(T)` without
expanding all incoming invalidators. One Fact may create at most eight
outgoing INVALIDATES links.

Recall reads at most four `CONTRASTS` peers per Fact under a current-policy
check (docs/05 §6). No conflict adjacency cache and no conflict count exist.

| Role | Direction | Layer | Meaning | Conducts PPR |
|---|---|---|---|---|
| `NEXT_EPISODE` | Episode → next Episode | cache | Rebuildable same-session total order. Rewired on backfill | yes |
| `REPLIES_TO` | replying Episode → answered Episode | cache | Explicit reply address or Slack thread parent; no generation or content in its key | no |
| `MENTIONS` | Episode\|Fact → Entity | derived | What it is about | yes |
| `RELATES_TO` | Fact\|Entity ↔ Fact\|Entity | derived | Free natural-language relation | yes |
| `HAS_MEMBER` | Community → Entity\|Fact | derived | In the lattice. Nothing writes it (docs/09) | yes |
| `DERIVED_FROM` | Fact → Episode\|Fact | derived | Provenance chain. Provenance is snapshot-exempt, **never policy-exempt** (docs/03 §3) | yes |
| `INVALIDATES` | Fact → Fact / Episode → Episode | derived / originals | The target is not valid from this event's time onward | no |
| `CONTRASTS` | Fact ↔ Fact | derived | An unresolved contradiction, preserved | no |

PPR uses the five conducting roles **bidirectionally**. The stored direction is
for the semantic model.

### Episode room, space and replies (D58)

`RoomAssignment {source, room, space, name?}` is an explicit daemon-managed
control table in Neo4j, unique on `(source, room)`. It is not an Element.
The client chooses labels; the engine never infers a project from `cwd`.
Import upserts entries (last line wins); omitted rooms retain their assignments,
and omitting `name` clears that optional display name. Labels are written by
the same function at admission and rebuild, in the Episode creation transaction
for new admissions. Exact remember retries do not relabel old originals.

`properties.reply` addresses `<source>:<room>#<message>` by
`(origin_source, origin_session, origin_record)`. Slack thread replies with
session `<room>/<thread>` additionally address `(slack, room, thread)`.
If revisions or actors share an address, the greatest `ingest_seq` wins.
Self-links are excluded. Absent targets increment `missing` per address;
they do not fail admission or rebuild. A later rebuild resolves late targets
and replaces a prior target when a newer revision arrived. Duplicate addresses
and duplicate resolved endpoints collapse to one link.

`REPLIES_TO.idem_key = sha256(JSON.stringify([from, to, "REPLIES_TO"]))`;
its ID is UUIDv5 of that key in the URL namespace. It carries fixed descriptive
content and no generation. Rebuild only deletes stale `REPLIES_TO` links from
its page. Neither labels nor replies change Episode properties JSON, digests,
identity, lineage, NEXT_EPISODE, conducting rows, serving revisions or recall.

Operator surface (local or the existing authenticated TCP transport):

```sh
node dist/anamnesis-ops.mjs spaces import rooms.jsonl
node dist/anamnesis-ops.mjs spaces rebuild
node dist/anamnesis-ops.mjs replies rebuild
node dist/anamnesis-ops.mjs spaces list
node dist/anamnesis-ops.mjs spaces rebuild --source slack --room C123
node dist/anamnesis-ops.mjs replies rebuild --after 500 --through 1600000 --limit 500
```

JSONL rows require `source`, `room`, `space`, optionally `name` (absent or
`null` both mean no name); other keys are discarded. Records end with LF only,
so a name may hold U+2028 / U+2029 unescaped. CLI import streams batches of
100; each RPC transaction accepts up to 1,000 entries. A failed later batch leaves earlier batches committed; replaying
the file is safe. Import changes the table, not existing Episode labels: follow
it with rebuild. `spaces.list` reports labelled Episode counts, distinct
`(source, room)` counts and first/last event time, ordered and paged by space.
Run the initial labels rebuild before relying on listing or room-scoped rebuild.

RPCs are `spaces.import`, `spaces.list`, `spaces.rebuild`, `replies.rebuild`.
Each rebuild RPC commits one page, default 500 / maximum 1,000 Episodes, and
returns `{after, through, done, scanned, updated, created, removed, missing}`.
`through` is the initial ingest high-water mark; the CLI retains it and prints
each committed cursor. Resume with the last printed `--after` and `--through`,
and the same room filter. A lost response can replay its page safely. Page
counts describe that invocation, not a persistent job total. New admissions
above the high-water mark already receive labels and available replies.
Rerun replies from zero to repair targets arriving after an earlier pass.

Initial backfill uses the unique `Episode.ingest_seq` range index. Subsequent
room-scoped work uses `(origin_source, room, ingest_seq)` and touches only that
room, including its Slack threads. Target resolution uses
`(origin_source, origin_session, origin_record, ingest_seq)` with an ordered
`LIMIT 1`. Import and rebuild share the existing Meta writer fence. If an
assignment changes during a pass, rerun that room after import completes.
Listing uses the `Episode.space` index; aggregation reads the labelled rows.

The ordinary Neo4j dump retains the assignment table, properties and reply
relationships, with no new filesystem member or digest format. Integrity
decodes originals independently of these cache fields. The authority snapshot's
`physical_links` remains the conducting-only inventory; dump checksums cover
these additional stored bytes. After a cache loss, rebuild labels then replies.

### ConductingArc — bounded physical access cache

The mandatory rebuildable cache is a node, not another semantic relationship:

```text
  (:ConductingArc {source_id, link_id, peer_id, role,
                   generation?, source_extraction_generation?})
  (:ConductingArcCoverage {stream, generation, state})
      stream = cache | extraction; cache generation = 0
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
NEXT_EPISODE. Rows include hidden, inactive and retired
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
probe slots.

Every physical link CREATE/DELETE, NEXT_EPISODE topology rewire and generation
GC creates/deletes its endpoint rows in the **same transaction**, including
hidden-generation writes.
Generation opening publishes COMPLETE empty coverage atomically before its
first link append; subsequent appends preserve complete coverage atomically.
Generation cutover requires complete coverage, and GC removes a partition's
coverage record only after its last physical link and cache row are gone.

Maintenance/restore reconstructs ConductingArc solely from the retained
physical graph, not policy-filtered serving exports.
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
  Episode        --REPLIES_TO----> Episode
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
<runtime root>/               mode 0700 (ANAMNESIS_RUNTIME_ROOT)
├── anamnesis.sock            UDS, mode 0600 (docs/02 §10)
├── token                     installation token for hello, mode 0600
├── owner/                    ownership claim on this root
├── incarnation.json          data incarnation of this root
├── config.jsonc              backed up with the authority
├── neo4j.auth                Neo4j credentials, mode 0600
├── embedding-state.json      embedding attempt and quarantine ledger (D54)
├── extraction-state.json     extraction journal: tasks, leases, attempts, decisions (D55)
├── materialization-state.json  materialization custody and replay intent (D53)
├── objects/                  Payload bytes (§2). Part of the authority
├── uploads/                  in-progress object uploads
└── backup.state              journal of a running backup (§9)
```

The sibling paths `<runtime root>-operation.lock` and
`<runtime root>-restore.state` hold the backup/restore lock and the restore
activation journal, so they stay discoverable while the root is renamed (§9).
Install paths, units and environment are in [deploy](deploy.md).

The three state files hold pipeline bookkeeping, never memory. Each is a
single JSON document rewritten atomically (temporary name plus rename) on
every change and schema-validated on load. `embedding-state.json` keeps an
entry only while a Fact has a failed or deferred attempt and no vector.
`extraction-state.json` keeps one entry per pipeline and prunes it once
coverage has sealed the source and materialization is terminal, so the file
is bounded by in-flight work (docs/02 §5, D55).
`materialization-state.json` keeps operation custody and replay intent until
the source settles. None is part of the backup authority: losing one
re-drives unsealed work and forgets failure history. The structured audit log
lines are the durable record of what happened.

## 7. Indexes and constraints

The statements are `SCHEMA_STATEMENTS` in
`packages/core/src/store/schema.ts`; that file is the source of truth.

```text
unique    Element.id · Episode.revision_key · Episode.ingest_seq · Payload.hash · Meta.key
          Fact(generation, meaning_digest, primary_episode_id)   fact_identity
          ExtractionGeneration.id · ExtractionCoverage.key
          Usage.usage_id · UsageOutcome.usage_id · UsageOutcome.idempotency_key
          Hit.id · Hit.idem_key · HitCache.episode_id
          PolicyAuthority.key · PolicyEvent.revision · PolicyEvent.key
          EmbeddingVector.key · EmbeddingProfile.id
          <role>.idem_key for all eight roles
          <role>.id for the five conducting roles
          ConductingArc(source_id, link_id) · ConductingArcCoverage(stream, generation)
          Generation(stream, generation)                         conducting partition registry
          RoomAssignment(source, room)
          Account.key · AccountSession(account_key, session_key)
          DirectoryEntry(source, actor) · IdentityDecision.id · IdentityState.key
range     Episode.origin_key · Episode(origin_key, ingest_seq)   episode_origin_head
          Episode(session_key, time_utc, ingest_seq)
          Episode(origin_source, room, ingest_seq)
          Episode(origin_source, origin_session, origin_record, ingest_seq)
          Episode.space · Element.time_utc · Element.schema
          Fact(generation, id) · Entity(generation, entity_key)
          Entity(witness_generation, witness_policy_revision)
          Hit(episode_id, t, id)
          EmbeddingVector(fact_id, profile_id)
          ConductingArc.link_id
          INVALIDATES(target_id, effective_time_utc, id) · INVALIDATES.id
          AccountSession.account_key · Person(generation, id)
fulltext  element_content over Element.content, cjk analyzer
vector    vec_fact_<profileId> over (:Embedding_<profileId>).vector, one per profile (§4)
```

Startup also drops the constraints and indexes of removed mechanisms
(`hub_arc_link`, the six `adjudication_*` constraints, the four recall
receipt constraints and `receipt_expiry`). It refuses to become ready on a
store recorded before schema version 2 that still holds receipt-era rows or
an incompatible Hit: those rows are never converted or dropped (D62).

No extraction task, attempt, pipeline, judge-input or decision label exists in
the graph. That state is the `extraction-state.json` journal (§6, D55). Only
`ExtractionGeneration` and `ExtractionCoverage` remain as nodes, because
coverage is the cursor the scheduler resumes from.

The two UsageOutcome constraints enforce one verdict per Usage and one meaning
per idempotency key. The unique Hit key prevents duplicate source credit.

The ConductingArc uniqueness constraint's backing RANGE index is the ordered
access path. For `source_id` equality it supplies `link_id ASC` directly,
with LIMIT 256 before any filter or sort. Per-role id uniqueness supplies the
relationship indexes used by bounded link resolution. EXPLAIN/PROFILE fixtures
prove these access paths (docs/07). DegreeProbe counts only the captured rows:
fewer than 256 is exact under complete coverage, and 256 means saturated.

## 8. Immutability discipline

Neo4j Community has no database triggers. The discipline is two-fold.

1. **The daemon is the only write path.** Bolt is bound to 127.0.0.1, the
   password is per-install random, and only the daemon holds a connection
   (docs/02 §10).
2. **SET and DELETE stay inside the cache and cursor state below.** Anything
   else is rejected in review.

```text
SET allowed
  HitCache.{s, t_last_hit, hit_count, u_anchor, u_at}   hit and utility cache (docs/04)
  Episode.{room, space}                        recomputable labels (§5, D58)
  Entity.{witness_generation,
          witness_policy_revision}             witness marker, set with an allowed mention
  ConductingArcCoverage.state                  rebuild and publication barrier
  ExtractionGeneration.{state, covered_ingest_seq, body}
  ExtractionCoverage.{covered_ingest_seq, body}
  IdentityState, Account, AccountSession       identity lane caches (§4)
  Meta.{structure_revision, policy_revision, ingest_seq, schema_version,
        writer_epoch, conducting_arc_ready}    revisions, clock, fencing and cache gate
  Meta(extraction_selector).{generation_id, selector_version}
CREATE/DELETE allowed in caches
  session NEXT_EPISODE                         local rewire or topology rebuild
  REPLIES_TO                                   reply cache rebuild
  ConductingArc, ConductingArcCoverage         maintained with every physical link write
  Person, OF_PERSON                            one identity generation at a time
  EmbeddingVector index                        a wrongly named profile index is dropped and recreated
retained, append-only
  Usage, UsageOutcome, Hit, PolicyEvent, IdentityDecision, Episode lineage properties
never SET on any Episode
  Episode.{episode_digest_version, digest,
           origin_role, lineage_digest}        version-1 rows stay byte-identical; verify, retry,
                                                backup/restore and rebuild dispatch on the stored
                                                version and migrate nothing (§1)
```

Integrity checks recompute every Episode digest under that row's own
`episode_digest_version`. An absent value is verified by the frozen version-1
serializer, and no check sets `episode_digest_version`, `digest`,
`origin_role` or `lineage_digest`. The authority snapshot used by backup and
restore covers Element IDs, physical links, invalidation outcomes and Episode
digests (§9). `hit-cache.verify` and `hit-cache.rebuild` compare and replay
the Hit ledger against HitCache. `anamnesis-ops verify` is a runtime
admission health check, not a full database audit.

ConductingArc verification is a maintenance scan of the retained graph, not
a recall-time full-degree scan. Missing/extra/stale rows or false completeness
mark the affected coverage UNAVAILABLE and are reported; rebuild before PPR
serves again. Rebuild and verify must include parallel links, both endpoints,
hidden generations, topology rewires and retired-generation GC (§5).

## 9. Durability, backup, restore

### Authority

The data authority is exactly two things: the Neo4j database and
`objects/`. An Episode is accepted only when its Neo4j transaction commits
([02-daemon-and-pipelines](02-daemon-and-pipelines.md) §4).

### Write ordering

| Step | Guarantee |
|---|---|
| objects write | fsync both temps; rename data first; rename metadata sidecar last as commit marker; fsync directory. A hash is committed iff its valid sidecar and matching data both exist |
| Neo4j | its own WAL. Every remember, report_outcome and materialization is one transaction, so there is no partially applied state |

An interrupted upload leaves only temporary files, which count toward the
upload quota until they expire. Data without a committed sidecar is not a
committed object. No command garbage-collects committed objects.

### Backup

The manifest's authority evidence stores per-collection `{count, sha256}`
digests instead of inventories: Element IDs ordered by `e.id`, physical
`{id, from, to, role}` links ordered by `l.id`, invalidation
`{id, source_hash, outcome_hash}` evidence ordered by `l.id`, and Episode
`e.digest` values ordered by Episode `e.id`. Each SHA-256 covers the canonical
JSON array, including its brackets and commas; invalidation outcome hashes retain
the existing `extractionBodyDigest` derivation. Keyset pages of 5,000 rows are
streamed inside the same write transaction holding the policy lock and writer
fence. There is no authority collection size cap; the manifest keeps only counts,
digests, the small sorted unique retained-generation list and cutoff coverage.

Backup and restore share the atomic sibling
`~/.anamnesis-operation.lock/`. Backup refuses when
`~/.anamnesis-restore.state` exists; restore refuses while the live-root
`backup.state` is not cleared. A backup destination must not exist; there is no
overwrite/force mode—so a prior complete or partial archive is never reused.

```text
  anamnesis backup <destination>
    PREPARE
      1. acquire the operation lock; require a nonexistent destination, create
         it as 0700; write the discoverable live-root
         ~/.anamnesis/backup.state = {operation_id, destination, phase: PREPARE}
      2. daemon acquires the backup gate; briefly take the object lease, finish
         in-flight publications and writes
    CUTOFF
      3. under the write queue, pause every Neo4j writer (remember, recall Usage
         creation, report_outcome, extraction, embedding, identity and policy)
      4. record structure_revision, ingest_seq, schema/Neo4j versions and the
         policy_revision and exact live container
         image digest plus committed Payload hash manifest; record the highest
         stored `episode_digest_version` in use so a restore verifies each
         Episode under its own version and rewrites none; record the active
         embedding profile and the selected extraction generation's
         configuration, so a restore can prove it came back on the same
         contract;
         fsync backup.state=CUTOFF, then release the object lease. Upload may
         proceed; Neo4j writers remain paused
    DUMP
      5. fsync backup.state=STOPPING; stop the Neo4j service; fsync
         backup.state=DB_STOPPED
      6. run a one-shot admin container using the recorded **exact image
         digest** against the stopped volume: neo4j-admin database dump;
         fsync dump and backup.state=DUMPED
      7. restart Neo4j and pass health checks; fsync backup.state=DB_STARTED;
         only now resume writers
    COPY
      8. copy exactly each manifested object data+metadata sidecar, config.jsonc
         and neo4j.auth; write SHA-256 and size for every archive member
      9. verify dump metadata and every checksum
     10. fsync files and destination; atomically publish backup.complete last;
         fsync the destination parent; clear backup.state
```

Neo4j Community 5.26 requires the database to be offline for `database dump`.
Recall and remember are unavailable during stop/dump/start. Copying objects after restart is safe because the manifest is fixed
at the cutoff and object files are immutable.

The fixed live-root `backup.state` and destination operation state are atomic
temp→fsync→rename→directory-fsync journals. On CLI
or daemon restart, every state before DB_STARTED first inspects and, if
needed, restarts Neo4j before releasing the gate; DUMPED preserves the dump.
DB_STARTED/COPYING may resume copying from the manifest. Writers cannot
resume while the journal is CUTOFF, STOPPING or DB_STOPPED. No incomplete
destination contains `backup.complete`, whose body pins the manifest hash.

### Restore

After the restored database is loaded and started, the trusted adapter recomputes
the authority digests under the writer fence without running migrations or
initialization that could rewrite the restored graph. The recomputation reads
the database the restore loaded: the adapter starts an owner-labelled Neo4j on
`<root>/database` and records its endpoint in `<root>/authority.json`; it never
reads the fenced source container, whose data the restore does not touch.
Restore compares the snapshot, including retained generations and coverage,
with the manifest before source rebind or rollback removal. A difference
refuses with `authority_digest_mismatch`; opaque dump checksums alone do not
establish restored authority. A tree that fails after promotion is moved back
to the staging name, the rollback copy returns to the live name, and the
adapter's quarantine removes the container that served the rejected tree.
Quarantine runs even when that undo fails (`rollback_failed`); a removal that
fails for any reason other than the container already being gone is reported as
`quarantine_failed` beside the refusal, so a refused restore whose error names
either code is followed by `docker ps -a` for leftover owner-labelled containers.

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

The old root is preserved as rollback state. A failed
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

Caches are not part of what must be restored. `hit-cache.rebuild` replays
the Hit ledger, and ConductingArc is reconstructed from the restored physical
links and published complete before PPR is available (§5).
