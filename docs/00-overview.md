# 00 — Overview

> This document set (docs/00–10) is the normative design for anamnesis2.
> Code follows the docs. The previous drafts (docs/00–11, 2026-08) are
> retired; the reasons and the decisions that replaced them are recorded in
> [10-decision-log](10-decision-log.md). Non-normative material (competitor
> comparison, field lessons) lives in `docs/background/`.
>
> **Target, not runtime status.** Everything here describes the design the
> code must reach. What actually runs today is the short list in
> [09-roadmap](09-roadmap.md) ("Where the code is today"); no number in this
> set is a production measurement, and the two frozen development screens
> behind the D48 and D50 defaults are reported with their limits in
> [07-gds-validation](07-gds-validation.md) §7.

## One line

anamnesis2 is a personal memory engine that uses **Neo4j as its only graph
and index store**, and performs the repeated numeric work of remembering —
spreading activation, forgetting, fusion — **in TypeScript memory, within
strict per-request bounds**. Neo4j GDS is not on the online path; it is used
for offline analysis and as an accuracy baseline only.

```text
  Neo4j                                          ~/.anamnesis/
    ├─ originals: Episode · Hit ledger              ├─ objects/   payload bytes (content-addressed, part of the authority)
    │             policy Episodes                   ├─ spool/     transient remember() queue while Neo4j is down
    ├─ control:   RecallReceipt (append-only)       └─ neo4j/     container volume
    │             InvalidationEvidence · EchoLineage
    │             adjudication + embedding records (append-only)
    ├─ derived:   Fact · Entity · Community
    │             Link · embedding (generations)
    ├─ caches:    hit cache · utility cache · active policy · ConductingArc · hub shortlist
    ├─ vector (HNSW) · fulltext (Lucene) candidates
    └─ bounded envelope extraction
              │
              v
  TypeScript dynamics (inside anamnesisd, per request)
    ├─ local PPR  (CSR, Float64Array, ≤ 2,000 nodes / 20,000 links)
    ├─ forgetting m(now) = m₀ · R(t, S)  — no tick
    ├─ RRF fusion × mass weighting × (1 + β·U)   U = outcome utility, separate from S
    ├─ policy filter · conflict bundles · exact budget (bytes / scalars / pinned tokens)
    └─ snapshot(T) · INVALIDATES · provenance assembly · RecallReceipt
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
truncation costs is unknown today. The design obligates itself to measure
it against a pinned GDS baseline rather than estimate it; no measurement
exists yet, and the gates that will produce one are in
[07-gds-validation](07-gds-validation.md).

## Invariants — rules that span every document

1. **The originals layer is CREATE-only.** Episode, Payload, Hit,
   HAS_PAYLOAD, HIT_OF and revision INVALIDATES are never modified or deleted
   once written. NEXT_EPISODE is a rebuildable cache because event-time
   backfill must rewire session order. Mistakes are fixed by events
   (INVALIDATES). D49's digest change is prospective for the same reason: an
   Episode stored without `episode_digest_version` stays byte-verified as
   version 1 and is never rewritten, reserialized or given lineage in place,
   while only newly admitted revisions store version 2. There is no migration
   ([01-storage](01-storage.md) §1, §9). The data authority is the Neo4j
   database plus `~/.anamnesis/objects/`, nothing else.
2. **The derived layer is regenerable.** Fact, Entity, Community, Link and
   embeddings must be rebuildable at any time from the originals layer, the
   Hit ledger, the retained content-free `InvalidationEvidence` ledger, the
   retained `EchoLineage`, `AdjudicationReview` / operator-adjudication
   Episode, `AdjudicationCorrection` and `EmbeddingResolution` control
   records, and the explicit old/new target mappings of each rebuild.
   Originals and Hits alone are not enough: a denied invalidator's text may
   leave the serving view, and its established invalidation outcome must
   still be replayed; an accepted operator repair or embedding skip is a
   decision no re-extraction can rediscover
   ([01-storage](01-storage.md) §4, D49–D51). That is why **Hits point at
   Episodes, never at derived elements**
   ([04-forgetting](04-forgetting.md) §2).
3. **Only the cache layer is ever SET.** Hit cache, utility cache, active
   policy, Entity witness rows, ConductingArc, hub shortlist, `m_cache`,
   Outbox, selectors. All of it must be deletable and regenerable.
   ConductingArc is nonsemantic endpoint access metadata rebuilt from retained
   physical links and maintained atomically with them. Its composite
   (source_id, link_id) index supplies ordered first-256 probes before filters;
   missing/incomplete coverage drops PPR, never falls back to native adjacency
   ([01-storage](01-storage.md) §5, [06-envelope-ppr](06-envelope-ppr.md) §2).
4. **There is one semantic time axis: event time.** Semantic time, the time
   a memory is about, is stored only on Episode and Fact; visibility of
   Entity, Community and Link is derived from it, and `snapshot(T)` reads
   only this axis ([03-time](03-time.md)). Operational clocks are separate
   and never enter snapshot: `Episode.ingested_at`, `Hit.t`, RecallReceipt
   timestamps and TTL, and policy revisions record when the system did
   something, and are read by forgetting, feedback windows and audit.
5. **Forgetting is computed, not stored.** The same state and the same clock
   give the same answer whenever it is evaluated. No tick daemon.
6. **The recall handler is read-only, and Hits have one producer path.**
   Every Hit (receipt commit with adoption and outcome, post-response
   exposure, extraction re_mention, dreaming promotion) goes through the same
   server-side commit function ([04-forgetting](04-forgetting.md) §6). Only
   `recall_hit`, a confirmed adoption, updates `S` and `t_last_hit`. Exposure,
   re_mention, promotion and outcome are audit-only for accessibility.
   Reinforcement is always positive and capped at `S_max`; nothing lowers `S`.
7. **Accessibility and utility are separate quantities.** `S` is a
   stability, the time scale over which retention `R(t, S)` decays;
   accessibility `A = R(now − t_last_hit, S)` is how easily a memory comes
   back right now; the per-Episode utility `U` answers "did it help when it
   came back". An outcome reward `r ∈ [−1,1]` feeds `U` through
   rank-weighted attribution recorded in the RecallReceipt; it never rewinds
   time, never resets `S`, and a missing outcome is not a zero. Hits attach to
   Episodes only, so adopting one Fact refreshes its sibling Facts from the
   same source; that collateral refresh is accepted, not hidden
   ([04-forgetting](04-forgetting.md), D45).
8. **Policy suppresses, it does not erase.** `policy.set` / `policy.revoke`
   append `anamnesis.memory-policy/1` Episodes; the active policy is a
   rebuildable cache. A denied source can't support any visible derived
   result, in extraction, dreaming, candidates, conduction, assembly or
   provenance text. Originals stay on disk; there's no `gc --erase` and no
   erasure guarantee. Instructions found inside ingested text are never
   executed as policy ([02-daemon-and-pipelines](02-daemon-and-pipelines.md),
   D43).
9. **Partial results are never used silently.** When a channel fails, the
   whole channel is dropped and the fact is recorded in diagnostics. A
   result that doesn't fit the caller's budget is skipped whole, never
   truncated, and the receipt lists exactly what was delivered
   ([05-recall](05-recall.md), D44). Contradiction companions come from a
   bounded scan (at most 64 raw `CONTRASTS` rows, first 4 eligible); when
   that scan can't see everything, the bundle carries a mandatory incomplete
   warning and no count is reported, rather than a guessed total
   ([05-recall](05-recall.md), D46).
10. **No echo is counted twice and no vector is skipped in silence.** An
   assistant turn that received anamnesis context declares its parent
   receipts; the resulting Facts carry bounded lineage and corroboration
   roots that never raise confidence, mass, utility, rank or a conflict
   winner. Unknown or incomplete assistant lineage is stored but never served
   as a semantic candidate, and that applies to the assistant Episode itself
   as much as to anything derived from it. An embedding entry that cannot produce a
   vector blocks its model's contiguous coverage until an authenticated
   retry or skip resolves it; nothing is truncated, chunked or zero-filled
   to let the cursor move ([01-storage](01-storage.md) §4,
   [05-recall](05-recall.md), D49, D51).
11. **Every constant is a calibration target.** Defaults are literature
   values or explicit assumptions, refitted once the hit ledger has data
   ([04-forgetting](04-forgetting.md) §9). Priors that enter `m₀` change only
   through a new derived generation, never by rewriting a stored `m₀`.

## Document map

| Document | Question it answers |
|---|---|
| [01-storage](01-storage.md) | What is stored where — layers, elements, link lattice, generations, filesystem |
| [02-daemon-and-pipelines](02-daemon-and-pipelines.md) | Who writes — single writer, RPC, revision, spool, extraction, policy RPCs and barrier, maintenance, dreaming, security boundary |
| [03-time](03-time.md) | Which world — snapshot(T), derived visibility, change vs correction, INVALIDATES |
| [04-forgetting](04-forgetting.md) | How alive, how useful — m₀, S, Hit ledger, Episode attribution, commit protocol, outcome utility U |
| [05-recall](05-recall.md) | How to retrieve — candidates → seeds → envelope → PPR → RRF → policy filter → conflict bundles → budget → assembly, receipts, degradation ladder |
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
| Hit | A ledger record that a memory was actually used. Attached to an Episode. Kinds: `recall_hit` (adoption, the only one that moves `S`), `exposure`, `re_mention`, `promotion`, `outcome` |
| RecallReceipt | Append-only control record of one recall: delivered primary and companion IDs, source snapshots, versions, channels, ranking state, budget and result digests. Exactly one adoption per source and one outcome per recall; expires by explicit TTL. Not a semantic Episode |
| U | Per-Episode outcome utility, `U = (ν·μ₀ + Σ w·r) / (ν + Σ w)`. A reported proxy, not truth or causal attribution. Enters score as `(1 + β·U)`, never enters mass |
| policy | A `deny` or `revoke` command with at least one selector (`subject_entity_id`, `schema`, `sub_kind`, `modality`, `literal`) and a scope (`derived` or `content`). Suppresses ordinary extraction and serving; never erases originals |
| budget | Optional recall bound `{unit: utf8_bytes \| unicode_scalars \| tokens, limit, tokenizer_id?}`. Exact counts of the final `context_text`; `tokens` needs an installed, version-pinned tokenizer, no estimates |
| epistemic | `observed \| extracted \| synthesized`: provenance distance from the source utterance. Not a trust probability |
| generation | A version of the derived layer. An integer per stream for extraction and community; an `embedding_profile_id` for embedding |
| revision_key | `sha256(origin_key, source_revision)` — one immutable revision occurrence; a later A→B→A revert has a new source revision and a new Episode |
| envelope | The bounded subgraph a single recall actually sees |
| ConductingArc | Rebuildable per-endpoint physical-link access cache, keyed by (source_id, link_id); not an Element, authority, candidate, conductor or output |
| structure_revision | Serving-view revision: changes only when recall-visible structure or selectors change |
| snapshot(T) | The world up to event time T |
| now | Server clock. The reference for forgetting |
| `fact_language_policy` | Extraction-generation configuration, `source` or `en`. Chooses the language of Fact content for that whole generation; never an in-place rewrite of an existing Fact, and never applied to quotes, spans, names or identifiers (D48) |
| corroboration root | An original Episode ID in a Fact's bounded `corroboration_root_episode_ids` (at most 16). Provenance accounting only: roots gate policy checks and never add candidates, Hits, mass or score terms (D49) |
| adjudication proposal | An immutable proposed verdict from the shadow-mode adjudicator, with its own attempt row and candidate digest. Creates no Fact or edge until an authenticated review accepts it and its named generation consumes it once (D50) |
| embedding profile | `sha256` over one exact `(embedding_model_id, vector_index_id)` pair; the value of `active[embedding]`. ONLINE indexes are not an approved profile: activation needs a recorded `EmbeddingQualification` (D51) |
