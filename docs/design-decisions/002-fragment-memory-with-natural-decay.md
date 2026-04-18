# ADR-002: Fragment-Based Memory with Natural Decay and Revival

**Status**: Accepted

## Context

Designing the knowledge representation model for LLM agent memory. The engine needs to decide:

1. **Granularity**: What unit of knowledge to store (summaries, facts, full conversations, or individual fragments)
2. **Lifecycle**: How knowledge ages and when it becomes irrelevant
3. **Recovery**: Whether aged-out knowledge can be recovered

Options considered:

1. **Extracted facts** (Mem0 approach) — Summarize conversations into key-value facts, retrieve by vector similarity
2. **Tiered text storage** (Letta approach) — Store conversation history in core/archival tiers, retrieve by recency and search
3. **Evolving playbook** (Stanford ACE approach) — Maintain a single evolving document that a Curator rewrites after each session
4. **Fragment graph with natural decay** — Store individual conversation turns as graph nodes, connect with typed edges, decay salience over time, revive on access

## Decision

Option 4: Fragment-based graph with natural decay and revival.

Each conversation turn is stored as an individual node (fragment) in the graph. Nodes have a salience score (0.0–1.0) that decays over time via `tick()`. Nodes below a query threshold become invisible to spreading activation queries but remain in the graph. Precise mention via `touch()` reactivates decayed nodes.

## Rationale

- **Fragments preserve reasoning context** — summaries are lossy by definition. When a fact is extracted from "we tried X but it failed because Y, so we chose Z," the reasoning chain (Y) and rejected alternative (X) are lost. Fragments retain the full context.
- **Natural decay is self-maintaining** — no human intervention needed to clean up stale knowledge. The formula `salience * decay_rate^elapsed_days` progressively filters noise. Knowledge that matters gets reinforced through `touch()` on access.
- **Revival prevents permanent loss** — unlike pruning or archival, decayed nodes are still traversable when directly cued. This matches human memory: you can't recall a detail spontaneously, but when someone mentions it, the memory returns along with its connected context.
- **Consolidation is additive, not destructive** — when the consumer's Curator detects repeated patterns across fragments, it creates a new semantic/consolidated node linked via `CONSOLIDATED_FROM` edges. The original fragments remain. This avoids the brevity bias problem (ACE's known weakness where playbooks shrink over time).
- **Graph structure enables associative retrieval** — fragments connected by typed edges allow spreading activation to discover non-obvious relationships across sessions and domains. Vector similarity alone cannot represent "X was rejected in favor of Y because of Z."

## Consequences

- **Storage grows with usage** — every conversation turn creates nodes. Mitigation: perception gating filters low-value observations before ingestion; salience decay makes most nodes invisible to queries.
- **Consumers must implement extraction** — the engine stores whatever is ingested. Quality of the knowledge graph depends on how well the consumer's Generator role extracts meaningful nodes and creates typed edges.
- **Query budget is essential** — without budget constraints, spreading activation could traverse the entire graph. The `budget` parameter in `query()` is not optional — it is a core design constraint.
- **Decay parameters need tuning** — decay rate, query threshold, and reinforcement amount affect the balance between retention and noise. These are configurable via `EngineConfig`.
