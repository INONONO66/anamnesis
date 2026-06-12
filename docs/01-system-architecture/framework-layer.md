# Framework Layer

The Framework Layer is the official consumer-layer implementation of the Anamnesis crate. It ships as `anamnesis::Memory` and is the default way to use the engine.

## What It Is

`Memory` implements the bench-proven ingest and retrieval recipe on top of the public `Engine` API. It is not a separate system — it is an orchestration wrapper that codifies the encoding strategy validated by the LoCoMo and LongMemEval benchmarks. Every call `Memory` makes goes through the same public `Engine` paths available to any consumer.

**Vocabulary:**

- **Framework API** — `Memory`: the validated consumer layer, ready to use. Namespace: `anamnesis::memory`.
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

`Memory::search` and `search_at` read from `trace.readout` — the same surface the benchmarks measure. Hits are returned as `Vec<Hit>` with `node_id`, `text`, `score`, `at`, `speaker`, and `session` fields. The assembled `ContextPackage` is returned in `Recall.package` for commit-gated reinforcement via `used()`.

## Commit-Gated Used

`used(recall)` calls `engine.commit(recall.package, Some(ConfidenceLevel::Medium))`. Call it only for results actually consumed — reinforcement appends an access trace, raising `B_i` and strengthening co-activated edges.

## Boundary Rules

`Memory` operates within three strict constraints:

1. **Public API only.** No `pub(crate)` backdoors into the engine. Everything `Memory` does is reproducible by any consumer using the same public `Engine` methods.
2. **No LLM calls.** All encoding is deterministic (text formatting + embedding provider). The crate contains no LLM API calls.
3. **Replaceable.** Call `memory.engine_mut()` to drop below the recipe. Mix framework and raw engine calls only when you know what you are doing — the recipe's node topology assumptions no longer apply below that line.

## Benchmark Attribution

The LoCoMo and LongMemEval benchmark harness builds and queries memory through `Memory` (via `Memory::with_provider` + `add` / `flush_all` / `search_result_at_with`). The published numbers are measurements of this layer end-to-end — not a lower-level engine path. See [calibration records](../07-quality-gates/calibration-records.md) for full provenance.
