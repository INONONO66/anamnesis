# Graph Data Model

The memory graph stores typed sites and typed edges. It is both a persistence model and the substrate over which activation flows.

## Nodes

A node is a memory site. It stores identity, source content, projections, provenance, and temporal validity.

| Field | Meaning |
|---|---|
| `id` | Stable `NodeId` allocated by storage |
| `name` | L0 short name used for compact display |
| `summary` | Optional L1 summary |
| `content` | L2 full preserved content |
| `node_type` | Knowledge taxonomy value |
| `retained_action` | Persistent memory strength `A_i = B_i + P_i`; the base-level term `B_i` is computed on demand from `access_history` (not stored), the evidence prior `P_i` is stored |
| `access_history` | Bounded 32-trace window (creation trace plus committed accesses); each trace is a pair `(timestamp, per-trace decay rate d_j)`, with `d_j` computed at creation from current activation and immutable thereafter; the persistent substrate from which `B_i` is computed |
| `evidence_prior` | Stored evidence prior `P_i` (encoding surprise, feedback / social reinforcement, peer trust); a decay-exempt evidence offset |
| `salience` | Bounded logistic projection of the sum, `s_i = logistic(B_i + P_i)` |
| `embedding` | Optional semantic vector |
| `entity_tags` | Normalized entity labels |
| `origin` | Peer, session, scope, source kind, confidence |
| `created_at` / `accessed_at` | Record time and last access time |
| `valid_from` / `valid_until` | Fact-time validity interval |
| `tier` | Optional explicit tier policy |

## Origin

Every site carries origin. Provenance is not optional.

| Field | Meaning |
|---|---|
| `peer_id` | Human, agent, tool, or system that produced the fragment |
| `source_kind` | Observation source category |
| `session_id` | Session where the fragment was produced |
| `scope` | Visibility and applicability path |
| `confidence` | Source-level confidence in `[0, 1]` |

## Node Types

`KnowledgeType` collapsed from 15 variants to 4 in the [v0.10.0 shrink](../adr/0014-shrink-to-product.md); the v6→v7 migration normalizes legacy rows into these.

| Variant | Dynamics Role |
|---|---|
| `Episodic` | Raw or time-bound fragment — a specific event or conversation turn |
| `Semantic` | Reusable fact or generalization; target of consolidation |
| `Identity` | Retrieval prior; routed to a dedicated context partition |
| `Custom(String)` | Consumer-defined type (renders by its bare label) |

Type affects decay prior, packaging bucket, readout treatment, and conductance priors. The decay prior is the `node_type` policy multiplier `m_type` applied during per-trace `d_j` computation: it is the outer multiplier in `d_j = m_type · ( c · e^{m_j} + α )`, not an independent rate. A type with `m_type = 0` (`Identity`) yields `d_j = 0` for every trace and never decays. It must not be replaced by free-form strings at the engine boundary except through `Custom`. (The pre-0.10.0 finer taxonomy — the `IdentityCore`/`IdentityLearned`/`IdentityState` split, `Procedural`/`Convention`/`Decision`/`Gotcha`/`Entity`/`Event`, and the inert `DebugSession`/`Hypothesis`/`Evidence` debug family — is historical; ADR-0014 records the by-design decay coarsenings applied to their former `m_type` values.)

## Edges

Edges connect sites and carry typed relationships.

| Field | Meaning |
|---|---|
| `id` | Stable `EdgeId` |
| `source` / `target` | Directed endpoints |
| `edge_type` | Relationship taxonomy value |
| `conductance` | Authoritative associative-strength reservoir |
| `weight` | Bounded projection of conductance |
| `created_at` / `accessed_at` | Record and access time |
| `valid_from` / `valid_until` | Validity interval for fact-time queries |
| `metadata` | Explanation and provenance for the link |

## Edge Types

| Type | Retrieval Meaning |
|---|---|
| `Semantic` | Conceptual association |
| `Causal` | Cause-effect relation |
| `Temporal` | Sequence relation |
| `Reason` | Rationale for a decision or claim |
| `ReinforcedBy` | Repeated confirmation |
| `ConsolidatedFrom` | Synthesis derived from sources |
| `ExtractedFrom` | Extracted knowledge linked to source episode |
| `Entity` | Shared entity link, including cross-agent reflection |
| `Supersedes` | Replaces older knowledge |
| `RejectedAlternative` | Considered and discarded option |
| `Supports` | Positive evidential support for a hypothesis |
| `Refutes` | Refuting evidence for a hypothesis; a positive conductive path that surfaces counter-evidence (inhibition is modeled by `Contradicts`/frustration, never negative conductance). Surfacing refuting evidence raises the hypothesis's *retrievability*, not its credence — recall co-activates a claim with its rebuttal; acceptance is decided only by the debug lifecycle (`confirm`/`reject`), a channel separate from activation |
| `BelongsTo` | Debug-session membership |
| `Contradicts` | Constraint edge excluded from propagation and surfaced as frustration |
| `Custom(String)` | Consumer-defined relationship |

`Contradicts` is not a negative conductance path. It is a constraint used by the frustration channel.

## State Classes

| State | Persistent? | Query-local? |
|---|---:|---:|
| `access_history` (drives `B_i`) | yes | read |
| `evidence_prior P_i` | yes | read |
| `base-level B_i` | computed (not stored) | read |
| `conductance` | yes | read |
| `salience` = `logistic(B_i + P_i)` | projected | read |
| `weight` | projected | read |
| `activation a_i` | no | yes |
| `current I_ij` | no | yes |
| `impedance Z_i` | no | yes |
| `stress sigma_ij` | no | yes |
| `trace` | returned / optionally committed | yes |

## Multi-Resolution Content

The engine preserves multiple resolutions instead of forcing one summary:

| Level | Field | Use |
|---|---|---|
| L0 | `name` | Compact identity in traces and lists |
| L1 | `summary` | Mid-size packaging |
| L2 | `content` | Full source text |

Packaging may downgrade resolution to fit budget, but the original content remains.

## Query Types

| Query | Purpose |
|---|---|
| `Associative` | Run activation flow from a seed |
| `TypeFiltered` | Retrieve sites by knowledge type |
| `Neighborhood` | Return k-hop structural neighborhood |
| `Temporal` | Retrieve recent or valid-time-filtered sites |
| `List` | List sites above a salience threshold |

`search` is a broader entry point that gathers candidate seeds and then invokes graph recall.

## SessionSummary

> **Removed in [v0.10.0](../adr/0014-shrink-to-product.md).** `SessionSummary` and `reflect_batch` (metadata-only cross-agent reflection that linked sites sharing entity tags without calling an LLM) had no consumer and were removed with the peer/trust subsystem. Cross-agent linking is [roadmap](peer-identity.md), gated on a real multi-peer deployment.

## Graph Invariants

- Every site has an origin.
- Query-local activation, current, impedance, and stress are never persisted as authoritative state.
- Public projections stay bounded.
- Scope and valid-time filters are applied before packaging.
- Contradiction edges preserve both sides and do not auto-delete.
- Synthesis nodes link to sources instead of overwriting them.
