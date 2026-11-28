# 00 — Overview

> Docs 00 to 08 describe the contracts the code implements. Unbuilt work is
> listed only in [09-roadmap](09-roadmap.md). Why each rule exists is in the
> [10-decision-log](10-decision-log.md), and superseded history is in
> [history/decision-log-archive](history/decision-log-archive.md).
> Comparisons and field lessons live in `docs/background/`; dated
> experiments live in `docs/research/`. No number in this set is a production
> measurement.

## One line

anamnesis2 is a personal memory engine that uses **Neo4j as its only graph
and index store** and does the numeric work of remembering (spreading
activation, forgetting, fusion) **in TypeScript memory, within fixed
per-request bounds**. Neo4j GDS is never on the online path; it is an offline
accuracy baseline only.

```text
  Neo4j                                          <runtime root>/
    ├─ originals: Episode · Payload · Hit           ├─ objects/   payload bytes (part of the authority)
    ├─ control:   Usage · UsageOutcome · PolicyEvent│
    │             Episode lineage · identity decisions
    ├─ derived:   Fact · Entity · Link · Fact vectors (one extraction generation selected)
    ├─ caches:    HitCache · ConductingArc · session topology · room/space · REPLIES_TO
    └─ fulltext (Lucene, cjk) · vector (HNSW) candidates
              │
              v
  TypeScript dynamics (inside anamnesisd, per request)
    ├─ local PPR over seed-adjacent ConductingArc rows (CSR, Float64Array)
    ├─ forgetting m(now) = m₀ · R(t, S), no tick
    ├─ RRF fusion × mass weighting × (1 + β·U)
    ├─ policy filter · CONTRASTS companions · exact budget (bytes / scalars / pinned tokens)
    └─ snapshot(T) · INVALIDATES · provenance assembly · Usage
              │
              v
  Neo4j GDS (offline, disposable container)
    └─ solver accuracy baseline
```

## Three separations

| Component | Responsible for | Not responsible for |
|---|---|---|
| **Neo4j** | durable memory, indexes, relationship pattern matching, bounded neighborhood reads | iterative numeric computation, score fusion |
| **TypeScript dynamics** | bounded per-request spreading, forgetting and fusion | global graph analysis, durable state |
| **GDS** | offline solver baseline | online recall |

The central trade-off: the engine **gives up full-graph PPR** for the
**predictable latency, determinism and local-first operation** of a strictly
bounded local PPR. How much quality that costs is not measured; the comparison
is a roadmap item.

Two more separations hold throughout. Raw collection stays outside the graph:
the archive is the collection source of truth and Neo4j holds memory (D56).
Event time and the server clock are different axes: `snapshot(T)` asks what
was true at T, forgetting asks how retained something is now (docs/03,
docs/04).

## Invariants — rules that span every document

1. **The originals layer is CREATE-only.** Episode, Payload, Hit,
   HAS_PAYLOAD, HIT_OF and revision INVALIDATES are never modified or deleted
   once written. NEXT_EPISODE is a rebuildable cache because event-time
   backfill must rewire session order. Mistakes are fixed by new events. The
   Episode digest has two versions: an Episode stored without
   `episode_digest_version` stays byte-verified as version 1 and is never
   rewritten, reserialized or given lineage in place
   ([01-storage](01-storage.md) §1). The data authority is the Neo4j database
   plus `objects/`, nothing else (§9).
2. **The derived layer is regenerable from the originals.** Facts, Entities,
   derived links and Fact vectors belong to an extraction generation, and a
   new generation re-extracts from Episodes. That is why **Hits point at
   Episodes, never at derived elements** ([04-forgetting](04-forgetting.md)
   §2).
3. **Only caches and cursors are SET.** HitCache, Entity witness markers,
   ConductingArc and its coverage, room/space labels, the identity lane's
   caches, extraction coverage and Meta revisions. Every cache is deletable
   and rebuildable. Pipeline bookkeeping is not in the graph: it lives in
   `embedding-state.json` (D54), `extraction-state.json` (D55) and
   `materialization-state.json`, with structured audit log lines as the
   durable record. ConductingArc is nonsemantic access metadata; incomplete
   coverage drops PPR and never falls back to native adjacency
   ([01-storage](01-storage.md) §5).
4. **There is one semantic time axis: event time.** Only Episode and Fact
   carry it, and `snapshot(T)` reads only it ([03-time](03-time.md)).
   Operational clocks (`ingested_at`, Hit time, Usage time, policy revisions)
   record when the system acted. The one exception is the recall `known_by`
   predicate over `ingested_at` (D57).
5. **Forgetting is computed, not stored.** The same state and the same clock
   give the same answer whenever it is evaluated. No tick daemon.
6. **Only explicit reports create Hits.** Recall writes one Usage before
   returning. `report_outcome` allocates signed credit to the selected source
   Episodes and updates stability and utility
   ([04-forgetting](04-forgetting.md) §5). Socket delivery has no effect on
   either.
7. **Accessibility and utility are separate quantities.** `S` is a stability;
   accessibility `A = R(now − t_last_hit, S)` is how easily a memory comes back
   now; the per-Episode utility `U` answers "did it help when it came back". A
   missing outcome is not a zero. Hits attach to Episodes only, so crediting
   one Fact refreshes its sibling Facts from the same source; that collateral
   refresh is accepted, not hidden.
8. **Policy suppresses, it does not erase.** `policy.set` and `policy.revoke`
   append immutable policy events. A denied source can't support any visible
   result, in extraction, candidates, assembly, provenance text or outcome
   credit. Originals stay on disk and there is no erasure guarantee.
   Instructions inside ingested text are never executed as policy
   ([01-storage](01-storage.md) §3.2, D43).
9. **Partial results are never used silently.** A failed channel is dropped
   whole and reported in diagnostics. A bundle that doesn't fit the budget is
   skipped whole, never truncated, and Usage records only the included
   primaries ([05-recall](05-recall.md), D44).
10. **No echo is counted twice and no vector is faked.** An assistant turn that
    received anamnesis context declares its parent Usage IDs; the resulting
    Facts carry bounded lineage roots that never raise confidence, mass,
    utility, rank or a conflict winner. An assistant Episode with unknown or
    incomplete lineage is stored but produces no semantic output. An embedding
    input is never truncated, chunked or zero-filled to produce a vector
    ([01-storage](01-storage.md) §3.3, §4, D49).
11. **Every constant is a calibration target.** Defaults are literature values
    or explicit assumptions ([04-forgetting](04-forgetting.md) §9). An
    immutable `m₀` changes only through a new derived generation.

## Document map

| Document | Question it answers |
|---|---|
| [01-storage](01-storage.md) | What is stored where: layers, elements, policy, lineage, generations, links, filesystem, backup |
| [02-daemon-and-pipelines](02-daemon-and-pipelines.md) | Who writes: the single writer, RPC, remember, extraction, security boundary |
| [03-time](03-time.md) | Which world: snapshot(T), validity, INVALIDATES, `known_by` |
| [04-forgetting](04-forgetting.md) | How alive and how useful: m₀, S, Episode credit, outcome admission, utility U |
| [05-recall](05-recall.md) | How to retrieve: candidates, PPR, RRF, assembly, budget, Usage, degradation |
| [06-envelope-ppr](06-envelope-ppr.md) | The bounded graph read, DegreeProbe, normalization, convergence, determinism |
| [07-gds-validation](07-gds-validation.md) | How accurate: solver validation, gates, utility evaluation |
| [08-repo-and-release](08-repo-and-release.md) | How it is built and shipped |
| [09-roadmap](09-roadmap.md) | What is not built yet |
| [10-decision-log](10-decision-log.md) | Why |

## Terms

| Term | Meaning |
|---|---|
| Element | A Neo4j node that is a memory element: the `:Element` label plus exactly one kind label |
| Episode / Fact / Entity | Element kinds: original, derived statement, anchor. `Community` is a reserved kind with no writer |
| Account / Person | Identity layer, not Elements: one `(origin_source, origin_actor)` speaker, and a group of accounts judged to be one person (D59) |
| Link | A relationship between Elements. Eight roles, real Neo4j relationship types; `REPLIES_TO` is a nonconducting Episode cache (D58) |
| Hit | One signed outcome credit for one source Episode, allocated from a UsageOutcome |
| Usage | The ordered primary selection, source Episodes and bound lineage of one recall. At most one UsageOutcome supplies all its Episode credit |
| U | Outcome utility, clipped after each event and decayed with a 30-day half-life. Enters score as `(1 + 0.25 U)` |
| policy | A `deny` or `revoke` command with an `episode_id` and/or `source` selector and `content` scope. Suppresses serving and extraction; never erases originals |
| budget | Optional recall bound `{unit: utf8_bytes \| unicode_scalars \| tokens, limit, tokenizer_id?}`. Exact counts of the final `context_text`; `tokens` needs an installed, digest-pinned tokenizer, no estimates |
| epistemic | `observed \| derived`: derivation distance from the source utterance. Not a trust probability |
| generation | A version of the derived layer, one `ExtractionGeneration`; the selector picks the one that serves |
| revision_key | `sha256(origin_key, source_revision)`: one immutable revision occurrence; a later A→B→A revert has a new source revision and a new Episode |
| ConductingArc | Rebuildable per-endpoint physical-link access cache keyed by `(source_id, link_id)`; not an Element, authority, candidate, conductor or output |
| structure_revision | Serving-view revision: changes only when recall-visible structure or the selector changes |
| snapshot(T) | The world up to event time T |
| now | Server clock. The reference for forgetting |
| `fact_language_policy` | Extraction-generation setting, `source` or `en`. Chooses the language of Fact content for that whole generation; never an in-place rewrite and never applied to quotes, spans, names or identifiers (D48) |
| corroboration root | An original Episode ID in a Fact's bounded `corroboration_root_episode_ids` (at most 16). Provenance accounting only: roots never add candidates, Hits, mass or score terms (D49) |
| embedding profile | The SHA-256 of one canonical embedding profile; it names the vector label and index (`vec_fact_<profileId>`) |
