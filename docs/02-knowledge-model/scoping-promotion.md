# Scope And Promotion

> **Scope hierarchy is roadmap (as of [v0.10.0](../adr/0014-shrink-to-product.md)).** The `ScopeRelation` model below — `Ancestor` / `Descendant` / `Sibling` / `Disjoint` / `Equal` / `Universal` with per-relation retrieval handling — was **removed in the v0.10.0 shrink**. What ships is the reduced form: `ScopePath` is an opaque, canonicalized string with an `is_universal()` flag, and scope scoring is a **two-branch** weight (universal vs. non-matching). Promotion (`crystallize` + `ConsolidatedFrom`) still works, but without ancestor-aware upward routing. Read the multi-relation hierarchy here as **design intent** for a future scope layer, not current behavior.

Scope expresses where knowledge is valid and who may see it. Promotion moves repeatedly confirmed narrow-scope knowledge into a broader scope by adding synthesis, not by overwriting the source fragment.

## Scope Path

A scope path is a hierarchical string:

```text
universal
project/anamnesis
project/anamnesis/session/2026-06-05
```

`ScopePath::universal()` is the global scope, represented by the empty path. Other paths are compared structurally by segment prefixes.

Paths are canonical before comparison:

- `/` is the only separator. Segments are matched verbatim and case-sensitively; there is no case folding and no separator escaping.
- A trailing `/` is trimmed (`a/b/` becomes `a/b`).
- Consecutive slashes and empty segments are rejected at construction, not collapsed (`a//b` and `a//` are errors, never `a/b` or `a`). Only `ScopePath::universal()` may produce the empty path.

Structural comparison then operates on whole segments: a path is an `Ancestor` of another when its segment sequence is a strict prefix of the other's, a `Descendant` in the reverse case, `Sibling` when both share the same parent segments but differ in the last, and `Disjoint` otherwise. The empty (universal) path takes precedence and yields `Universal` against any non-empty path.

## Scope Relation

Two scope paths have exactly one relation. The relation is the first match in this precedence order, which resolves the overlap between `Universal` (the empty path is a prefix of every path) and the structural `Equal`/`Ancestor` cases:

1. Both paths universal → `Equal`
2. Exactly one path universal → `Universal`
3. Identical paths → `Equal`
4. Self is a strict prefix of other → `Ancestor`
5. Other is a strict prefix of self → `Descendant`
6. Same parent, different final segment → `Sibling`
7. Otherwise → `Disjoint`

| Relation | Meaning | Retrieval Handling |
|---|---|---|
| `Equal` | Same scope | Strong relevance |
| `Ancestor` | Wider than query scope | Usable if policy permits |
| `Descendant` | Narrower than query scope | Requires visibility check |
| `Sibling` | Same parent, different branch | Lower relevance |
| `Disjoint` | No relation | Excluded by default |
| `Universal` | Exactly one path is universal | Always a candidate |

## Promotion Conditions

| Condition | Meaning |
|---|---|
| repeated use | Retrieved across multiple sessions |
| corroboration | Confirmed by multiple peers |
| low contradiction | Few active conflict edges |
| stable content | No recent supersession |
| explicit approval | Consumer requested promotion |

## Promotion Flow

```mermaid
flowchart LR
    local["Local fragments"] --> candidate["Promotion candidate"]
    candidate --> review["Policy / approval"]
    review --> synthesis["Broader-scope synthesis"]
    synthesis --> sources["ConsolidatedFrom edges"]
```

The source nodes remain. Promotion adds a broader-scope synthesis site linked back to sources. It must not erase the original fragments or their scope.

## Access Rules

- If the query scope cannot see a private fragment, that fragment is excluded.
- Universal knowledge may be a candidate for any query.
- Session-local state naturally moves to lower priority over time.
- Promotion may generate candidates automatically, but final application follows consumer policy.

## Related Documents

- The origin model is defined in [peer-identity.md](peer-identity.md).
- Synthesis and packaging are described in [pipeline.md](../05-context-retrieval/pipeline.md).
- Storage indexes are described in [storage.md](../03-persistence/storage.md).
