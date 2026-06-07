# Vision

LLM agents lose continuity between sessions. They repeat mistakes, rediscover project conventions, and forget why earlier decisions were made. A durable memory system must preserve more than a flat transcript or a vector index; it must preserve the relationships that let one cue reconstruct the surrounding context.

Anamnesis models memory as a conductive graph of fragments. Each fragment is a memory site. Typed edges record associations, causality, temporal order, contradiction, consolidation, and provenance. When a query arrives, activation flows through this graph and lights up the sites that are useful now.

## Problem

Current agent memory systems usually collapse into one of three shapes:

- **Vector recall** retrieves similar text but loses why fragments matter together.
- **Conversation archives** preserve chronology but make relevance the caller's problem.
- **Summaries and playbooks** compress experience but erase source detail and contradiction.

Long-running agents need fragment-level memory with natural decay, reinforcement on use, scoped provenance, contradiction visibility, and graph-based retrieval.

## Product Principles

| Principle | Meaning |
|---|---|
| Fragment first | Preserve individual conversation turns, extracted facts, and decisions instead of rewriting them into one summary |
| Retrieval is associative | A cue should activate nearby memories, which can activate further memories through typed relations |
| Use changes future recall | Access, commit, co-readout, and feedback leave measurable work behind |
| Forgetting is graceful | Unused knowledge fades in salience but remains available for precise reactivation |
| Provenance is first-class | Every site knows which peer, session, scope, and confidence produced it |
| Contradiction is visible | Conflicting facts are returned as tensions instead of being silently merged |
| Core is local and deterministic | The library performs graph storage and traversal, not LLM calls or network orchestration |

## System Image

```mermaid
flowchart LR
    observation["Observation"] --> perception["Perception gate"]
    perception --> graph["Conductive memory graph"]
    query["Query field"] --> flow["Activation flow"]
    graph --> flow
    flow --> readout["Readout"]
    readout --> context["ContextPackage"]
    context --> commit["Committed usage"]
    commit --> graph
    time["Time"] --> dissipation["Dissipation"]
    dissipation --> graph
```

The graph is not a static database. It is a driven-dissipative memory substrate. Queries perturb it transiently; committed use updates retained action and conductance; time leaks unused action away.

## Design Direction

- Use spreading activation / ACT-R as the theoretical base.
- Use the conductive-network frame as the representation for flow, conductance, impedance, and committed work.
- Keep storage pluggable through a trait; use SQLite as the zero-config default.
- Keep the core synchronous and local-first.
- Keep extraction, embedding generation, orchestration, and network APIs outside the core.

## Audience

This specification is for implementers of the Anamnesis library, reviewers evaluating API behavior, and downstream consumers deciding how to integrate agent memory. It is intentionally technical: public docs can later derive a shorter guide from this SSOT.
