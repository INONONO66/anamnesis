# Frustration

Frustration is query-local constraint stress that appears when contradictory sites are active together. It is analogous to spin-glass frustration: not all active constraints can be satisfied at once.

Frustration does not decide truth and does not delete knowledge. It marks combinations that are risky to read together.

## Relation Types

Constraint edges are stored, but they do not propagate activation. RWR sums converging evidence; contradictions are not evidence to add.

| Edge | Meaning | Retrieval Handling |
|---|---|---|
| `Contradicts` | Two claims cannot both hold | Generate frustration stress when both endpoints are active; exclude from propagation |
| `Refutes` | Evidence refutes a hypothesis | Handled by debug lifecycle, not frustration |
| `Supersedes` | Newer fact replaces older fact | Handled by lineage and recency policy |
| `RejectedAlternative` | Considered and discarded option | Low-conductance supporting path |

Only `Contradicts` generates frustration stress.

## Stress Generation

Stress appears only when both endpoints are active:

```text
sigma_ij =
    contradiction_weight_ij
  * min(a_i, a_j)
  * scope_overlap
  * temporal_overlap
```

Each factor is a gate. If either endpoint is inactive, if the scopes do not overlap, or if the facts are not valid together, stress is zero.

## Link To Energy

Frustration contributes to the readout objective:

```text
E(S | Q) =
  - field_alignment
  - conductive_support
  + impedance_regularization
  + frustration_penalty(S, Sigma)
```

This is the same objective as [energy.md](energy.md): the leading `-`/`+` are structural descent-direction signs (`+/-1`), not tunable magnitudes. Field alignment and conductive support *lower* energy; impedance and frustration *raise* it. The positive sign on the frustration penalty is what matters here: contradiction raises energy, which encourages conflicting bundles to separate without deleting either side.

The caveat from [energy.md](energy.md) applies: this objective is a strict Lyapunov function only under symmetric coupling. Directed RWR has a stationary vector, and energy is an interpretive objective.

## Activation And Commit

Read-only retrieval returns active tensions but changes no reservoir. If a caller actually presents or uses the tension, commit may record a `TensionActivated` interaction:

```text
S_frustration(i, j) = tension_presented_ij * sigma_ij
```

This records that the conflict repeatedly matters in use. It does not decide which side is true.

## Return Shape

| Field | Meaning |
|---|---|
| `primary` | Site included in context |
| `conflicting` | Conflicting site |
| `edge_id` | Constraint edge |
| `stress` | Query-local stress value |
| `scope_overlap` | Scope gate contribution |
| `temporal_overlap` | Time gate contribution |
| `explanation` | Short human-readable reason |

## Visualization Boundary

A graph viewer may place contradiction pairs far apart, but layout coordinates do not affect retrieval score. Spring/repulsion layout belongs to visualization. Frustration belongs to cognition dynamics.

## Safety Rules

- Do not auto-delete contradictory sites.
- Query does not create contradiction edges.
- Read-only retrieval does not change reservoirs.
- Conflicting facts preserve lineage.
- Private contradictions do not leak across unauthorized scopes.
- Time-filtered queries return only stress valid at that time.

## Failure Conditions

- One active endpoint must not create stress.
- `Contradicts` must not propagate activation.
- Scope or time gate failure must prevent stress return.
- Reservoir mutation without commit violates read-only retrieval.

## Cost

Stress calculation is proportional to contradiction edges adjacent to selected sites. Because contradiction edges are excluded from RWR propagation, they do not increase activation-flow traversal cost.

## Related Documents

- Frustration penalty appears in [energy.md](energy.md).
- Edge types are defined in [graph-model.md](../02-knowledge-model/graph-model.md).
- `TensionActivated` is defined in [interactions.md](interactions.md).
- Time filtering is defined in [temporal-model.md](../02-knowledge-model/temporal-model.md).
