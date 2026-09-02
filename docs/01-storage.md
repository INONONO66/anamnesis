# 01 — Storage

All graph data lives in one Neo4j (Community 5.26+, Docker, localhost-only).
Logically there are three layers, and each layer permits a different kind of
write.

```text
  originals  CREATE-only. No update, no delete. Loss here is irreversible
             Episode · Payload (metadata) · Hit
             originals-layer links: HAS_PAYLOAD · HIT_OF · Episode→Episode INVALIDATES (revision)
  derived    CREATE-only nodes/links inside a lifecycle-managed generation. Regenerable from originals
             Fact · Entity · Community · derived links · embedding
  caches     SET allowed. Can be dropped and regenerated at any time
             hit cache · session topology · HubArc · ProfileCache · m_cache · Outbox · OriginHead · selectors
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
| `ingest_seq` | Episode | Globally monotonic integer allocated in the remember transaction; unique build/catch-up cursor |
| `ingested_at` | Episode | Server ms, written once at CREATE. **Not used in snapshot computation** — audit and spool-drain ordering only (docs/03 §1) |
| `payload_hash` | Episode | Payload reference (optional) |
| `digest` | Episode | SHA-256 of canonical `{schema, content, properties, time, payload_hash, previous_revision_key}` for integrity and retry conflict detection |
| `generation` | Fact, Entity, Community, derived links | Immutable owning generation (§4) |
| `idem_key` | Fact | SHA-256 of the full canonical Fact identity below. Unique |
| `entity_key` | Entity | `sha256(generation, normalized_name, entity_kind)`. Unique; create-new Entity retries are no-ops |
| `visible_from_utc` | Entity, Community | O(1) historical visibility threshold. Entity is a cache; Community is fixed at build |
| `source_extraction_generation`, `source_covered_ingest_seq` | Community | Extraction snapshot used to build this Community generation |
| `source_structure_revision`, `source_export_digest` | Community | Exact serving-view revision and ordered ID/arc/threshold export hash |
| `source_episode_ids` | Fact | Sorted immutable list of 1–16 original Episode IDs used for mass and Hit attribution |
| `primary_episode_id` | non-synthesis Fact | Episode whose extraction/correction created this Fact; indexed for bounded session recall |
| `source_count_total`, `sources_truncated` | Fact | Audit fields recording the authority-set size before its deterministic cap |
| `max_source_ingest_seq` | Fact | Maximum ingest sequence in `source_episode_ids`; Community snapshot filter |
| `support_fact_ids` | synthesis Fact | 1–16 non-synthesis Facts whose validity the synthesis depends on |
| `sub_kind` | Fact | `fact / state / event / preference / procedure / decision / summary`. Input to the forgetting prior (docs/04 §1) |

### Schema registry

| schema | Label | Content |
|---|---|---|
| `anamnesis.original-message/1` | Episode | Conversation message |
| `anamnesis.original-document/1` | Episode | Document or file. One Episode per revision |
| `anamnesis.correction/1` | Episode | An explicit correction uttered by the user (the original behind docs/03 §5) |
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
                       + top 15 of authority(replaced Fact)
    synthesis Fact     = ∪ authority(member Facts)

  source_episode_ids = top 16 unique candidates
                       ORDER BY Episode.time_utc DESC, Episode.id ASC
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
    generation, schema, content, properties, time, sub_kind, primary_episode_id,
    max_source_ingest_seq,
    sorted(entity_ids), sorted(source_episode_ids), sorted(support_fact_ids)
  })
```

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
| `kind` | `recall_hit / re_mention / promotion / exposure` |
| `kappa_eff` | Reinforcement coefficient applied (κ(kind)/n, docs/04 §5) |
| `namespace` | Recall UUID or `extract:…` / `dream:…` producer namespace (audit) |
| `idem_key` | `sha256(namespace, episode_id, kind)`. Unique — a retry is a no-op |

Hits **never point at Facts or Communities.** When a derived element is
adopted, the hit is resolved to its source Episodes (docs/04 §5). Reason: a
generation switch replaces derived IDs wholesale; an immutable ledger pointing
at derived IDs would reset forgetting state on every switch
([10-decision-log](10-decision-log.md) D1). Every Hit, whatever its producer,
is written through the single commit path (docs/04 §6).

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
       all generation-scoped indexes ONLINE, authority and links valid
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
  "generation": 42,                        // absent on originals-layer links
  "idem_key": "…"
}
```

Links carry **no per-link weight.** PPR transition strength is a per-role
constant `w_role` in `config.jsonc`; each retained visible row normalizes its
role weights (docs/06 §4). This keeps the TypeScript and GDS transition
matrices identical ([10-decision-log](10-decision-log.md) D24).

Every INVALIDATES relationship copies `target_id` and the source's
`effective_time_utc`. Fact invalidation also carries `generation`. These
immutable fields support a relationship-index seek for `valid(T)` without
expanding all incoming invalidators. One Fact may create at most eight
outgoing INVALIDATES links.

| Role | Direction | Layer | Meaning | Conducts PPR |
|---|---|---|---|---|
| `NEXT_EPISODE` | Episode → next Episode | cache | Rebuildable same-session total order. Rewired on backfill | yes |
| `MENTIONS` | Episode\|Fact → Entity | derived | What it is about | yes |
| `RELATES_TO` | Fact\|Entity ↔ Fact\|Entity | derived | Free natural-language relation | yes |
| `HAS_MEMBER` | Community → Entity\|Fact | derived (community) | Topic membership; captures `member_visible_from_utc` at build | yes |
| `DERIVED_FROM` | Fact → Episode\|Fact | derived | Provenance chain. **Provenance is snapshot-exempt** (docs/03 §3) | yes |
| `INVALIDATES` | Fact → Fact / Episode → Episode | derived / originals | The target is not valid from this event's time onward | no |
| `CONTRASTS` | Fact ↔ Fact | derived | An unresolved contradiction, preserved | no |

PPR uses the five conducting roles **bidirectionally**. The stored direction is
for the semantic model.

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
          <role>.idem_key (7)
          · Outbox(stage, target_generation, ingest_seq, model_key)
          · EmbeddingCoverage(stream, generation, model_id)
          · EmbeddingBuild.model_id
          · EmbeddingBuildSource(model_id, stream, generation)
          · HubArc(hub_id, link_id) · OriginHead.origin_key · Meta.key (single node, key = 'meta')
range     Episode.origin_key · Episode.ingest_seq · Element.time_utc · Element.schema · Element.generation
          composite Episode(session_key, time_utc, ingest_seq)
          composite Fact(generation, primary_episode_id, time_utc, id)
          composite HubArc(hub_id, rank) · Outbox.processed_at
          composite Fact-INVALIDATES(target_id, generation, effective_time_utc, id)
          composite Episode-INVALIDATES(target_id, effective_time_utc, id)
fulltext  Episode.content (global) · Fact/Entity content (one index per generation)
vector    Episode.embedding_<model> (global) · :ExtractionG<N>.embedding_<model>
           · RELATES_TO.embedding_g<N>_<model>
           (one node/relationship index per generation and model)
```

Every ingest-derived Outbox sets every unique-key component:
`target_generation=0` for global Episode work and `model_key='-'` for
non-embedding stages. Re-queuing a different embedding model uses
`model_key=model_id`, so it cannot collide with an older model's completed
entry.

Degree is not a separate cache — Neo4j keeps per-node, per-type relationship
counts as metadata, so `COUNT { (n)-[:MENTIONS]-() }` is O(1). The envelope's
hub test uses the total conducting-role count (docs/06 §2).

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
  Element.m_cache                              mass snapshot for ordering, refreshed by maintenance (docs/06 §2)
  Entity.visible_from_utc                      min visible mention time, updated on active backfill
  Element.embedding_<model>                    backfill; write-once null → vector
  RELATES_TO.embedding_g<N>_<model>             generation-partitioned relationship vector
  Outbox.{state, attempts, next_retry_at,
          error, processed_at}                 sequencer/blocked-head cursor
  OriginHead.revision_key                      CAS head cache, rebuilt from the immutable revision chain
  Generation.{state, next_ingest_seq,
               covered_ingest_seq}              build/cutover lifecycle
  EmbeddingCoverage.covered_ingest_seq           model-scoped contiguous embedding cursor
  EmbeddingBuild.state                           BUILDING→ACTIVE/INACTIVE lifecycle
  Meta.{structure_revision, ingest_seq,
         last_server_time, writer_epoch, active_*,
         target_embedding_model}                 serving selectors, model build, clock and fencing
CREATE/DELETE allowed in caches
  session NEXT_EPISODE                         local rewire or full topology rebuild
  HubArc                                       maintenance rebuild
  ProfileCache                                 dreaming rebuild
DELETE allowed in derived/authority-adjacent state
  gc --derived                                 derived output of retired generations (§4)
  gc --embedding                               retired embedding property + index (§4)
  gc --objects                                 unreferenced Payload files (§9)
```

Integrity is checked by `verify` (every Episode digest + Payload existence and
hash + bounded Fact authority/list↔link agreement + Hit ledger ↔ cache replay
agreement).

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
         generation, embedding, maintenance, gc); redirect new remember to spool
      4. record structure_revision, ingest_seq, schema/Neo4j versions and the
         exact live container image digest plus committed Payload hash manifest;
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
