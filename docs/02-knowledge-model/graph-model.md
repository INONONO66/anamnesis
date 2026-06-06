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
| `retained_action` | Authoritative memory-strength reservoir |
| `salience` | Bounded projection of retained action |
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

| Class | Types | Dynamics Role |
|---|---|---|
| Identity | `IdentityCore`, `IdentityLearned`, `IdentityState` | Retrieval priors and agent-state context |
| Knowledge | `Semantic`, `Procedural`, `Entity`, `Convention`, `Decision`, `Gotcha` | Reusable facts and operating knowledge |
| Debug | `DebugSession`, `Hypothesis`, `Evidence` | Inert debug lifecycle records |
| Memory | `Episodic`, `Event` | Raw or time-bound fragments |
| Custom | `Custom(String)` | Consumer-defined type |

Type affects decay prior, packaging bucket, readout treatment, and conductance priors. It must not be replaced by free-form strings at the engine boundary except through `Custom`.

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
| `retained_action` | yes | read |
| `conductance` | yes | read |
| `salience` | projected | read |
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

`SessionSummary` is the metadata-only input for cross-agent reflection. It carries agent id, session id, and node ids. `reflect_batch` links sites sharing entity tags across agents without calling an LLM.

## Graph Invariants

- Every site has an origin.
- Query-local activation, current, impedance, and stress are never persisted as authoritative state.
- Public projections stay bounded.
- Scope and valid-time filters are applied before packaging.
- Contradiction edges preserve both sides and do not auto-delete.
- Synthesis nodes link to sources instead of overwriting them.
