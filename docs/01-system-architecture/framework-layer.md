# Framework Layer

The Framework Layer is the official consumer-layer implementation of the
Anamnesis crate. It ships as `anamnesis::Memory` and is the default way to use
the engine.

## What It Is

`Memory` implements the canonical conversation-ingest and reranked-recall
contract on top of the public `Engine` API. It is not a separate system: every
operation goes through the same public engine paths available to a direct crate
consumer.

**Vocabulary:**

- **Framework API** — `Memory`: the canonical consumer layer, ready to use. Namespace: `anamnesis::memory`.
- **Kernel API** — `Engine`: the raw substrate; all mechanics, no encoding opinion. Namespace: `anamnesis::engine`.

## The Recipe

`Memory` applies a fixed encoding strategy to conversational turns:

| Step | What happens |
|:-----|:-------------|
| **Episodic node** | Each turn is ingested as `KnowledgeType::Episodic` with content `"{speaker}: {text}"`. |
| **Semantic node (±1 window)** | Each turn's Semantic view is the three-line window `prev_turn\ncur_turn\nnext_turn` (one-sided at session boundaries). Ingested as `KnowledgeType::Semantic`. |
| **ExtractedFrom edge** | Each Semantic node is linked to its source Episodic node via `EdgeType::ExtractedFrom`. |
| **Temporal edge** | Each Episodic node is linked to the next via `EdgeType::Temporal`. |
| **Entity tags** | Every node carries `session-{norm}` and `speaker-{norm}` tags (normalized: lowercase, spaces/colons/underscores → hyphens). |
| **Engine config** | `dedup_enabled = false`, `novelty_threshold = 0.0`, `confidence_threshold = 0.0` — the framework contract is "remembers what you add". Surprise-gating remains an `Engine`-level feature for consumers who opt in. |

## Buffering Semantics

`Memory` is incremental — the "+1 future turn" does not exist at `add` time. The recipe is replicated exactly via **one-turn buffering** per session:

- `add(session, speaker, text, at)` ingests the Episodic node immediately. If a buffered turn exists, its Semantic window is now complete and is ingested and linked.
- `flush_session` / `flush_all` finalize the last buffered turn with a one-sided window (no `+1` to append).

**Flush-boundary caveat:** The final turn of any session has a one-sided Semantic window until flushed. `search` and `search_at` auto-flush before executing the query, so all turns are always searchable. `Drop` also flushes, but swallows errors — call `flush_all()` explicitly before dropping if you need to observe flush errors.

## Readout Surface

`Memory::search` and `search_at` read from the engine's pre-packaging readout
surface. Hits are returned as `Vec<Hit>` with `node_id`, `text`, `score`, `at`,
`speaker`, and `session` fields. The assembled `ContextPackage` is returned in
`Recall.package` for commit-gated reinforcement via `used()`.

`Memory::search_reranked` is the canonical quality-oriented path. It collects a
bounded source surface, applies the configured reranker, selects source-aware
evidence, and returns the exact `RecallPlan` beside the packaged result. Shipped
MCP, hook, and plugin recall clients use this path and pass the retained plan to
`render_context_for_plan_with`, so rendering does not re-infer a potentially
different policy. Direct crate integrations can use the same calls without
reproducing ranking, selection, packaging, or reader-guidance policy.

Consumers whose reranker is an external service or process use the bound
two-step form: `prepare_rerank_for_plan_at` exposes the exact ordered scoring
texts through `PreparedRerank::rerank_texts`, and
`complete_prepared_rerank` validates scores and consumes that receipt on its
originating `Memory`. The unbound document and repackaging methods are diagnostic
surfaces, not a production handoff across an external provider call.

## Proposed Result Extension

The additive evidence-complete extension proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
would keep this small surface. Canonical entity/fact indexes and evidence chains
would remain internal retrieval capabilities; consumers would receive an
`EvidenceBundle` through an additive result field or API and could continue
using `ContextPackage` as its compatibility rendering. These types and paths are
not implemented by the current `Memory` API.

## Commit-Gated Used

`used(recall)` calls `engine.commit(recall.package, Some(ConfidenceLevel::Medium))`. Call it only for results actually consumed — reinforcement appends an access trace, raising `B_i` and strengthening co-activated edges.

## Boundary Rules

`Memory` operates within three strict constraints:

1. **Public API only.** No `pub(crate)` backdoors into the engine. Everything `Memory` does is reproducible by any consumer using the same public `Engine` methods.
2. **No LLM calls.** All encoding is deterministic (text formatting + embedding provider). The crate contains no LLM API calls.
3. **Replaceable.** Call `memory.engine_mut()` to drop below the recipe. Mix framework and raw engine calls only when you know what you are doing — the recipe's node topology assumptions no longer apply below that line.

## Evaluation Parity

Quality harnesses build raw conversation memory and run qualifying live
reranker output through `search_reranked_for_plan_at` and plan-aware rendering.
They may run a separate read-only source search after that package and its
latency are frozen to measure candidate and feature surfaces; the diagnostic
search must not alter or replace the product result. Evaluation adapters may
convert input records and collect diagnostics, but they must not own an
alternate ranking or packaging policy. Every published result declares any
derived artifact separately; replaying such an artifact does not establish
parity with a plugin extraction or review workflow. See
[benchmarks](../07-quality-gates/benchmarks.md) and
[calibration records](../07-quality-gates/calibration-records.md).
