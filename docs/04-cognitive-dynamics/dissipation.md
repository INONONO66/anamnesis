# Dissipation

Dissipation is the between-query process that lowers retained action for unused sites and weak unused edges. It is not deletion. A decayed site can still be reactivated by precise access.

The persistent quantity is retained action `A_i`. Public salience is a bounded projection:

```text
s_i = project_salience(A_i)
```

## Design Goals

| Goal | Meaning |
|---|---|
| power-law memory | Use ACT-R-like base-level decay, not arbitrary linear fading |
| lazy correctness | Apply accumulated leakage before reinforcement |
| tier compatibility | Allow protected tiers while preserving dynamics for ordinary sites |
| telemetry separation | Access timestamps support reports but are not the memory-strength reservoir |
| no deletion by default | Low-salience sites remain addressable |

## Leakage Model

ACT-R base-level activation follows a power-law form:

```text
B_i = ln(sum_j t_j^-d)
```

For a stored reservoir, maintenance applies the equivalent aging effect to retained action:

```text
A_i' = decay(A_i, delta_days, node_type, d)
s_i' = project_salience(A_i')
```

Node type supplies calibrated priors for decay rate and floor. Debug lifecycle nodes may be inert. Core identity may be protected.

## Time Unit

Between-query dissipation uses days unless explicitly stated otherwise. Query-local RWR iterations are dimensionless and must not be merged with this time scale.

## `d` Parameter

`d` is calibrated from re-access hazard:

```text
log P(reaccess after t) = c - d * log(t)
```

Common ACT-R defaults are acceptable priors, but production graphs should fit `d` from observed access logs.

## Readout Work: Decay First, Reinforce Second

Access applies lazy decay before reinforcement:

```text
A_after_decay = decay(A_before, now - accessed_at)
A_next        = reinforce(A_after_decay, work)
```

This order is invariant. It prevents an old site from gaining fresh strength without first paying accumulated leakage.

## Access Timestamp And Reservoirs

`accessed_at` is telemetry and a checkpoint for lazy dissipation. It is not retained action itself. The engine may update `accessed_at` on committed access, but query-only retrieval leaves it unchanged.

## Memory Tier

| Tier | Effect |
|---|---|
| Core | Protected from ordinary decay |
| Working | Decays slowly and is favored in packaging |
| Episodic | Decays normally |
| Archive | Low salience; excluded from broad retrieval unless explicitly targeted |

Tier is policy. It must not become a hidden direct salience setter.

## Tick Report

`tick(now)` returns a report containing:

- scanned node count,
- changed node count,
- salience projection deltas,
- tier transitions,
- archived/reactivated counts,
- flush status,
- errors.

## Relation To Deletion

Dissipation makes sites less likely to be read out. It does not remove them. Deletion is an explicit storage operation or retention policy decision, not the default result of forgetting.

## Failure Conditions

- Applying reinforcement before decay violates ordering.
- Non-finite retained action or salience becomes an error.
- Protected tiers must not decay under ordinary tick.
- Read-only retrieval must not update access time.
- A low-salience site must remain retrievable by precise id or targeted query.

## Cost

Batch tick is `O(node_count)` unless an adapter provides an index or incremental queue. Lazy decay is `O(1)` for the touched site. Edge leakage is proportional to scanned or indexed idle edges.

## Related Documents

- Access and commit ordering are defined in [interactions.md](interactions.md).
- Salience projection is defined in [graph-model.md](../02-knowledge-model/graph-model.md).
- Observability counters are defined in [observability.md](../07-quality-gates/observability.md).
