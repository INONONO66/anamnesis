# ADR-006: Knowledge Scoping and Promotion

**Status**: Accepted

## Context

Anamnesis stores memory from many runs, agents, domains, and projects in one cognitive graph. Some knowledge is only valid inside a single workspace. Some applies to a broader life or work domain. Some is useful across all domains. Some starts local, then becomes a general operating principle after repeated support.

Without explicit scoping, a shared graph has two failure modes:

1. **Over-generalization** — a project-specific convention appears in unrelated projects or personal domains.
2. **Under-transfer** — a hard-won lesson remains trapped in the project where it was first observed.
3. **Domain bleed** — work habits, personal routines, and private projects influence each other when they should remain separate.

The engine needs a deterministic scope model that supports domain separation, project lock-in, universal knowledge, and additive promotion from local evidence to general principles.

## Decision

Use `Origin.scope` as the scope key for knowledge:

```rust
/// A validated hierarchical scope path stored as a single slash-delimited string.
/// Construct via `ScopePath::new("work/company-a")` or `ScopePath::universal()`.
pub struct ScopePath(String);

pub struct Origin {
    pub agent_id: String,
    pub session_id: String,
    pub scope: ScopePath,
    pub confidence: f64,
}
```

### 1. Scope levels

Conceptually, scopes are hierarchical:

```text
universal
  -> domain/category
    -> project/workspace
      -> session
        -> event/fragment
```

Examples:

```text
universal
work/company-a
work/company-a/backend-service
personal/daily-life
personal-projects/anamnesis
personal-projects/game-prototype
```

The API stores this hierarchy in `ScopePath`. A scoped identifier may encode a domain path such as `work/company-a` or `personal-projects/anamnesis`; the root `universal` scope represents knowledge that applies across domains.

| Scope | Representation | Meaning |
|:------|:---------------|:--------|
| Session evidence | `session_id` plus a concrete `scope` | A specific event, observation, or turn from one run |
| Domain-scoped knowledge | `work`, `personal`, or another scope prefix | Valid for a broad category of life/work |
| Project-scoped knowledge | `domain/project` | Valid for one project or workspace |
| Universal knowledge | `universal` | Applies across domains unless contradicted by local context |

The scope model is not access control. It is a recall and ranking signal.

### 2. Scope-aware recall

Queries may provide a current scoped path. During ranking, scope influences relevance:

| Node scope | Example | Query role |
|:-----------|:--------|:-----------|
| Same project/workspace | `personal-projects/anamnesis` | Strongest boost |
| Same domain/category | `personal-projects/*` | Medium boost |
| Universal | `universal` | Always available with broad weight |
| Other domain | `work/*` when querying `personal/*` | Downweighted unless explicitly requested or entity-linked |

This keeps local conventions local while allowing related-domain habits and universal principles to participate in recall.

### 3. Promotion is additive crystallization

Scoped memories may be promoted upward by creating a new node at a broader scope and linking it back to source evidence with `ConsolidatedFrom` edges.

Promotion can happen at multiple levels:

| From | To | Example |
|:-----|:---|:--------|
| Session evidence | Project knowledge | One debugging session becomes a project gotcha |
| Project knowledge | Domain knowledge | Several personal projects reveal the same development habit |
| Domain knowledge | Universal knowledge | Work and personal projects both support a general principle |

Universal promotion uses `ScopePath::universal()`. Domain-level promotion uses a broader scoped path, such as `personal-projects` rather than `personal-projects/anamnesis`.

Promotion must not mutate or delete the original scoped memories.

```text
Universal principle
  ├─ ConsolidatedFrom -> personal project evidence
  ├─ ConsolidatedFrom -> work project evidence
  └─ ConsolidatedFrom -> daily-life evidence
```

The broader node stores the abstracted principle. The source nodes preserve when, where, and why the principle emerged.

### 4. Promotion criteria

A scoped pattern is a candidate for broader promotion when it has:

- support from multiple projects, domains, or independent sessions,
- high average confidence,
- low contradiction or exception rate,
- sustained salience after repeated use,
- an abstraction that removes project-specific names, paths, and tools.

Example:

```text
Project evidence:
  “This crate requires checking public API before refactors.”
  “This service breaks if refactors ignore existing tests.”
  “This frontend relies on established test fixtures.”

Universal principle:
  “Before refactoring unfamiliar code, inspect public boundaries and existing tests first.”
```

If a pattern has many exceptions, it should become a conditional principle or remain project-scoped.

## What the Engine Does Not Do

- It does not decide by itself that a local pattern is universal.
- It does not call an LLM to abstract a principle.
- It does not delete source memories during promotion.
- It does not enforce security isolation between projects.

The consumer decides when a pattern is universal and provides the crystallized content. The engine provides the graph primitives, provenance links, salience dynamics, and scope-aware recall.

## Rationale

- **Single graph, scoped recall**: One graph can store universal, domain-scoped, and project-specific knowledge without separate databases.
- **Transfer without contamination**: Broader nodes allow cross-scope learning; scope weights prevent accidental leakage of local conventions.
- **Evidence preservation**: `ConsolidatedFrom` keeps promotion auditable.
- **Physics consistency**: Promoted knowledge is an ordinary graph node. It has salience, origin, validity, edges, and decay behavior according to its `KnowledgeType`.

## Consequences

- Consumers should populate `Origin.scope` consistently as a stable path.
- Universal knowledge should use the `universal` scope.
- Domain-scoped knowledge should use broad paths such as `work`, `personal`, or `personal-projects`.
- Project-specific exceptions should remain attached as scoped `Gotcha`, `Contradicts`, `RejectedAlternative`, or `Supersedes` nodes.
- `crystallize()` is the natural API for promotion because it creates source provenance and reinforces contributing memories.

## Related Decisions

- [ADR-003: Multi-Agent Memory Support](./003-multi-agent-memory.md)
- [ADR-004: Universal Knowledge Memory with Identity](./004-universal-knowledge-memory.md)
- [ADR-005: Query Crystallization](./005-query-crystallization.md)
