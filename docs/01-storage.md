# 01 — Storage

All graph data lives in one Neo4j (Community 5.26+, Docker, localhost-only).
Logically there are three layers, and each layer permits a different kind of
write.

```text
  originals  CREATE-only. No update, no delete. Loss here is irreversible
             Episode · Payload (metadata) · Hit
             originals-layer links: NEXT_EPISODE · HAS_PAYLOAD · HIT_OF · Episode→Episode INVALIDATES (revision)
  derived    CREATE + a write-once generation retirement stamp (gen_to). Regenerable from originals
             Fact · Entity · Community · derived links · embedding
  caches     SET allowed. Can be dropped and regenerated at any time
             hit cache (s, t_last_hit, hit_count) · hub shortlist · m_cache · Outbox · selectors
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
(:Element:Entity)     anchor for a person, thing or concept. No stored time (visibility is derived, docs/03)
(:Element:Community)  topic-set summary. No stored time (visibility is derived, docs/03)
```

### Common properties

| Property | Kinds | Meaning |
|---|---|---|
| `id` | all | UUIDv7. **Never used as a sort or truncation key** — it is creation-ordered, so truncating on it biases toward old elements (docs/06 §2) |
| `schema` | all | `anamnesis.<kind>/<n>`. The only notion of "type" |
| `content` | all | Normalized natural language |
| `m0` | all | Intrinsic mass in [0, 1]. Assigned once at creation, immutable (docs/04 §1) |
| `properties` | all | Schema-specific extras as JSON. Kept minimal |
| `time_value`, `time_utc`, `time_precision` | Episode, Fact | Event time (docs/03 §1) |
| `origin_source/session/actor/record` | Episode | Source identification |
| `origin_key` | Episode | **Logical** source identity (`sha256(source, session, actor, record)`). Indexed, **not unique** — every revision of the same document shares it |
| `revision_key` | Episode | `sha256(origin_key, digest)`. **Unique.** Identity of one revision; what makes remember idempotent |
| `ingested_at` | Episode | Server ms, written once at CREATE. **Not used in snapshot computation** — audit and spool-drain ordering only (docs/03 §1) |
| `payload_hash` | Episode | Payload reference (optional) |
| `digest` | Episode | SHA-256 of the original for integrity (checked by `verify`) |
| `gen_from`, `gen_to` | Fact, Entity, Community, derived links | Generation range (§4) |
| `idem_key` | Fact | `sha256(gen_from, sorted(direct source ids), content)`. Unique. Re-extraction within a generation is a no-op; a new generation produces a new Fact |
| `sub_kind` | Fact | `fact / state / event / preference / procedure / decision / summary`. Input to the forgetting prior (docs/04 §1) |

### Schema registry

| schema | Label | Content |
|---|---|---|
| `anamnesis.original-message/1` | Episode | Conversation message |
| `anamnesis.original-document/1` | Episode | Document or file. One Episode per revision |
| `anamnesis.correction/1` | Episode | An explicit correction uttered by the user (the original behind docs/03 §5) |
| `anamnesis.claim/1` | Fact | An extracted claim. Invalidation events are claims too — the only special thing about them is an outgoing INVALIDATES edge |
| `anamnesis.mapping/1` | Fact | An actor ↔ person mapping claim |
| `anamnesis.synthesis/1` | Fact | A higher-level fact combining several sources (many DERIVED_FROM) |
| `anamnesis.entity/1` | Entity | Anchor |
| `anamnesis.community/1` | Community | Topic set. Owns members through HAS_MEMBER |

### Contract: every Fact has provenance

A Fact has **at least one** outgoing DERIVED_FROM, created in the same
transaction as the Fact. `verify` reports a Fact without one as
`orphan-fact`. Everything that derives mass from sources (docs/04 §3) relies
on this.

## 2. Payload — outside Neo4j

Original bytes do not go into the graph database. To avoid property-store
bloat, page-cache pollution and dump growth, they are stored as
content-addressed files.

```text
~/.anamnesis/objects/<sha256[0:2]>/<sha256>        bytes (write-once, fsync)
(:Payload {hash, size, media_type})                 metadata node. No bytes
(:Element:Episode {payload_hash}) -[:HAS_PAYLOAD]-> (:Payload)
```

- Write order: file first (temp name → fsync → rename), then the Neo4j
  transaction. A file without a node is a gc candidate (with the spool
  exception in §9); a node without a file is reported by `verify` as
  `missing-payload`.
- Because the file always exists before any transaction that references it,
  a backup that dumps Neo4j **first** and copies `objects/` **afterwards** is
  consistent without pausing writes (§9).

## 3. Hit ledger — attached to Episodes

```text
(:Hit {id, t, kind, kappa_eff, recall_id, idem_key}) -[:HIT_OF]-> (:Element:Episode)
```

| Property | Meaning |
|---|---|
| `id` | UUIDv7 (server-issued) |
| `t` | Server time, ms epoch. **Not an event time** — forgetting runs on the now axis (docs/04 §4) |
| `kind` | `recall_hit / re_mention / promotion / exposure` |
| `kappa_eff` | Reinforcement coefficient applied (κ(kind)/n, docs/04 §5) |
| `recall_id` | Which recall (or which extraction / dreaming run) produced it (audit) |
| `idem_key` | `sha256(recall_id, episode_id, kind)`. Unique — a retry is a no-op |

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
               selector active[embedding]  = model_id string. No gen_from/gen_to — a property is present or absent
```

Why separate streams: when dreaming rebuilds communities there is no reason to
re-extract Facts, and swapping the embedding model does not change extraction
output. Switching one stream's selector never touches another stream.

### Originals-layer links have no generation

`NEXT_EPISODE`, `HAS_PAYLOAD`, `HIT_OF` and `Episode → Episode INVALIDATES`
are created by remember and commit, never by a pipeline. They carry no
`gen_from/gen_to`, are CREATE-only like their endpoints, and are always
generation-visible. Every generation filter in this design is written as
`l.gen_from IS NULL OR (…)` so that they pass.

### Generation range and visibility

Every derived element and derived link has `gen_from` (the generation that
created it) and `gen_to` (the generation that retired it, default null).
`gen_to` is the only SET permitted in the derived layer, and it is written
**exactly once, null → integer**.

```text
  visible_gen(x, stream) = x.gen_from IS NULL
                        || (x.gen_from <= active[stream] && (x.gen_to IS NULL || x.gen_to > active[stream]))
```

The stream a link belongs to is fixed by its role: HAS_MEMBER → community,
every other derived role → extraction.

### Incremental switch — per-Episode supersession

A new extraction generation (an extractor version bump) does not re-extract
everything before switching.

```text
  1. active = 42. New extractor ready → open 43 (selector still 42)
  2. Re-extract Episodes in priority order (recent, high-mass first):
       new Facts/Links          gen_from = 43
       42-output of that Episode gen_to = 43   (write-once stamp)
  3. Sample validation passes → active := 43 (atomic SET, structure_revision += 1)
       · re-extracted Episodes show their 43 output
       · not-yet-processed Episodes keep showing their 42 output (gen_to null)
  4. Re-extraction continues in the background. When done, all 42 output has gen_to = 43
  5. rollback = active := 42. 43 output hides (gen_from > 42),
     42 output shows again (gen_to 43 > 42). No data moves
```

When the extractor version is unchanged, a new Episode's extraction is
appended to the current active generation (gen_from = active). The number of
generations therefore equals the number of extractor versions and does not
grow with the number of elements.

Because `gen_from` is part of every derived `idem_key`, re-creating "the same"
Fact or link in generation 43 does not collide with its generation-42
predecessor — the two coexist, one visible, one retired.

### Generation range of links

A link carries the generation of the pipeline run that created it as
`gen_from`. Links whose endpoints belong to different generations are allowed
(e.g. a 43 INVALIDATES pointing at a 42 Fact — a new utterance invalidating a
fact from an Episode not yet re-extracted). Visibility is
`visible_gen(link) && visible(both ends)` (docs/03 §3).

### Retention and gc

Retired generations stay for audit and rollback. Deletion happens only through
the explicit command `anamnesis gc --derived --before-gen N`, with a default
retention of **the previous generation plus 30 days**. This is the only
DELETE path into the graph and it touches the derived layer only.

### The embedding stream

Embeddings are per-model properties and indexes.

```text
  embedding_<model_id>          e.g. embedding_bge_m3_1024
  vector index vec_<model_id>   dimension and similarity depend on the model
  active[embedding] = <model_id>
```

Model swap = backfill the new property (Outbox re-queue) → coverage threshold
met → switch active. The previous property and index are removed by
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
  "gen_from": 42, "gen_to": null,          // absent on originals-layer links
  "idem_key": "…"
}
```

Links carry **no per-link weight.** PPR transition strength is a per-role
constant `w_role` in `config.jsonc` (docs/06 §4). A per-link weight would make
the true weighted degree unavailable in O(1) and would break the agreement
between local PPR and the GDS baseline ([10-decision-log](10-decision-log.md)
D24).

| Role | Direction | Layer | Meaning | Conducts PPR |
|---|---|---|---|---|
| `NEXT_EPISODE` | Episode → next Episode | originals | Same-session timeline. Wired by remember() | yes |
| `MENTIONS` | Episode\|Fact → Entity | derived | What it is about | yes |
| `RELATES_TO` | Fact\|Entity ↔ Fact\|Entity | derived | Free natural-language relation | yes |
| `HAS_MEMBER` | Community → Entity\|Fact | derived (community) | Topic membership | yes |
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
  derived links     idem_key = sha256(from, to, role, content, gen_from)
  originals links   idem_key = sha256(from, to, role)
```

A per-role unique constraint. Re-running extraction within a generation never
creates the same link twice; a new generation legitimately creates its own
copy.

## 6. Filesystem

```text
~/.anamnesis/                 mode 0700
├── sock                      UDS, mode 0600 (docs/02 §10)
├── daemon.lock               flock — one anamnesisd per data directory
├── config.jsonc              calibration parameters and modes (docs/04 §9)
├── neo4j.auth                per-install random Neo4j password, mode 0600
├── objects/                  Payload bytes (§2). Part of the authority
├── spool/                    transient remember() queue while Neo4j is unavailable (docs/02 §4)
├── neo4j/                    container volume (data/, dumps/)
└── compose.yaml              container definition managed by the CLI
```

## 7. Indexes and constraints

```text
unique    Element.id · Episode.revision_key · Fact.idem_key · Payload.hash · Hit.idem_key
          <role>.idem_key (7) · Meta.key (single node, key = 'meta')
range     Episode.origin_key · Element.time_utc · Element.schema · Element.gen_from · Element.gen_to
          Outbox.processed_at
fulltext  Element.content  (Lucene, analyzer 'cjk')
vector    Element.embedding_<model> · RELATES_TO.embedding_<model>  (HNSW, cosine)
```

Degree is not a separate cache — Neo4j keeps per-node, per-type relationship
counts as metadata, so `COUNT { (n)-[:MENTIONS]-() }` is O(1). The envelope's
hub test and the weighted true degree (five such counts per node) use this
(docs/06 §4).

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
  Element.embedding_<model>                    backfill; write-once null → vector, like gen_to
  Element|Link.gen_to                          null → integer, once
  Entity.shortlist                             hub neighbor shortlist (docs/06 §3)
  Outbox.processed_at                          cursor
  Meta.{structure_revision, active_*}          selectors
DELETE allowed
  gc --derived                                 derived output of retired generations (§4)
  gc --embedding                               retired embedding property + index (§4)
  gc --objects                                 unreferenced Payload files (§9)
```

Integrity is checked by `verify` (every Episode digest + Payload existence and
hash + orphan Facts + Hit ledger ↔ cache replay agreement).

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
| objects write | temp file → `fsync(file)` → `rename` → `fsync(dir)`. Only then is the hash referenced anywhere |
| spool append | one JSON line → `fsync` → **then** ack to the caller. A crash before the ack loses nothing the caller believes stored |
| spool drain | per line: Neo4j transaction commits → `.done` offset advanced → `fsync`. A crash between the two replays the line, and `revision_key` makes the replay a no-op |
| Neo4j | its own WAL. Every remember/commit/extract is one transaction, so there is no partially applied state |

### gc --objects safety

A payload file is deleted only if **no** `(:Payload {hash})` node references it
**and** no undrained spool line references it. gc reads the spool's pending
lines before deciding. A file younger than 1 hour is never deleted (covers the
window between the objects write and its transaction).

### Backup

```text
  1. neo4j-admin database dump  →  ~/.anamnesis/neo4j/dumps/<ts>.dump     (online-consistent snapshot)
  2. copy objects/                                                          (after the dump — see §2)
  3. copy spool/ if non-empty                                               (optional; a non-empty spool means Neo4j was down)
```

No write pause is needed: every hash referenced by the dump was fully written
before the transaction that referenced it, and objects are never modified, so
a later copy of `objects/` is a superset of what the dump needs.

### Restore

```text
  1. anamnesis down
  2. neo4j-admin database load <dump>
  3. restore objects/ (and spool/ if present)
  4. anamnesis up  → drains the spool if any
  5. anamnesis verify --scope all    (digests, payloads, orphan Facts, ledger ↔ cache)
```

Caches are rebuilt on demand (`anamnesis rebuild --hit-cache`, maintenance
job for `m_cache` and shortlists); they are not part of what must be restored.
