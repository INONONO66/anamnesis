# 0002. Reservoir And Projection State

- Status: Accepted
- Date: 2026-06-05
- Related: [overview](../04-cognitive-dynamics/overview.md), [graph-model](../02-knowledge-model/graph-model.md)

## Context

The engine exposes intuitive values such as salience and edge weight, but the dynamics need unbounded or differently scaled internal quantities. If projections become authoritative, public knobs can bypass the theory.

## Decision

Store authoritative reservoirs separately from bounded projections:

| Reservoir | Projection |
|---|---|
| `retained_action A_i = B_i + P_i` | `salience s_i` |
| `conductance C_ij` | `edge weight w_ij` |

The node reservoir is no longer a single scalar. It is the node's access-trace history (a bounded 32-trace window: a creation trace plus each committed access) — from which the base-level `B_i = ln( Σ_j (now − t_j)^(−d_j) )` (per-trace activation-dependent decay) is computed on demand — together with the persistent evidence prior `P_i` (encoding surprise, feedback / social reinforcement, peer trust). `B_i` owns forgetting and use-driven reinforcement; `P_i` is a decay-exempt evidence offset.

Reservoirs are changed only by interactions. Projections are derived views used by APIs, ranking, packaging, and storage indexes.

```text
s_i = logistic(B_i + P_i)
w_ij = project_weight(C_ij)
```

Public operations may request events such as access, feedback, or link creation. They must not directly assign reservoir values as semantic operations.

## Consequences

Benefits:

- Dynamics can use log-odds and log-LR units while public APIs stay bounded.
- Readout and storage get stable values.
- Direct salience/weight editing is avoided.

Tradeoffs:

- Implementations must keep reservoirs and projections synchronized.
- Storage adapters need hot-field support for projections.
- Documentation must consistently distinguish the two layers.

## Preserved Properties

- Retrieval reads reservoirs and projections but does not mutate them.
- Commit updates reservoirs, then recomputes projections.
- Projections remain bounded.
- Observability can report both raw dynamics and public values.

## Alternatives Considered

### Make `salience` and `weight` authoritative

Rejected. This makes direct tuning easy but breaks Bayesian/ACT-R semantics.

### Expose only reservoirs

Rejected. Consumers need bounded values for ranking, display, and stable API contracts.

### Persist reservoirs and projections as independent truth

Rejected. Two authoritative values can diverge. Projection must be derived.

### Add a separate decay multiplier

Rejected. Forgetting belongs in the base-level term `B_i`, which falls as its access traces age (power-law) and rises when a committed access appends a fresh trace; it is not an extra scalar decay knob. `B_i` is computed on demand from the trace history, not maintained by incremental scalar decay.
