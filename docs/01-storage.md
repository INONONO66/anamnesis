# 01 — Storage

All graph data lives in one Neo4j (Community 5.26+, Docker, localhost-only).
Logically there are three layers, and each layer permits a different kind of
write.

```text
  originals  CREATE-only. No update, no delete. Loss here is irreversible
             Episode · Payload (metadata) · Hit
  derived    CREATE + a write-once generation retirement stamp (gen_to). Regenerable from originals
             Fact · Entity · Community · Link · embedding
  caches     SET allowed. Can be dropped and regenerated at any time
             hit cache (s, t_last_hit, hit_count) · hub shortlist · m_cache · Outbox · selectors
```

Payload bytes and the spool live on the filesystem outside Neo4j (§6).

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
| `origin_source/session/actor/record`, `origin_key` | Episode | Source identification. `origin_key` is unique |
| `ingested_at` | Episode | Server ms, written once at CREATE. **Not used in snapshot computation** — audit and spool-drain ordering only (docs/03 §1) |
| `payload_hash` | Episode | Payload reference (optional) |
| `digest` | Episode | SHA-256 of the original for integrity (checked by `verify`) |
| `gen_from`, `gen_to` | all derived | Generation range (§4) |
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
  transaction. A file without a node is a gc candidate; a node without a file
  is reported by `verify` as `missing-payload`.
- Backup = `neo4j-admin database dump` + `objects/` + `spool/` (§6).

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
| `recall_id` | Which recall produced it (audit) |
| `idem_key` | `sha256(recall_id, episode_id, kind)`. Unique — a retry is a no-op |

Hits **never point at Facts or Communities.** When a derived element is
adopted, the hit is resolved to its source Episodes (docs/04 §5). Reason: a
generation switch replaces derived IDs wholesale; an immutable ledger pointing
at derived IDs would reset forgetting state on every switch
([10-decision-log](10-decision-log.md) D1).

## 4. Derived layer and generations

The derived layer is split into three streams, each with its own integer
generation.

```text
  extraction   Fact · Entity · MENTIONS · RELATES_TO · DERIVED_FROM · INVALIDATES · CONTRASTS
  community    Community · HAS_MEMBER
  embedding    embedding_<model> property + vector index
```

Why separate streams: when dreaming rebuilds communities there is no reason to
re-extract Facts, and swapping the embedding model does not change extraction
output. Switching one stream's generation never touches another stream.

### Generation range and visibility

Every derived element and link has `gen_from` (the generation that created
it) and `gen_to` (the generation that retired it, default null). `gen_to` is
the only SET permitted in the derived layer, and it is written **exactly once,
null → integer**.

```text
  visible_gen(x, stream) = x.gen_from <= active[stream]
                        && (x.gen_to IS NULL || x.gen_to > active[stream])
```

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
DELETE path in the system and it touches the derived layer only.

### The embedding stream

Embeddings are per-model properties and indexes.

```text
  embedding_<model_id>          e.g. embedding_bge_m3_1024
  vector index vec_<model_id>   dimension and similarity depend on the model
  active[embedding] = <model_id>
```

Model swap = backfill the new property (Outbox re-queue) → coverage threshold
met → switch active. The previous property and index are cleaned up under the
gc rule (previous generation + 30 days). `RELATES_TO.content` is embedded
under the same rule.

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
  "weight": 1.0,                 // (0, 1]. PPR transition strength
  "gen_from": 42, "gen_to": null
}
```

| Role | Direction | Meaning | Conducts PPR |
|---|---|---|---|
| `NEXT_EPISODE` | Episode → next Episode | Same-session timeline. Wired automatically by remember() | yes |
| `MENTIONS` | Episode\|Fact → Entity | What it is about | yes |
| `RELATES_TO` | Fact\|Entity ↔ Fact\|Entity | Free natural-language relation | yes |
| `HAS_MEMBER` | Community → Entity\|Fact | Topic membership | yes |
| `DERIVED_FROM` | Fact → Episode\|Fact | Provenance chain. **Provenance is snapshot-exempt** (docs/03 §3) | yes |
| `INVALIDATES` | Fact\|Episode → Fact\|Episode | The target is not valid from this event's time onward | no |
| `CONTRASTS` | Fact ↔ Fact | An unresolved contradiction, preserved | no |

PPR uses the five conducting roles **bidirectionally**. The stored direction is
for the semantic model.

### Lattice — allowed (from, role, to)

```text
  Episode        --NEXT_EPISODE--> Episode
  Episode|Fact   --MENTIONS------> Entity
  Fact|Entity    --RELATES_TO----> Fact|Entity
  Community      --HAS_MEMBER----> Entity|Fact
  Fact           --DERIVED_FROM--> Episode|Fact
  Fact|Episode   --INVALIDATES---> Fact|Episode      (Episode: revision / divergence detection)
  Fact           --CONTRASTS-----> Fact
```

Anything outside the lattice is a contract violation and is rejected at write
time.

### Idempotency key

`idem_key = sha256(from, to, role, content)` — a per-role unique constraint.
Re-running extraction never creates the same link twice.

## 6. Filesystem

```text
~/.anamnesis/
├── sock              UDS (docs/02)
├── config.jsonc      calibration parameters and modes (docs/04 §9)
├── objects/          Payload bytes (§2)
├── spool/            remember() queue while Neo4j is unavailable, append-only jsonl (docs/02 §4)
├── neo4j/            container volume (data/, dumps/)
└── compose.yaml      container definition managed by the CLI
```

## 7. Indexes and constraints

```text
unique    Element.id · Element.origin_key · Payload.hash · Hit.idem_key
          <role>.idem_key (7)
range     Element.time_utc · Element.schema · Element.gen_from · Element.gen_to
          Outbox.processed_at
fulltext  Element.content  (Lucene, analyzer 'cjk')
vector    Element.embedding_<model> · RELATES_TO.embedding_<model>  (HNSW, cosine)
```

Degree is not a separate cache — Neo4j keeps per-node relationship counts as
metadata, so `COUNT { (n)-[:MENTIONS|…]-() }` is O(1). The envelope's hub
test and boundary normalization use this value (docs/06).

## 8. Immutability discipline

Neo4j Community has no database triggers. The discipline is two-fold.

1. **The daemon is the only write path.** Bolt is localhost-only and only the
   daemon holds a connection.
2. **An exhaustive list of every SET/DELETE in the code** — anything outside
   this list is rejected in review.

```text
SET allowed
  Episode.{s, t_last_hit, hit_count}           hit cache (docs/04)
  Element.m_cache                              mass snapshot for ordering, refreshed by dreaming (docs/06 §2)
  Element.embedding_<model>                    backfill; write-once null → vector, like gen_to
  Element|Link.gen_to                          null → integer, once
  Entity.shortlist                             hub neighbor shortlist (docs/06 §3)
  Outbox.processed_at                          cursor
  Meta.{structure_revision, active_*}          selectors
DELETE allowed
  gc --derived                                 derived output of retired generations (§4)
  gc --objects                                 unreferenced Payload files
```

Integrity is checked by `verify` (every Episode digest + Payload existence and
hash + Hit ledger ↔ cache replay agreement).
