# The graphiti Family — Analysis and What We Borrowed

> Non-normative background (written 2026-08). The normative design is
> [docs/00–10](../00-overview.md); where a "borrowing decision" here differs
> from it, the normative docs win.

> Analyzed (2026-08-30, full read of code and ADRs):
> - [getzep/graphiti](https://github.com/getzep/graphiti) — the original. Zep's core engine.
> - [Soju06/graphiti](https://github.com/Soju06/graphiti) — a fork with production patches.
> - `Soju06/hermes-graphiti` — memory plugin for the Hermes agent plus a
>   single-writer daemon. The analyzed repository is no longer publicly
>   available as of 2026-09; observations below are retained as historical
>   analysis rather than a live citation.

## 1. Upstream graphiti — logical structure

The correspondence with our design is nearly isomorphic:

| graphiti | anamnesis | Note |
|---|---|---|
| EpisodicNode (raw) | Episode element (original-message etc.) | both keep originals lossless |
| EntityNode (name + summary + name_embedding) | mapping / Entity elements | entities are natural-language summaries too |
| **EntityEdge.fact (natural-language sentence) + fact_embedding** | `RELATES_TO.content` | "facts live on edges as natural language" — same principle as ours |
| valid_at / invalid_at / expired_at (updated in place) | invalidation-as-event (immutable) | ours is stronger — see the incident in §3 |
| CommunityNode (cluster summary) | dreaming's consolidation tier | |
| per-tier hybrid search (BM25 + cosine + BFS) → RRF/MMR/cross-encoder | recall pipeline | recipe structure borrowed |

**Borrowing 1 — link embeddings.** Like graphiti's fact_embedding, the
content of RELATES_TO links is embedded and included in vector search. Recall
candidates must come from "relationship statements", not only from elements.

**Borrowing 2 — search recipes.** Build BM25 + vector candidates per tier
(episode / derived / consolidated), fuse with RRF, and offer MMR (diversity)
and cross-encoder (precision) re-ranking as optional recipes.

## 2. The Soju06/graphiti fork — production patches

**Borrowing 3 — noise-entity filter (extraction prompt rule).** Machine
tokens — run ids (`proc_4fe2...`), SHAs, `attempt=1` counters, `/tmp` paths,
`OK`/`DONE` status tokens — are not extracted as entities. "They identify a
one-off execution, are not objects in the user's world, and are never searched
for later." Essential for us, since agent logs are a primary source. Keep them
inside the fact sentence when meaningful, but name a durable thing as the
subject.

**Borrowing 4 — cap on re-ranking candidates.** Uncapped node candidates
caused hundreds of classifier calls per search; the patch caps at RRF seeds →
`2 × limit`. Our recall puts candidate caps in the contract from the start.

**Borrowing 5 — write-path hook seam.** An explicit, fail-open hook contract
for intervening in the write path (edge judgment etc.) without monkey-patching.
Our digest handlers are formalized in the same spirit: the engine has a default
behavior when no hook is present; when one is, the hook receives the default
implementation as an argument and wraps it.

## 3. hermes-graphiti — lessons from operational incidents

**Lesson A — failure of an embedded graph DB (ADR-036).** Kuzu (embedded)
collapsed in production: two processes opening the same DB → lock conflicts,
SEGV on the first write after SIGKILL, +0.6 MB leak per search → a pile of
workarounds (boot integrity probes, shutdown sentinels, self-restart) that
were finally torn out in favor of a Neo4j server + **a single mandatory
daemon**. The root cause was "multiple processes opening an embedded DB
directly", and the fix is a single-writer daemon. **anamnesis has anamnesisd
as the only access path from day one, so this incident is structurally
impossible.**

**Lesson B — the invalidation misfire disaster (investigation of 2026-07-08).**
Invalidation judgments (resolve_extracted_edge) were delegated to a mini model,
and **54 % of all facts (96.6k / 178.6k) were falsely invalidated**. Re-judging
everything with a strong model restored 95.3 % and normalized the invalidation
rate to 2.5 %. Two things are fixed by this:
1. **Judgment (duplicate / contradiction / invalidation decisions) uses a model
   at least as strong as extraction.** It is not a job for a mini model.
2. graphiti edits `invalid_at` in place, so a repair script over 100k edges was
   needed. With invalidation-as-event, the invalidation itself is an immutable
   element, so repairing a misfire is "add an event that invalidates the
   invalidation". **Empirical justification of the immutable design.**

**Lesson C — banish maintenance from the hot path (ADR-107).** Community
summary and membership updates were removed from the write path and moved to
threshold-triggered deferred batches. Same conclusion as our hot/cold split and
dreaming. Consolidation never sits in the latency of remember or digest.

**Lesson D — time weighting is multiplicative, not a hard filter (ADR-102).**
`final = (1-w)·vector + w·(decay × recency × validity × kind)`. Same
philosophy as our read-time mass evaluation — invalidation and weathering do
not delete candidates, they weigh them down (except for the explicit snapshot
cut).

**Lesson E — failures are not silently dropped (ADR-098/101).** Failed
extraction episodes are preserved in a DLQ file with an idempotent replay
script, and a bounded ingest queue (bulkhead) blocks stampedes. To add to our
Outbox: a retry counter on digest-handler failure and DLQ marking after N
attempts (elements are immutable, so only cursor state changes).

**Lesson F — re-ranking and recall UX (ADR-041/042/106).** Center-node
proximity search (re-ranking by graph distance), two-stage recall (find the
entity → unfold surrounding facts), communities used to structure broad recall
(topic map + evidence by topic). Candidates for recall v0.3+.

## 4. Decision — whether to move to a graph DB

> **[Updated 2026-08-30]** The "npm install without Docker" constraint was
> lifted, reversing the initial decision below. **We move to Neo4j as the
> single store** (graph + vector HNSW + fulltext Lucene + GDS in one system).
> No separate vector DB such as Qdrant — both upstream graphiti and
> hermes-graphiti keep embeddings inside the graph DB; separating them only
> adds synchronization plumbing. Revisit behind the recall seam if vectors in
> the hundreds of millions are ever measured.
> Details in [docs/01-storage](../01-storage.md).

Initial decision (while the constraint held): borrow the logical architecture
wholesale from graphiti (three tiers, facts = natural language + embedding,
hybrid search + re-ranking, single-writer daemon, deferred maintenance) but do
not move the physical store to Neo4j —

- hermes-graphiti's Neo4j stack = Docker compose + JVM (8 GB+ RAM) + autoheal
  sidecar + systemd timers. A head-on collision with "npm install and run,
  no Docker".
- Their decisive reason for Neo4j was not performance but "concurrent
  multi-process access + dashboard routing" (ADR-036 trade-off table) — which
  we already solve by having a single daemon.
- Up to tens of millions of elements: SQLite adjacency lists + typed-array PPR
  (HippoRAG style) + LanceDB ANN (IVF-PQ). Graph traversal sits behind the
  recall seam, so when a measured limit arrives, replace just that point with
  FalkorDB or similar.

## 5. Roadmap impact

- v0.2 (extraction): noise-entity filter rule, judge model ≥ extraction model,
  DLQ cursor, link embeddings.
- v0.2 (recall): RRF fusion + candidate caps, multiplicative time weighting.
- v0.3: deferred community (consolidation) batches, center-node proximity
  re-ranking, two-stage recall, MMR / cross-encoder recipes.
