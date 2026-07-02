# ADR-0013: Reasoning capture pipeline — passive raw ingest + agent-side batch extraction

## Status

Accepted (implemented 2026-06-28, v0.9.0)

## Context

anamnesis's differentiator is that it preserves **reasoning** — `Causal` / `Reason` /
`Contradicts` / `Supports` edges that record *why*, not just *what* (the edge type is a
first-class propagation signal, see ADR-0005 / ADR-0006).

But today every **write** path depends on the agent *voluntarily* calling an MCP tool
(`remember` / `relate` / `ingest_conversation`). The plugin's hooks are **read-only recall**
only (ADR-0011) — there is no capture hook at all. Consequences:

- If the agent never calls `relate`, **no reasoning edge is ever created.**
- The automatic path (the `Memory` recipe) turns a turn into `Episodic` + `ExtractedFrom` /
  `Temporal` edges only — flat facts, no causal/decision/contradiction structure.

So **anamnesis claims to be reasoning-first, yet reasoning capture is not guaranteed** — it
falls back to a flat fact graph whenever the agent doesn't self-annotate.

Two constraints shape the fix:

1. Storing **raw chain-of-thought** verbatim is wrong — noise + volume blow-up. The thing
   worth keeping is the *distilled result structure*: decisions+rationale, cause→effect,
   contradictions, problem→resolution.
2. Extracting that structure needs an **LLM**, but the anamnesis engine is deliberately
   **LLM-free** — extraction is the consumer's job, and the daemon owns the engine.

## Decision

A **two-stage** capture pipeline. Raw text is saved synchronously by hooks; reasoning
structure is extracted in batch by the agent, not the engine.

### Stage 1 — passive raw ingest (no LLM)

The plugin's capture hook reads the turn (user + assistant) from the transcript and `ingest`s
it as `Episodic` into the daemon. **Idempotent**: the daemon dedups by turn id/hash, so the
same turn arriving from multiple hooks is harmless.

Hook event matrix (measured against CC and the Codex 0.142 binary):

| event       | Claude Code | Codex | role |
|-------------|:--:|:--:|--|
| `Stop`         | ✅ | ✅ | per-turn recent-window ingest (≤8 turns; best-effort) |
| `PreCompact`   | ✅ | ✅ | flush + extraction trigger before context compaction |
| `SessionEnd`   | ✅ | ❌ | end-of-session flush (CC only) |

`Stop` captures a recent window (≤8 turns) every turn; `PreCompact` / `SessionEnd` flush a
wider tail (≤50) before the window is compacted or closed. Dedup in the daemon makes the
per-`Stop` overlap harmless. **`Stop` capture is best-effort given tool-turn filtering** (the
transcript parser drops `tool_use`/`tool_result` entries, so the last 2 text-bearing turns may
both be `assistant`); `PreCompact` / `SessionEnd` are the real backstop for full coverage.
**Codex does not support `SessionEnd`** (absent from the binary; `PreCompact`/`PostCompact` are
present) — it MUST be omitted from `codex-hooks.json`, because Codex's strict hook parser rejects
unknown event keys and kills the whole hook file (cf. the `description`-field bug, #79). On Codex
the `SessionEnd` gap is covered by `Stop` (recent window each turn) + `PreCompact` (long sessions).

### Stage 2 — agent-side batch extraction (client LLM)

The daemon holds only an **un-extracted queue** of ingested turns. Extraction is performed by
the **next agent that connects**, using *its own* LLM: it pulls the queue, distills
decisions / cause→effect / contradictions / problem→resolution, and emits them as
`relate` / `remember` calls.

Trigger: the **daemon's accumulation threshold** (N un-extracted turns — the *guarantee*) plus
`PreCompact` / `SessionEnd` hook signals (best-effort, earlier flush). The engine and daemon stay
LLM-free; if extraction fails or no agent connects, the **raw `Episodic` still survives**
(fail-open).

## Consequences

- ✅ Closes the gap where reasoning capture depended 100% on agent volition. Raw is always safe
  (passive), reasoning structure is filled in by batch.
- ✅ Engine/daemon stay LLM-free and local-first — extraction is the client's responsibility,
  consistent with the existing "extraction is the consumer's job" principle.
- ⚠️ Extraction is **not immediate** — it lags until the next client connection / threshold.
  Raw is immediate; only the reasoning edges are deferred.
- ⚠️ Codex `SessionEnd` gap — accepted, covered by `Stop` + `PreCompact`.
- **Idempotent dedup is mandatory** (multi-hook). Daemon dedups by turn id; without it the
  multi-hook fan-out becomes a duplication storm.
- Raw `Episodic` volume rises, so the **readout must weight raw originals low** (provenance,
  not recall) to avoid drowning the readout — ties into the recall-weight≠evidence-weight split
  (ADR-0010 calibration; the one external-feedback item worth adopting).
