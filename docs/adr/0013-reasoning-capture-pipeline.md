# ADR-0013: Reasoning Capture Pipeline

## Status

Accepted (implemented 2026-06-28, v0.9.0)

## Context

Anamnesis represents rationale, causality, support, and contradiction as typed
relationships. Explicit `remember`, `relate`, and `ingest_conversation` calls
can create that structure, but a tool call cannot be assumed for every useful
conversation turn.

The core engine remains LLM-free. It can persist source fragments, validate
typed writes, and maintain graph dynamics, but extraction policy and model
execution belong to the consumer boundary. Verbatim private reasoning is not a
formation target; useful outputs are source-grounded decisions, rationales,
causal relations, contradictions, and lessons.

## Decision

Use a two-stage pipeline: best-effort passive source capture followed by
consumer-owned formation.

### Stage 1: passive source capture

Supported client lifecycle events submit bounded transcript windows to the
daemon. The daemon stores accepted text-bearing turns as `Episodic` sources and
deduplicates overlapping delivery by stable turn identity and content hash.

| Event | Claude Code | Codex | Role |
|---|:---:|:---:|---|
| `Stop` | yes | yes | Submit a recent window of at most eight turns |
| `PreCompact` | yes | yes | Submit a wider tail before compaction |
| `SessionEnd` | yes | no | Submit a final tail when the host exposes the event |

Each host adapter declares only events supported by that host. Repeated
delivery is idempotent, but coverage is not absolute: a missing event,
unreadable transcript, filtered non-text turn, timeout, or unreachable daemon
can leave a turn uncaptured. Once the daemon persists a source, later formation
failure cannot remove it.

### Stage 2: consumer-owned formation

Persisted, unprocessed sources enter a namespace-scoped queue. When the queue
crosses its configured threshold, a later `SessionStart` can prompt the agent
to call `extract_pending`. That call returns a bounded batch; the connected
agent may distill source-grounded memories and relations through the normal
`remember` and `relate` surfaces.

Formation is deferred and optional. A client may ignore the nudge, terminate
before writing results, or reject every candidate. Pulled batches use bounded
redelivery, and raw sources remain independently retrievable throughout.

Any configured automated extractor is also a consumer-layer formation path.
Its output must pass the same grounding, provenance, review, and admission
rules as equivalent direct or plugin entry points; it does not write graph
truth merely because a model produced structured output.

## Invariants

- The engine performs no LLM call.
- Passive capture never blocks or alters the host prompt.
- Accepted raw turns remain authoritative source evidence.
- Overlapping capture windows are idempotent.
- Formation output cites exact persisted sources.
- Missing formation output does not retract or mark a raw source invalid.
- Scope and temporal eligibility cannot be widened by a derived record.
- Client event support is explicit; absent events are not treated as delivered.

## Consequences

- Useful conversation evidence can enter memory without requiring a deliberate
  write on every turn.
- Typed reasoning may appear after the source turn rather than in the same
  interaction.
- Capture completeness depends on host lifecycle delivery and daemon
  availability, so operators monitor capture and queue health separately.
- Raw-source volume increases; compact derived records may assist routing, but
  reader-facing evidence retains source provenance.
- Deduplication and bounded redelivery are correctness requirements because
  multiple lifecycle events can submit overlapping windows.

## Relationship to ADR-0015

[ADR-0015](0015-evidence-grounded-formation-and-chain-retrieval.md) proposes a
shared typed formation and admission transaction for derived facts and
relations. It retains this ADR's source-first, consumer-owned boundary while
making source references, validation, review state, and routing isolation
explicit. Until an implementation satisfies ADR-0015's promotion criteria and
is released, the shipped capture and extraction surfaces remain authoritative.
