# 00 — Overview

> This document set (docs/00–10) is the normative design for anamnesis2.
> Code follows the docs. The previous drafts (docs/00–11, 2026-08) are
> retired; the reasons and the decisions that replaced them are recorded in
> [10-decision-log](10-decision-log.md). Non-normative material (competitor
> comparison, field lessons) lives in `docs/background/`.

## One line

anamnesis2 is a personal memory engine that uses **Neo4j as its only graph
and index store**, and performs the repeated numeric work of remembering —
spreading activation, forgetting, fusion — **in TypeScript memory, within
strict per-request bounds**. Neo4j GDS is not on the online path; it is used
for offline analysis and as an accuracy baseline only.

```text
  Neo4j                                          ~/.anamnesis/
    ├─ originals: Episode · Hit ledger              ├─ objects/   payload bytes (content-addressed, part of the authority)
    ├─ derived:   Fact · Entity · Community         ├─ spool/     transient remember() queue while Neo4j is down
    │             Link · embedding (generations)    └─ neo4j/     container volume
    ├─ caches:    hit cache · hub shortlist
    ├─ vector (HNSW) · fulltext (Lucene) candidates
    └─ bounded envelope extraction
              │
              v
  TypeScript dynamics (inside anamnesisd, per request)
    ├─ local PPR  (CSR, Float64Array, ≤ 2,000 nodes / 20,000 links)
    ├─ forgetting m(now) = m₀ · R(t, S)  — no tick
    ├─ RRF fusion × mass weighting
    └─ snapshot(T) · INVALIDATES · provenance assembly
              │
              v
  Neo4j GDS (offline)
    └─ dreaming (Leiden) · global centrality · local-PPR accuracy baseline · scale benches
```

## Three separations

| Component | Responsible for | Not responsible for |
|---|---|---|
| **Neo4j** | durable memory, indexes, relationship pattern matching, bounded neighborhood extraction | iterative numeric computation, score fusion |
| **TypeScript dynamics** | bounded per-request cognitive dynamics (spreading, forgetting, fusion) | global graph analysis, durable state |
| **GDS** | offline global analysis, quality baselines | online recall |

The central trade-off: we **give up the complete result of full-graph PPR**
in exchange for the **predictable latency, determinism and local-first
operability** of a strictly bounded local PPR. How much quality the envelope
truncation costs is measured, not estimated
([07-gds-validation](07-gds-validation.md)).

## Invariants — rules that span every document

1. **The originals layer is CREATE-only.** Episode, Payload, Hit,
   HAS_PAYLOAD, HIT_OF and revision INVALIDATES are never modified or deleted
   once written. NEXT_EPISODE is a rebuildable cache because event-time
   backfill must rewire session order. Mistakes are fixed by events
   (INVALIDATES). The data authority is the Neo4j
   database plus `~/.anamnesis/objects/`, nothing else
   ([01-storage](01-storage.md) §9).
2. **The derived layer is regenerable.** Fact, Entity, Community, Link and
   embeddings must be rebuildable at any time from the originals layer and
   the Hit ledger alone. That is why **Hits point at Episodes, never at
   derived elements** ([04-forgetting](04-forgetting.md) §2).
3. **Only the cache layer is ever SET.** Hit cache, hub shortlist,
   `m_cache`, Outbox, selectors. All of it must be deletable and
   regenerable.
4. **There is one time axis: event time.** Stored time exists only on
   Episode and Fact; visibility of Entity, Community and Link is derived
   ([03-time](03-time.md)).
5. **Forgetting is computed, not stored.** The same state and the same clock
   give the same answer whenever it is evaluated. No tick daemon.
6. **The recall handler is read-only, and Hits have one producer path.**
   Every Hit — receipt commit, post-response exposure, extraction re_mention,
   dreaming promotion — goes through the same server-side commit function
   that recomputes the reinforcement ([04-forgetting](04-forgetting.md) §6).
   No caller supplies numbers.
7. **Partial results are never used silently.** When a channel fails, the
   whole channel is dropped and the fact is recorded in diagnostics.
8. **Every constant is a calibration target.** Defaults are literature
   values or explicit assumptions, refitted once the hit ledger has data
   ([04-forgetting](04-forgetting.md) §9).

## Document map

| Document | Question it answers |
|---|---|
| [01-storage](01-storage.md) | What is stored where — layers, elements, link lattice, generations, filesystem |
| [02-daemon-and-pipelines](02-daemon-and-pipelines.md) | Who writes — single writer, RPC, revision, spool, extraction, maintenance, dreaming, security boundary |
| [03-time](03-time.md) | Which world — snapshot(T), derived visibility, change vs correction, INVALIDATES |
| [04-forgetting](04-forgetting.md) | How alive — m₀, S, Hit ledger, Episode attribution, commit protocol |
| [05-recall](05-recall.md) | How to retrieve — candidates → seeds → envelope → PPR → RRF → assembly, degradation ladder |
| [06-envelope-ppr](06-envelope-ppr.md) | How far to look — budgets, fanout, hubs, retained-row normalization, convergence, determinism |
| [07-gds-validation](07-gds-validation.md) | How accurate — solver validation, envelope validation, CI gates |
| [08-repo-and-release](08-repo-and-release.md) | How it is built and shipped |
| [09-roadmap](09-roadmap.md) | In what order |
| [10-decision-log](10-decision-log.md) | Why — decisions from the design review |

## Terms

| Term | Meaning |
|---|---|
| Element | A Neo4j node that is a memory element. `:Element` common label plus exactly one kind label |
| Episode / Fact / Entity / Community | Element kinds: original, derived statement, anchor, topic set |
| Link | A relationship between Elements. Seven roles, real Neo4j relationship types |
| Hit | A ledger record that a memory was actually used. Attached to an Episode |
| generation | A version of the derived layer. An integer per stream for extraction and community; a model id for embedding |
| revision_key | `sha256(origin_key, source_revision)` — one immutable revision occurrence; a later A→B→A revert has a new source revision and a new Episode |
| envelope | The bounded subgraph a single recall actually sees |
| structure_revision | Serving-view revision: changes only when recall-visible structure or selectors change |
| snapshot(T) | The world up to event time T |
| now | Server clock. The reference for forgetting |
