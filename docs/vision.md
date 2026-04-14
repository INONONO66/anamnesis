# Vision — Why Anamnesis Exists

> Plato's anamnesis: the soul already possesses knowledge; learning is recollection triggered by the right cue.

---

## The Problem No One Has Solved

Every agent session starts from zero. Agents repeat mistakes, rediscover conventions, and lose the reasoning behind past decisions. The industry has converged on two inadequate solutions:

**Memory Layers** (Mem0, Letta/MemGPT) store facts or conversations and retrieve by similarity. They answer "what was said" but not "why it was decided" or "how things connect." Noise grows linearly with data.

**Context Evolution** (Stanford ACE, OmO Wisdom) evolve a monolithic playbook from execution feedback. They improve over time but lose granularity — brevity bias erodes detail, and there's no way to access the original fragment that led to a conclusion.

Neither provides what a long-running coding agent actually needs: **fragment-level knowledge with associative retrieval, natural decay, and reasoning preservation.**

---

## Core Philosophy

### 1. Memory Is Cognition, Not Storage

The dominant framing treats agent memory as a database problem. This misses the point.

Human memory is an associative network. One cue activates related memories, which activate further memories, reconstructing understanding from fragments. The value is not in what is stored but in **how fragments connect and activate each other**.

Anamnesis models this directly: knowledge exists as a graph of fragments connected by typed relationships. Retrieval is graph traversal (spreading activation), not keyword search.

### 2. Fragments, Not Summaries

Existing systems summarize conversations into compact facts. This is lossy — the reasoning, context, and alternatives considered are discarded.

Anamnesis preserves **individual conversation turns as nodes**. Each retains the original information, temporal position, entity references, and origin metadata. Summaries are emergent — they arise when the Curator consolidates repeated patterns into higher-level semantic nodes. The raw fragments remain accessible.

**Clarification on tiered resolution:** A future planned feature (tiered content with L0 abstracts) may attach short summaries to nodes for efficient scanning. These are **non-destructive previews** — indexes into the original content, not replacements. The full fragment is always preserved. L0 abstracts serve the same role as a book's table of contents: they help locate relevant knowledge without reading everything, but the original text remains the source of truth.

### 3. Thinking Expands From Cues

When an agent encounters "auth module," Anamnesis activates the auth node and traverses outward:

```
"auth module" (cue)
  -> "auth uses factory pattern" (attraction: 0.92)
     -> "factory pattern defined in conventions" (gravity pull)
  -> "race condition in auth middleware" (attraction: 0.78)
     -> "session module has similar issue" (cross-domain link)
  -> "auth refactored to DI" (supersedes factory node)
```

Each hop weakens the activation signal. Traversal stops when the token budget is exhausted or activation drops below threshold. This is spreading activation — implemented as standalone functions in `src/query/spread.rs`, pending integration into the Engine's `query()` method.

### 4. Forgetting Is a Feature

Forgetting is a first-class concept in Anamnesis. The mechanics exist in `src/mechanics/forgetting.rs` as pure scoring functions; integration into the Engine's `tick()` loop is planned. Natural decay makes irrelevant nodes invisible without human intervention. Knowledge that matters gets reinforced through access; knowledge that doesn't fades naturally. No manual cleanup.

### 5. Forgotten Is Not Gone — Reactivation

A node at salience 0.03 is invisible to general queries but still exists in the graph. When someone precisely mentions the topic, the node reactivates:

```
March:     Node created, salience 0.7
June:      No access, decay -> salience 0.08 (below threshold, invisible)
September: Direct mention -> touch() -> salience spikes back
           -> Connected nodes reactivate via spreading activation
```

The access path weakened, not the memory itself. This maps directly to `Engine::touch()` — reinforcement on access.

### 6. Knowledge Transfer, Not Session Memory

The ultimate purpose is not "remember last conversation." It is:

**Transfer a human's cognitive framework to agents.**

This means recording not just what was decided, but _why_:

```
Node: "Use namespace pattern over classes"
  REASON edge -> "tree-shaking, explicit deps, no DI needed"
  REJECTED_ALTERNATIVE edge -> "class instances — state management too complex"
  REINFORCED_BY edges -> [Session #15, #23, #41]
```

When a new agent session starts, it inherits not rules but _judgment_.

### 7. Universal Knowledge Memory, Not Just Conversation

Anamnesis is not a conversation logger. It is a **universal knowledge substrate** — any structured knowledge the consumer extracts can live in the same graph with the same physics.

Knowledge types that belong in the graph:

| Type | Example | How It Enters |
|:--|:--|:--|
| **Episodic** | Raw conversation turn | Consumer feeds session text directly |
| **Semantic** | Extracted fact ("auth uses factory pattern") | Consumer extracts via LLM, links to source episode |
| **Procedural** | "How to deploy this service" | Consumer extracts from agent execution logs |
| **Entity** | "auth module", "session service" | Consumer identifies entities, Anamnesis auto-links by tag |
| **Identity** | Agent persona traits, values, preferences | Consumer or user defines; engine applies physics |

All types are graph nodes. All receive the same mechanics: attraction clusters related nodes, gravity surfaces important ones, forgetting decays unused ones, touch reinforces accessed ones.

**The consumer (e.g., an orchestration layer) is responsible for extraction.** Anamnesis does not call LLMs. But Anamnesis must provide a graph structure rich enough to represent all these knowledge types naturally.

### 8. Episodic Preservation — Original Text as Source of Truth

When the consumer extracts facts from a conversation, the original conversation turn must also be stored as an episodic node. Extracted knowledge links back via `EXTRACTED_FROM` edges:

```
Episode (original turn)
  ├── EXTRACTED_FROM ← Fact: "auth uses factory pattern"
  ├── EXTRACTED_FROM ← Entity: "auth module"
  └── EXTRACTED_FROM ← Reason: "tree-shaking + no DI"
```

This enables:
- **Provenance tracing**: Any fact can be traced to the exact conversation that produced it
- **Hallucination verification**: "Did I really say that?" → follow EXTRACTED_FROM → read original
- **Context reconstruction**: When a fact is retrieved, its source episode provides full context

Without episodic preservation, extracted facts become orphans — no way to verify or contextualize them.

### 9. Identity as Graph Nodes — The Multi-Persona Brain

Agent identities are not static system prompts. They are **graph nodes subject to the same physics as knowledge**.

Inspired by MetaGPT Stanford Town's three-layer identity and agentic-cognition's ConvictionGravity:

**Identity Hierarchy (L0/L1/L2):**

| Layer | Name | Decay | Example |
|:--|:--|:--|:--|
| L0 | IdentityCore | None (salience fixed at 1.0) | "I am a code architect", "Accuracy is paramount" |
| L1 | IdentityLearned | Very slow | "This project prefers factory pattern" |
| L2 | IdentityState | Normal | "Currently refactoring auth module" |

L0 nodes are immutable anchors. L1 nodes evolve slowly through experience (reinforced by `touch()`, decayed by `tick()`). L2 nodes change freely with context.

**Multi-Agent Identity:**

When multiple agents share a graph, each agent's identity nodes carry `Origin` metadata. The graph becomes a **multi-persona brain** — like dissociative identity, each persona has its own knowledge and traits, but they share a common substrate:

- **Spreading activation respects persona scope**: Query with `Origin.agent_id` filter to get one agent's perspective
- **Social reinforcement across personas**: When multiple agents independently confirm the same knowledge, it gains extra salience
- **Contradiction detection**: `Contradicts` edges between nodes from different agents surface disagreements

This is not agent orchestration (that's the consumer's job). This is providing a **cognitive substrate where identities and knowledge coexist and interact through physics**.

### 10. Repulsion — Not All Connections Are Attraction

Current mechanics model only attraction (similar things cluster). Real cognition also has **repulsion** — contradictory ideas push each other apart.

New edge types for repulsion:

| Edge Type | Meaning | Effect on Spreading Activation |
|:--|:--|:--|
| `Contradicts` | Two nodes assert opposite things | Activation is **dampened** when crossing |
| `Supersedes` | One node replaces another | Old node's salience decays faster |
| `RejectedAlternative` | Option considered and discarded | Activation flows but is marked as "rejected" |

When spreading activation encounters a `Contradicts` edge, the target node receives **negative** or **reduced** activation. This naturally surfaces conflicts: "You said X here, but Y there."

Combined with Origin metadata, this enables **inter-agent conflict detection**: two agents' contradictory observations are linked, flagged, and surfaced for resolution (by the consumer's LLM, not the engine).

---

## How This Maps to Anamnesis Architecture

The existing codebase contains the building blocks as standalone modules. They are not yet wired into the Engine. The vision clarifies _what these mechanics are for_:

| Mechanic                 | Implementation                | Purpose in Vision                                                  | Integration |
| ------------------------ | ----------------------------- | ------------------------------------------------------------------ | ----------- |
| **Attraction**           | `src/mechanics/attraction.rs` | Similar/related fragments cluster — enables associative retrieval  | Standalone |
| **Gravity**              | `src/mechanics/gravity.rs`    | Important nodes attract new knowledge — hub nodes emerge naturally | Standalone |
| **Perception**           | `src/mechanics/perception.rs` | Input gating — not every conversation turn becomes a node          | Standalone |
| **Forgetting**           | `src/mechanics/forgetting.rs` | Natural decay + reinforcement on access = self-maintaining graph   | Standalone |
| **Spreading Activation** | `src/query/spread.rs`         | Cue-based retrieval — "think outward from a fragment"              | Standalone |

### What Needs to Be Added

**Edge types for reasoning preservation:**

- `REASON` — why a decision was made
- `REJECTED_ALTERNATIVE` — option considered and discarded
- `SUPERSEDES` — replaces outdated knowledge
- `REINFORCED_BY` — confirmed by repeated experience
- `CONSOLIDATED_FROM` — semantic node derived from multiple episodic nodes

**Three-role processing (adapted from Stanford ACE):**

| Role      | When                  | What                                                                  |
| --------- | --------------------- | --------------------------------------------------------------------- |
| Generator | During ingestion      | Extracts nodes from conversation turns, creates temporal/entity edges |
| Reflector | On session completion | Reviews nodes, assigns importance, creates cross-session edges        |
| Curator   | Periodic batch        | Applies decay, detects contradictions, consolidates patterns          |

Note: These roles are **consumer-side** (the orchestration layer manages them). Anamnesis engine provides the primitives (`ingest`, `link`, `tick`, `query`, `touch`, `auto_merge`). The engine does not call LLMs.

### Consumer vs. Engine Boundary (What Needs LLM)

| Operation | Who | Why |
|:--|:--|:--|
| Entity extraction ("auth module" from text) | **Consumer** (LLM) | Requires language understanding |
| Relationship judgment ("A depends on B") | **Consumer** (LLM) | Requires semantic reasoning |
| Embedding generation | **Consumer** (LLM/model) | Requires ML model |
| Similarity-based auto-linking | **Engine** (math) | Cosine similarity on provided embeddings |
| Entity tag matching | **Engine** (rules) | Same tag → auto-link |
| Temporal adjacency linking | **Engine** (rules) | Same session, consecutive → auto-link |
| Cluster detection | **Engine** (graph algorithm) | Pure structure, no content understanding |
| Bridge node detection | **Engine** (graph algorithm) | Structural analysis |
| Contradiction flagging | **Engine** (edge type) | Consumer creates Contradicts edges; engine surfaces them |
| Conflict resolution ("which is right?") | **Consumer** (LLM) | Requires judgment |

**Selective injection (token-budget aware):**

- Query graph with scope (entities relevant to next task)
- Rank by `salience * activation_strength`
- Return top nodes within token budget
- This is the intended shape of `Engine::query(seed, budget)` — budget-constrained subgraph extraction (not yet wired)

**Memory tiers:**

```
Tier 0: Constitution (external, not in Anamnesis)
  AGENTS.md, golden principles. Human-only modification.

Tier 1: Core Memory (high-salience nodes, always surfaced)
  Project conventions, active decisions.
  Maintained by gravity — high-centrality nodes stay salient.

Tier 2: Working Knowledge (session-scoped)
  Current task learnings.
  Promoted to Tier 3 by Reflector.

Tier 3: Accumulated Wisdom (cross-session)
  Episodic: "Race condition appeared when modifying auth"
  Semantic: "This codebase uses factory pattern for services"
  Procedural: "Tests follow given/when/then structure"

Tier 4: Archive (low-salience, search-only)
  Decayed nodes. Still in graph, below threshold.
  Reactivated on precise mention.
```

Tiers are not separate stores — they are **salience ranges** within the same graph. Gravity and forgetting naturally distribute nodes across tiers.

---

## Differentiation

| System           | Storage Unit               | Retrieval           | Decay                       | Knowledge Transfer              |
| ---------------- | -------------------------- | ------------------- | --------------------------- | ------------------------------- |
| **Mem0**         | Extracted facts            | Vector similarity   | None                        | Facts only                      |
| **Letta/MemGPT** | Conversation history       | Text search         | None                        | Session recall                  |
| **OmO Wisdom**   | Category text blobs        | Full load           | None                        | Pattern notes                   |
| **Stanford ACE** | Monolithic playbook        | Full load           | Curator rewrites            | Strategy evolution              |
| **Anamnesis**    | **Conversation fragments** | **Graph traversal** | **Natural decay + revival** | **Reasoning chains + judgment** |

> **Relationship to GraphRAG:** Anamnesis and GraphRAG solve different problems. GraphRAG optimizes document corpus QA via community detection and LLM summarization. Anamnesis models agent memory with temporal dynamics. Anamnesis's spreading activation covers GraphRAG's local search; planned cluster detection and structural queries will cover the "global view" need — without LLM dependency. See [ADR-004](./design-decisions/004-universal-knowledge-memory.md) for details.

What Anamnesis does that others cannot:

1. **Cross-session associative retrieval** — one cue activates related knowledge across sessions and domains, because the graph connects them through shared entities
2. **Natural pruning without data loss** — salience decay makes irrelevant nodes invisible; precise mention revives them. No manual cleanup, no permanent deletion
3. **Reasoning preservation** — not just "use Zod" but why, what was rejected, and how the decision was validated
4. **Compounding accuracy** — more data makes it more precise (decay filters noise), unlike vector stores where more data increases noise

### 11. Origin Attribution — Who Knows What

When multiple agents share the same knowledge graph, a fragment's meaning changes depending on who produced it. "Use factory pattern" from an architect agent carries different weight than the same fragment from a junior code assistant.

Anamnesis addresses this by attaching **origin metadata** to every node:

```
Origin {
  agent_id:    &str,    // which agent produced this knowledge
  session_id:  &str,    // from which session
  confidence:  f64,     // how certain the agent was at creation time
}
```

Origin is not an access-control mechanism — it's an epistemic marker. The Reflector uses it to:

- **Resolve contradictions**: If two nodes disagree, origin reveals whether they come from the same agent (correction) or different agents (genuine disagreement).
- **Weight expertise**: A graph traversal can factor in which agent produced a node, allowing the consumer to bias toward domain-specific expertise.
- **Trace provenance**: When a decision chain is reconstructed, origin shows exactly which agents contributed which fragments.

Origin is the foundation for both Social Reinforcement and Batch Reflect — without it, multi-agent signals are indistinguishable from single-agent repetition.

### 12. Social Reinforcement — Independent Corroboration

In human communities, knowledge gains credibility when multiple independent sources confirm it. The same principle applies to multi-agent systems.

When the same concept is independently reinforced by multiple distinct agents, it deserves a salience bonus beyond what single-agent reinforcement provides. This is **social reinforcement**:

```
social_bonus(node) =
  distinct_agents = count unique agent_ids on REINFORCED_BY edges of node
  if distinct_agents > 1:
    bonus = 1.0 + ln(distinct_agents)
  else:
    bonus = 1.0  // no multi-agent bonus
```

Key properties:

- **Logarithmic scaling**: Two independent agents confirming is significant; ten agents is more so, but with diminishing returns. This prevents popularity cascades.
- **Independence requirement**: Multiple reinforcements from the *same* agent in different sessions do not trigger social bonus — only distinct `agent_id` values count.
- **Composable with existing mechanics**: Social bonus multiplies with the existing salience reinforcement in `forgetting.rs`. It does not replace any current mechanic.

This leverages the Origin struct (Section 11) to distinguish agents. Without origin attribution, there is no way to tell whether five reinforcements came from five agents or one agent in five sessions.

### 13. Batch Reflect — Cross-Agent Entity Linking

After a round of parallel agent execution, each agent has ingested its own fragments. These fragments may reference overlapping entities (the same function, module, or concept) without knowing about each other.

**Batch Reflect** is a round-boundary operation that creates cross-agent links:

```
Engine::reflect_batch(sessions: &[SessionSummary]) -> ReflectReport

SessionSummary {
  agent_id:   &str,
  session_id: &str,
  node_ids:   Vec<NodeId>,
}

ReflectReport {
  entity_edges_created: usize,
  clusters_found:       usize,
}
```

The algorithm:

1. Collect all nodes from the given sessions.
2. Group by shared entities (extracted from node metadata — no LLM calls).
3. For each entity group spanning 2+ agents, create `Entity` edges between the relevant nodes.
4. Return a report of what was linked.

Batch Reflect does **not** merge nodes or alter salience. It only creates edges that make cross-agent knowledge discoverable via spreading activation. The Reflector or Curator can later evaluate these edges for contradiction or consolidation.

This requires Origin (Section 11) to identify which nodes belong to which agents, and creates the graph topology that Social Reinforcement (Section 12) operates on.
---

## Dependency Chain (Multi-Agent Features)

```
Origin Attribution (Section 11)
  ├── Social Reinforcement (Section 12)
  │     Uses origin to count distinct agents
  └── Batch Reflect (Section 13)
        Uses origin to identify cross-agent nodes
```

All three features are **design-level** — they describe future capabilities to be implemented. The existing engine already has the primitives (`link()`, `touch()`, metadata on nodes) that these features will build on.

## References

- Stanford ACE: "Agentic Context Engineering" (ICLR 2026, arXiv:2510.04618)
- Anthropic: "Effective Context Engineering for AI Agents" (Sep 2025)
- OpenAI: "Harness Engineering" (Feb 2026)
- Letta: "Letta Code: A Memory-First Coding Agent" (Dec 2025)
- Mem0: Graph Memory for AI Agents (Jan 2026)
- Animesis: "Memory as Ontology" (Mar 2026, arXiv:2603.04740)
- MiroFish: Multi-agent social simulation with shared memory (GitHub: 666ghj/MiroFish) — inspiration for Origin Attribution, Social Reinforcement, and Batch Reflect patterns
- Collins & Loftus: "A Spreading-Activation Theory of Semantic Processing" (1975)
- Tulving: "Episodic and Semantic Memory" (1972)
- oh-my-openagent: Wisdom Accumulation pattern (Atlas notepad system)
