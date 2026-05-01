# ADR-009: Identity-Conditioned Recall

**Status**: Accepted

## Context

Agent memory needs stable operating principles, learned preferences, and current working state. These identity cues should influence recall, but they should not become a hidden behavior prompt, access-control mechanism, or truth filter.

## Decision

Represent identity as graph-resident memory nodes:

| Type | Role | Dynamics |
|:-----|:-----|:---------|
| `IdentityCore` | Stable retrieval anchors and operating principles | No decay; high mass |
| `IdentityLearned` | Learned preferences and conventions | Very slow decay |
| `IdentityState` | Current task or stance | Normal decay |

Identity nodes condition recall. They bias activation, ranking, packaging, and tension detection. They do not define the agent’s runtime instructions.

Canonical rule:

> Identity conditions recall; it does not define the agent’s runtime instructions.

## Retrieval Behavior

When a query includes an active `agent_id`, the engine may collect that agent’s identity nodes and use them as retrieval priors. Identity-aligned memories can receive stronger activation, and identity-contradicting memories should surface as tensions rather than being hidden.

The `ContextPackage.identity` partition contains retrieved identity cues. The consumer decides whether and how to expose those cues to an LLM.

## What the Engine Does Not Do

- It does not generate a system prompt.
- It does not enforce behavior.
- It does not hide contradictory facts.
- It does not use identity as access control.

## Consequences

- Naming should use `identity` in APIs and docs.
- Contradictions involving identity nodes should be visible through `tensions` and `agent_tension`.
- Search traces should explain identity contributions when identity bias affects ranking.

## Related Decisions

- [ADR-003: Multi-Agent Memory Support](./003-multi-agent-memory.md)
- [ADR-004: Universal Knowledge Memory with Identity](./004-universal-knowledge-memory.md)
- [ADR-007: Trigger Indexes vs. Graph Memory](./007-trigger-indexes-vs-graph-memory.md)
