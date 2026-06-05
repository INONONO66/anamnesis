# Scope And Promotion

Scope expresses where knowledge is valid and who may see it. Promotion moves repeatedly confirmed narrow-scope knowledge into a broader scope by adding synthesis, not by overwriting the source fragment.

## Scope Path

A scope path is a hierarchical string:

```text
universal
project/anamnesis
project/anamnesis/session/2026-06-05
```

`ScopePath::universal()` is the global scope. Other paths are compared structurally.

## Scope Relation

| Relation | Meaning | Retrieval Handling |
|---|---|---|
| `Equal` | Same scope | Strong relevance |
| `Ancestor` | Wider than query scope | Usable if policy permits |
| `Descendant` | Narrower than query scope | Requires visibility check |
| `Sibling` | Same parent, different branch | Lower relevance |
| `Disjoint` | No relation | Excluded by default |
| `Universal` | Applies everywhere | Always a candidate |

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
