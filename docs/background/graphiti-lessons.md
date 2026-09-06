# The graphiti Family — Analysis and What We Borrowed

> Non-normative background (written 2026-08, anamnesis-side statements
> corrected 2026-09 to match the finalized design). The normative design is
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
| valid_at / invalid_at / expired_at (updated in place) | invalidation-as-event (immutable) | different failure mode, see the incident in §3 |
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
*Not adopted in the normative design.* The extraction pipeline is a fixed
sequence (claim LLM → bounded read → judge LLM → revalidated write, D28)
with no caller hooks; harnesses sit outside the daemon (D39) and the only
write-path intervention a caller has is the authenticated policy RPC (D43).

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
   needed. With invalidation-as-event the wrong INVALIDATES is itself an
   immutable record, so nothing is lost. The repair is not "invalidate the
   invalidator": INVALIDATES is non-recursive (D3), so a wrongly invalidated
   Fact A is restored by an explicit replacement A′ that copies A's content
   and time, DERIVED_FROM A and the correcting source, and INVALIDATES the
   wrong invalidator's Fact. Mass repair is a batch of replacement Facts,
   which the ledger records as such. The incident shows why the original
   must survive the misfire; it doesn't by itself show which repair protocol
   is better.

**Lesson C — banish maintenance from the hot path (ADR-107).** Community
summary and membership updates were removed from the write path and moved to
threshold-triggered deferred batches. Same conclusion as our hot/cold split and
dreaming. Consolidation never sits in the latency of remember or digest.

**Lesson D — time weighting is multiplicative, not a hard filter (ADR-102).**
`final = (1-w)·vector + w·(decay × recency × validity × kind)`. Partly the
same philosophy: our accessibility `m` and utility `U` multiply into the
score at read time and never delete a candidate. Validity is different: an
invalid Fact still conducts in the envelope but is a hard filter at assembly
(D23), and policy is a hard filter everywhere (D43). We weigh by mass and
filter by validity and policy; we don't fold validity into a soft weight.

**Lesson E — failures are not silently dropped (ADR-098/101).** Failed
extraction episodes are preserved in a DLQ file with an idempotent replay
script, and a bounded ingest queue (bulkhead) blocks stampedes. The
normative equivalent: the extraction sequencer pauses the blocked sequence
head after three automatic failures and nothing overtakes it (D28); the
Episode is already durable, so only cursor state is involved, and the spool
is the bounded ingest queue (D22).

**Lesson F — re-ranking and recall UX (ADR-041/042/106).** Center-node
proximity search (re-ranking by graph distance), two-stage recall (find the
entity → unfold surrounding facts), communities used to structure broad recall
(topic map + evidence by topic). Candidates for recall v0.3+.

## 4. Decision — whether to move to a graph DB

> **[Updated 2026-08-30]** The "npm install without Docker" constraint was
> lifted, reversing the initial decision below. **We move to Neo4j as the
> single graph and index store** (graph + vector HNSW + fulltext Lucene).
> No separate vector DB such as Qdrant — both upstream graphiti and
> hermes-graphiti keep embeddings inside the graph DB; separating them only
> adds synchronization plumbing. Revisit behind the recall seam if vectors in
> the hundreds of millions are ever measured.
> Details in [docs/01-storage](../01-storage.md).
>
> **[Corrected 2026-09]** "Single store" was later narrowed (D5): payload
> bytes live in `~/.anamnesis/objects/`, and GDS runs only in disposable
> offline jobs for dreaming and validation, never on the recall path.

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

As of the 2026-09 finalization, checked against [docs/09](../09-roadmap.md):

- v0.2 (extraction): noise-entity filter rule, judge model ≥ extraction
  model, blocked-head pause instead of a DLQ (D28), link embeddings.
- v0.2 (recall): RRF fusion + candidate caps, multiplicative mass weighting.
- v0.3: deferred community (consolidation) batches. Center-node proximity
  re-ranking and two-stage recall are still candidates, not scheduled. MMR
  and cross-encoder re-ranking are not planned inside the daemon: docs/09
  puts re-ranking with the caller, and recall makes no LLM calls.
