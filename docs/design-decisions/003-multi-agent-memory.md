# ADR-003: Multi-Agent Memory Support

**Status**: Partially Accepted

## Context

Anamnesis was originally designed as a single-agent cognitive graph — one agent ingests, one agent queries. In practice, orchestration systems run multiple specialized agents (architect, coder, reviewer, etc.) that operate on the same codebase and would benefit from sharing a knowledge graph.

Analysis of MiroFish (a multi-agent social simulation platform with shared memory) revealed three patterns relevant to Anamnesis:

1. **Origin tracking** — MiroFish agents tag every memory with who produced it, enabling conflict resolution when agents disagree.
2. **Social reinforcement** — Knowledge confirmed by multiple independent agents is treated as more reliable than single-agent observations.
3. **Cross-agent linking** — After a round of parallel execution, agents' observations about the same entities need to be connected.

These patterns address a gap: without them, a shared Anamnesis graph cannot distinguish "two agents independently confirmed X" from "one agent said X twice."

## Decision

Extend the Anamnesis design with three features:

### 1. Origin Attribution ✅

Add an `Origin` struct to every node:

```rust
struct Origin {
    agent_id: String,           // which agent produced this
    session_id: String,         // from which session
    scope: Option<String>,      // hierarchical scope path; None = universal
    confidence: f64,            // certainty at creation time
}
```

Origin is metadata, not access control. It enables the consumer-side Reflector to resolve contradictions and weight expertise by source.

**Implementation Status**: ✅ Accepted direction. Origin carries provenance for every node. Scope-aware recall should use a hierarchical scope path rather than a project-only identifier.

### 2. Social Reinforcement ✅

Extend gravity scoring with a logarithmic bonus for multi-agent corroboration:

```
social_bonus(node) = 1.0 + ln(distinct_agent_count)   // only if > 1
```

- Only distinct `agent_id` values count (same agent, different sessions = no bonus).
- Logarithmic scaling prevents popularity cascades.
- Composes with existing decay and reinforcement mechanics — does not replace them.

**Implementation Status**: ✅ Social corroboration scoring primitives and consumer feedback salience updates are implemented. Automatic gravity multiplier wiring is not part of the current engine behavior.

### 3. Batch Reflect ✅

Add a round-boundary API for cross-agent entity linking:

```rust
pub fn reflect_batch(&mut self, sessions: &[SessionSummary]) -> Result<ReflectReport, Error>;
```

- Groups nodes from completed sessions by shared entities (metadata matching, no LLM).
- Creates `Entity` edges between nodes from different agents referencing the same concept.
- Does not merge nodes or alter salience — only creates edges for discoverability.

**Implementation Status**: ✅ Implemented as metadata-only cross-agent entity linking. The method creates `Entity` edges between nodes from different agents that share entity tags and returns a `ReflectReport`.

### Dependency Chain

```
Origin → Social Reinforcement (needs agent_id to count distinct agents)
Origin → Batch Reflect (needs agent_id to identify cross-agent nodes)
```

## Rationale

- **Origin is cheap**: Adding metadata to nodes has zero runtime cost for single-agent use and unlocks the other two features.
- **Social reinforcement fits existing mechanics**: Gravity already computes centrality; social bonus is a multiplier, not a new system.
- **Batch Reflect stays within engine boundaries**: It creates edges using metadata matching — no LLM calls, no external dependencies. This is consistent with ADR-001's principle that cognitive mechanics are application-layer logic.
- **All three are backward-compatible**: A single-agent graph with no Origin metadata behaves identically to today's engine. Multi-agent features activate only when Origin is present.

## Alternatives Considered

1. **Full agent simulation (MiroFish approach)**: Simulate agent profiles and social dynamics within the memory engine. Rejected — Anamnesis is a cognitive graph engine, not a simulation platform. Agent orchestration belongs in the consumer.
2. **Shared vector store**: Multiple agents write to the same embedding store. Rejected — loses all graph structure, typed relationships, and reasoning preservation that Anamnesis provides.
3. **Separate graphs per agent with merge**: Each agent maintains its own graph; periodic merge combines them. Rejected — complex merge semantics, loses real-time cross-agent activation, and contradicts the single-graph architecture.

## Implementation Summary

| Feature | Status | Notes |
|---------|--------|-------|
| Origin struct (agent_id, session_id, scope, confidence) | ✅ | Every Node carries provenance. |
| Hierarchical scoping via Origin.scope | ✅ | Enables multi-domain, multi-project graphs with scope-aware queries. |
| Social reinforcement scoring | ✅ | Multi-agent corroboration scoring and consumer feedback salience updates are implemented. |
| Batch reflect API | ✅ | Creates `Entity` edges for cross-agent nodes with shared entity tags. |
| Entity edge type | ✅ | Defined in `src/graph/types.rs` with kappa=0.95. |

## Consequences

- Node struct carries an `Origin` field (required, not optional — all nodes have provenance).
- `EdgeType` enum includes `Entity` variant for cross-agent links.
- Social reinforcement is exposed as scoring primitives and feedback-driven salience updates.
- `Engine` has `reflect_batch()` for metadata-only cross-agent entity linking.
- All changes are additive — no existing API breaks.
- The consumer is responsible for populating `Origin` when ingesting and calling `reflect_batch()` at round boundaries.

## References

- MiroFish: Multi-agent social simulation (GitHub: 666ghj/MiroFish)
- ADR-001: Cognitive Graph as Application-Layer Engine
- Vision document, Sections 7–9: Origin Attribution, Social Reinforcement, Batch Reflect
