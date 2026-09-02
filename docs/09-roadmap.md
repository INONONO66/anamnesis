# 09 — Roadmap

Three stages. Each stage must be a usable product on its own, and the next
stage inherits the previous one's data without migration — the originals
layer and the Hit ledger are fixed in v0.1 and never change afterwards.

```text
  v0.1  originals + forgetting + search      "a search engine that forgets"
  v0.2  derived layer + time + local PPR     "memory that follows relationships"
  v0.3  dreaming + validation + scale        "memory that organizes itself"
```

## v0.1 — originals, forgetting, search

**Goal**: Episodes go in and come out; what comes out is reinforced and what
does not is forgotten. No derived layer, no PPR.

| Area | Contents | Docs |
|---|---|---|
| storage | Neo4j compose, schema, constraints, `:Element:Episode`, Payload metadata + `objects/`, NEXT_EPISODE | 01 §1–2, §6–7 |
| daemon | `anamnesisd` UDS JSON-RPC, write queue, `structure_revision`, `hello/remember/recall/commit/status/verify` | 02 §1–3 |
| spool | append-only while Neo4j is down, drain, cold-start wait | 02 §4, §8 |
| time | Episode `time_*`, `ingested_at`, snapshot(T) filter (Episodes only), revision INVALIDATES | 03 §1, §3 |
| forgetting | m₀, hit-cache initialization, R(t,S), Hit node + HIT_OF, κ recall_hit/exposure, S update, replay, verify | 04 all |
| commit | `hello.commit_mode`, receipt commit procedure, auto exposure, idem_key | 04 §6, 05 §10 |
| recall | vector + BM25 + session channels, RRF, score = rel·m^γ, deterministic ordering, degradation ladder, diagnostics | 05 (without PPR and identity) |
| clients | CLI (`up/down/remember/recall/status/verify`), MCP server (receipt), claude-code hook (auto) | 08 |
| CI | forgetting fixtures, RRF invariance, ordering conventions, contract schema tests | 04 §10, 07 §6 |

**Exit criteria**: ingest one month of the author's own conversation logs →
recall p50 < 50 ms (no PPR) → after 100 receipt adoptions, `verify` shows
ledger ↔ cache agreement → 50 remembers with Neo4j killed → drain verified
after recovery.

## v0.2 — derived layer, time, local PPR

**Goal**: Facts and Entities are extracted, corrections follow time, and PPR
retrieves along relationships.

| Area | Contents | Docs |
|---|---|---|
| derived layer | `:Fact`, `:Entity`, the 7-role link lattice, `idem_key`, `gen_from/gen_to`, extraction generation + selector, `gen` RPC, per-Episode supersession, rollback | 01 §4–5 |
| extraction | Outbox worker: claims, time resolution, entity resolution, judge (new/duplicate/elaboration/contradiction), mode change\|correction, re_mention Hit, embed stage | 02 §5 |
| time | Fact time, derived visibility for Entity and Link, non-recursive valid(T), replacement protocol, provenance exception | 03 §3–5 |
| forgetting | Fact mass = source max · σ_fact, κ conservation and merging, sources resolution | 04 §3, §5 |
| envelope | budgets, fanout, hub test (COUNT{}), 4-query tx, 100 ms deadline, true degree and leak | 06 §1–4 |
| PPR | CSR, iteration, convergence cap, determinism conventions | 06 §5–7 |
| recall | seeds (hub damping), PPR list in fusion, valid filter, provenance/supersedes/contrasts, torn retry | 05 §3–7 |
| GDS | solver validation (σ-node construction, L1, top-k, NDCG), CI on every PR | 07 §2 |
| gc | `gc --derived` (previous generation + 30 days) | 01 §4 |

**Exit criteria**: extraction runs on v0.1 data with no downtime → 20 solver
validations pass → correction scenario fixtures (change / correction /
replacement) pass snapshot queries → recall p50 < 100 ms.

## v0.3 — dreaming, validation, scale

**Goal**: the graph builds its own topics, handles hubs, and the truncation
quality is measured.

| Area | Contents | Docs |
|---|---|---|
| dreaming | Leiden (GDS profile) → community generation, synthesis + promotion Hits, hub shortlist, `m_cache`, profile cache | 02 §6 |
| derived layer | `:Community`, HAS_MEMBER, majority visibility, Community mass | 01, 03 §3, 04 §3 |
| recall | identity channel, `entities` block, hub shortlist expansion | 05 §2, 06 §3 |
| embedding | model swap procedure (new property and index, backfill, switch, gc) | 01 §4 |
| GDS | envelope validation overlap@20, 100k/1M scale benches, health report | 07 §3–5 |
| calibration | receipt logs → refit DECAY, FACTOR, a, b, c, γ, σ_fact, RRF weights; config version tags | 04 §9 |

**Exit criteria**: on a 1M synthetic graph p50 < 100 ms / p95 < 250 ms,
envelope deadline exceeded < 1 %, overlap@20 ≥ 0.8, dreaming on 1M in
< 30 minutes.

## Not doing

- Multi-user, auth, remote bolt. Personal, localhost.
- A second graph store outside Neo4j (SQLite, custom format). The previous
  roadmap is retired ([10-decision-log](10-decision-log.md) D0).
- LLM calls on the recall path. Summarization and re-ranking belong to the
  caller.
- Transaction time / bitemporal (03 §6).
- Recursive INVALIDATES (03 §4).
- Tick- or schedule-based forgetting updates (04).
