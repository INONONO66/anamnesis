# 09 — Roadmap

Three stages. Each stage must be a usable product on its own, and the next
stage inherits the previous one's data without migration — the originals
layer and the Hit ledger are fixed in v0.1 and never change afterwards.

```text
  v0.1  originals + forgetting + search      "a search engine that forgets"
  v0.2  derived layer + time + local PPR     "memory that follows relationships"
  v0.3  dreaming + validation + scale        "memory that organizes itself"
```

Where the current code disagrees with the docs (payload bytes stored as base64
on a node, no `revision_key`, default Neo4j password, Element schema requiring
a time on every kind), v0.1 brings it in line — the docs lead.

## v0.1 — originals, forgetting, search

**Goal**: Episodes go in and come out; what comes out is reinforced and what
does not is forgotten. No derived layer, no PPR.

| Area | Contents | Docs |
|---|---|---|
| storage | Neo4j compose, schema, canonical Episode digest, global `ingest_seq`, OriginHead CAS, `:Element:Episode`, Payload metadata + `objects/`, originals links, rebuildable event-time NEXT_EPISODE topology | 01 §1–2, §6–8; 02 §3 |
| daemon | `anamnesisd` UDS JSON-RPC, bounded object upload, write queue, serving revision, pure-Node nonce/heartbeat singleton lease, core RPCs | 02 §1–3 |
| security | 0700/0600 modes, UDS capability token, length-prefixed frame and global resource caps, bolt on 127.0.0.1 only, per-install random password | 02 §10 |
| spool | fsync-before-ack, `.done` after commit, drain, retention, cold-start wait | 02 §4, §9 |
| embedding | `embed_episode` Outbox worker, bounded batches, active model property/index and retry behavior | 01 §4, 02 §3 |
| durability | write ordering, `gc --objects` safety, `anamnesis backup` / `restore`, `verify` | 01 §9 |
| time | Episode `time_*`, `ingested_at`, snapshot(T) filter (Episodes only) | 03 §1, §3 |
| forgetting | m₀, hit-cache initialization, R(t,S), Hit node + HIT_OF, S update, replay, `rebuild --hit-cache` | 04 §1–5, §7 |
| commit path | `commitHits` with producers 1 (receipt `recall_hit` + signed `outcome`) and 2 (exposure); negative-branch S update; `hello.commit_mode`; idem_key | 04 §5.1, §6, 05 §10 |
| recall | vector (nodes) + BM25 + session channels with caps, RRF, score = rel·m^γ, deterministic ordering, degradation ladder, diagnostics | 05 (without PPR, identity, relationship vectors) |
| clients | ops CLI (`up/down/status/verify/backup/restore`), RPC contract export (`schemas/`); remember/recall harnesses are external (D39) | 08 |
| protocol | zod: Episode-only time requirement, `source_revision`, `previous_revision_key`, `revision_key`, `correction` schema, Hit, RPC methods | 08 |
| CI | forgetting fixtures, RRF invariance, ordering conventions, contract schema tests | 04 §10, 07 §6 |

**Exit criteria**: ingest one month of the author's own conversation logs →
recall p50 < 50 ms (no PPR) → after 100 receipt adoptions, `verify` shows
ledger ↔ cache agreement → 50 remembers with Neo4j killed → drain verified
after recovery → `backup` then `restore` into an empty directory passes
`verify --scope all`.

## v0.2 — derived layer, time, local PPR

**Goal**: Facts and Entities are extracted, corrections follow time, and PPR
retrieves along relationships.

| Area | Contents | Docs |
|---|---|---|
| derived layer | `:Fact`, `:Entity`, physical generation labels/indexes, global `ingest_seq`, BUILDING/ACTIVE/CATCHING_UP/INACTIVE/RETIRED lifecycle, strict sequencer, dual-tail Outbox, atomic cutover and caught-up rollback | 01 §1, §4–5 |
| extraction | target sequencer: claim LLM → bounded generation-index reads → judge LLM → revalidated write; blocked-head retry, entity/fact identity, correction context, embed stage | 02 §5 |
| commit path | producer 3 (re_mention) | 04 §6 |
| maintenance | hourly job: `m_cache`, hub shortlist. **Precedes PPR** — the envelope depends on both | 02 §6 |
| time | Fact time, derived visibility for Entity and Link, non-recursive valid(T), replacement protocol, provenance exception | 03 §3–5 |
| forgetting | Fact mass = source max · σ_fact, κ conservation and merging, sources resolution | 04 §3, §5 |
| envelope | budgets, fanout, hub test (COUNT{}), non-hub scan cap, indexed HubArc cache, per-row directed arc cap, row-total initialization, 3-query tx, deadline, retained-row normalization | 06 §1–4 |
| PPR | CSR, iteration, convergence cap, determinism conventions | 06 §5–7 |
| recall | relationship vector channel (`queryRelationships`), seeds (hub damping), PPR list in fusion, valid filter, provenance/supersedes/contrasts, torn retry | 05 §2–7 |
| GDS | disposable validation container, σ-node solver validation, L1/top-k/NDCG CI | 07 §1–2 |
| gc | RETIRED-generation-only `gc --derived`, protected rollback targets, `gc --embedding` | 01 §4 |

**Exit criteria**: extraction runs on v0.1 data with no downtime → 20 solver
validations pass → correction scenario fixtures (change / correction /
replacement) pass snapshot queries → a hub Entity with degree > 256 is
expanded through its shortlist in a recall → recall p50 < 100 ms.

## v0.3 — dreaming, validation, scale

**Goal**: the graph builds its own topics, and the truncation quality is
measured.

| Area | Contents | Docs |
|---|---|---|
| dreaming | bounded ID/arc export → disposable networkless GDS Leiden → pinned community generation, exact-support synthesis, profile cache | 02 §7 |
| commit path | producer 4 (promotion) | 04 §6 |
| derived layer | `:Community`, cross-stream-compatible HAS_MEMBER, extraction-cutover disable/rebuild rule, majority visibility, Community mass | 01 §4, 03 §3, 04 §3 |
| recall | identity channel, `entities` block | 05 §2 |
| embedding | model swap procedure (new property and index, backfill, switch, gc) | 01 §4 |
| GDS | envelope validation overlap@20, 100k/1M scale benches, health report | 07 §3–5 |
| calibration | receipt logs → refit DECAY, FACTOR, a, b, c, γ, σ_fact, role weights, RRF weights; config version tags | 04 §9 |

**Exit criteria**: on a 1M synthetic graph p50 < 100 ms / p95 < 250 ms,
envelope deadline exceeded < 1 %, overlap@20 ≥ 0.8, dreaming on 1M in
< 30 minutes.

## Not doing

- Multi-user, auth beyond the OS user, remote bolt. Personal, localhost.
- A second graph store outside Neo4j (SQLite, custom format). The previous
  roadmap is retired ([10-decision-log](10-decision-log.md) D0).
- LLM calls on the recall path. Summarization and re-ranking belong to the
  caller.
- Transaction time / bitemporal (03 §6).
- Recursive INVALIDATES (03 §4).
- Tick- or schedule-based forgetting updates (04).
- Per-link weights (D24).
