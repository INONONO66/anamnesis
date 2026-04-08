# ADR-004: Universal Knowledge Memory with Identity

**Status**: Proposed

## Context

Anamnesis was originally designed around conversation fragments — individual conversation turns stored as graph nodes. In practice, LLM agents receive knowledge from many sources:

- Conversation turns (episodic)
- Extracted facts and relationships (semantic)
- Agent execution patterns (procedural)
- Named entities (modules, people, services)
- Time-bound events
- Agent persona traits and values (identity)

Analysis of production memory systems (Mem0, Zep/Graphiti, Cognee, CrewAI) revealed that all handle multiple knowledge types — not just conversations. Agents also need multiple query patterns beyond spreading activation: type-filtered, entity-centric, temporal, and listing queries.

Separately, analysis of agent persona systems (MetaGPT Stanford Town, agentic-cognition) revealed that no existing system places agent identity inside the knowledge graph. Identity is always a separate text string. Treating identity as graph nodes subject to the same physics (attraction, gravity, decay) is a novel approach.

## Decision

Expand Anamnesis from "conversation fragment memory" to "universal knowledge memory with identity":

### 1. Knowledge Type Taxonomy

Add a `KnowledgeType` enum to distinguish node types. Each type can have different decay rates, query behavior, and mechanics:

- `Episodic` — raw conversation/session text (source of truth)
- `Semantic` — extracted facts
- `Procedural` — agent execution patterns
- `Entity` — named concepts
- `Event` — time-bound occurrences
- `IdentityCore` (L0) — immutable agent traits (no decay)
- `IdentityLearned` (L1) — experience-formed traits (slow decay)
- `IdentityState` (L2) — current state (normal decay)

### 2. Episodic Preservation

Original session text is stored as `Episodic` nodes. Extracted knowledge links back via `ExtractedFrom` edges. This enables provenance tracing and hallucination verification.

### 3. Identity as Graph Nodes

Agent identities are graph nodes, not external strings. L0 nodes (IdentityCore) have fixed salience and zero decay. L1 nodes evolve slowly. L2 nodes change freely. Multi-agent identity works through Origin metadata — each agent's identity nodes are scoped but share the graph substrate.

### 4. Repulsion Mechanics

Add `Contradicts` edge type. When spreading activation crosses a Contradicts edge, activation is dampened or negated. This surfaces conflicts between agents or between old and new knowledge.

### 5. Multiple Query Modes

Add Associative (existing), TypeFiltered, Neighborhood, Temporal, and List query modes to cover the five real-world agent query patterns.

### 6. Pure Graph Algorithms

Add engine-level algorithms that require no LLM: Union-Find for connected components, bridge detection for decay protection, Label Propagation for clustering, metadata-based entity matching.

## Rationale

- **Universal knowledge types are needed**: Production systems (Mem0, Zep, Cognee) all handle multiple types. Conversation-only is insufficient.
- **Episodic preservation enables verification**: Without source episodes, extracted facts are unverifiable orphans.
- **Identity-in-graph is novel and consistent**: If knowledge nodes have physics, identity nodes should too. No existing system does this.
- **Repulsion completes the physics model**: Attraction without repulsion cannot model contradiction or conflict.
- **Multiple query modes match real usage**: Research shows spreading activation covers ~40% of agent queries. The remaining 60% need structured access.
- **All changes preserve core principles**: No LLM in core. Zero external deps. Synchronous API. Consumer handles extraction.

## Alternatives Considered

1. **Keep conversation-only scope**: Rejected — insufficient for real agent needs.
2. **Add LLM-based linking in engine**: Rejected — violates "No LLM in core" principle. Consumer handles extraction.
3. **Separate identity system**: Rejected — defeats the purpose of unified physics. Identity should be subject to the same mechanics.
4. **Import GraphRAG wholesale**: Rejected — GraphRAG is optimized for document corpus QA with batch indexing and LLM summarization. Anamnesis needs streaming updates and LLM independence. Spreading activation is a better fit for agent memory than community-based map-reduce.

## Consequences

- `KnowledgeType` enum replaces `node_type: String` on Node
- `ExtractedFrom` and `Contradicts` added to EdgeType enum
- `Observation` struct gains `source_ref`, `entities`, `valid_at` fields
- `Edge` struct gains `fact`, `valid_at`, `invalid_at`, `source_episodes` fields
- `Query` becomes an enum with five variants
- L0 identity nodes are exempt from decay in `tick()`
- `Contradicts` edges produce negative/dampened activation in spreading activation
- Consumer API contract expands: consumers should provide `KnowledgeType` and optionally `source_ref` when ingesting
- All changes are additive — existing API methods continue to work
