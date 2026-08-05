# 0014. Product Boundary And Provenance-Only Peer Identity

- Status: Accepted
- Date: 2026-07-03
- Version: v0.10.0
- Related: [ADR-0002](0002-reservoir-projection-state.md), [ADR-0008](0008-powerlaw-dissipation.md), [ADR-0012](0012-daemon-core-mcp-plugin-clients.md), [ADR-0013](0013-reasoning-capture-pipeline.md), [ADR-0015](0015-evidence-grounded-formation-and-chain-retrieval.md)

## Context

The public API previously included subsystems without a shipped `Memory`, MCP,
plugin, or operational consumer. Several also implied semantics the product did
not implement end to end, including peer authentication, peer reputation,
cross-agent corroboration, and hierarchical scope policy.

An unowned public contract increases migration and maintenance cost without a
way to validate its behavior. Provenance fields, by contrast, are required by
active retrieval and audit paths even when no producer-reputation system
exists.

## Decision

Keep the public surface aligned with active product contracts. Remove unused
subsystems and treat `PeerId` and `SourceKind` strictly as source provenance.

### Removed surfaces

| Area | Removed contract |
|---|---|
| Debug lifecycle | Debug-session, hypothesis, and evidence workflow methods and node types |
| Convenience methods | Unused wrappers for learning, activity, scheduling, perspective queries, batch reflection, support reports, and manual consolidation |
| Peer policy | Peer registry, aliases, profiles, trust levels, trust reservoirs, trust-weighted readout, and cross-agent reinforcement |
| Type taxonomy | Unused specialized knowledge variants; the supported set is `Episodic`, `Semantic`, `Identity`, and `Custom(String)` |
| Scope hierarchy | Inferred ancestor, descendant, sibling, and disjoint relations from scope text |
| Tier override | Manual memory-tier setters and getters |

### Current peer and feedback contract

- `Origin.peer_id` is an opaque producer identifier supplied by the consumer.
- `Origin.source_kind` classifies the producer channel.
- Neither field authenticates a caller, grants visibility, or contributes a
  producer-reputation score.
- Agreement among distinct producer ids does not promote a claim or strengthen
  a memory automatically.
- Explicit feedback and committed use update the returned memory evidence under
  the interaction model. They do not update a peer profile.
- Scope, temporal validity, source grounding, and contradiction remain
  independent eligibility constraints.

### Retained contracts

- `PeerId`, `SourceKind`, session id, scope, and confidence remain on `Origin`
  so every node has auditable provenance.
- `KnowledgeType::Identity` remains a supported graph/query partition.
- `MemoryTier` remains a derived display label; it is not manually assigned.
- `ScopePath` remains an opaque canonical value with explicit universal/exact
  compatibility. Consumers resolve authorization or organizational hierarchy
  before calling the engine.
- Internal modules remain the implementation behind the documented `Memory`,
  `Engine`, and `Error` surfaces.

## Behavioral and migration impact

Collapsing legacy knowledge variants changes their type-specific decay-policy
input while leaving the base-level/evidence-prior dynamics in
[ADR-0008](0008-powerlaw-dissipation.md) unchanged.

| Legacy class | Normalized behavior |
|---|---|
| Event, convention, and decision variants | Ordinary `Semantic` or `Custom` knowledge policy |
| Debug, hypothesis, and evidence variants | `Custom` knowledge policy |
| Identity subtypes | Unified protected `Identity` policy |
| Entity-to-entity special case | Ordinary seed distribution |

The shipped decay multipliers after normalization are `Identity = 0.0`,
`Semantic` / `Custom = 0.40`, and `Episodic = 1.0`, applied over the calibrated
intercept described by the mechanics documentation.

Both storage migrations run automatically on `SqliteStorage::open`:

- v5 to v6 removes peer-registry tables while retaining node origins.
- v6 to v7 rewrites legacy node-type strings to `Semantic`, `Identity`, or
  `Custom(<original>)` so existing content remains readable.

Migration guarantees are defined in the
[migration policy](../03-persistence/migration-policy.md).

## Consequences

- Stored provenance remains available without implying trust, identity, or
  consensus semantics.
- Downstream code using the removed v0.9 API requires migration to the retained
  surfaces.
- Any identity, authorization, reputation, or collaboration subsystem requires
  an explicit consumer boundary, threat model, calibration method, migrations,
  and end-to-end quality gates.
- Additive evidence formation and chain retrieval are specified separately in
  [ADR-0015](0015-evidence-grounded-formation-and-chain-retrieval.md); they do
  not restore peer trust or cross-agent reinforcement.
