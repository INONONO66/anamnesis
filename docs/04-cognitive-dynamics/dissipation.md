# Dissipation

Dissipation is the between-query process that lowers retained action for unused sites and weak unused edges. It is not deletion. A decayed site can still be reactivated by precise access.

The persistent quantity is retained action `A_i`, which decomposes into two terms:

```text
A_i = B_i + P_i
```

`B_i` is the multi-trace ACT-R base-level activation over the node's access-trace history; it owns forgetting and use-driven reinforcement and is computed on demand from the trace history. `P_i` is a separate persistent evidence prior (encoding surprise, feedback / social reinforcement, peer trust) that does NOT undergo base-level decay. Public salience is the bounded logistic projection of the sum:

```text
s_i = logistic(B_i + P_i)
```

## Design Goals

| Goal | Meaning |
|---|---|
| power-law memory | Use ACT-R-like multi-trace base-level decay, not arbitrary linear fading; the trace sum reproduces power-law forgetting plus the testing and spacing effects |
| decay-first by construction | A committed access appends a trace stamped at `now`, and `B_i` ages prior traces to `now` before adding it, so ordering is intrinsic |
| prior separation | The evidence prior `P_i` (encoding surprise, feedback, peer trust) is decay-exempt and never charged against the use-driven base level |
| tier compatibility | Allow protected tiers while preserving dynamics for ordinary sites |
| telemetry separation | Access timestamps support reports; the access-trace history is the load-bearing input to `B_i` |
| no deletion by default | Low-salience sites remain addressable |

## Leakage Model

Forgetting lives entirely in `B_i`, the multi-trace ACT-R base-level activation over the node's access-trace history:

```text
B_i = ln( Σ_j (now − t_j)^(−d·m_type) )
```

The `t_j` are the node's access traces (a creation trace plus each committed access; a bounded 32-trace window). `B_i` is computed on demand from the trace history; it is NOT maintained by incremental scalar decay. It falls as traces age (power-law) and rises when a committed access appends a fresh trace, and the multi-trace sum reproduces power-law forgetting AND the testing and spacing effects. There is no `decay(A_i, Δt)` scalar shift and no separate maintenance step that mutates a stored reservoir: aging is recomputed from `now` against the traces.

`d` is the single free decay prior; it sets the exponent of the base-level sum. Node type does not supply a separate decay knob: per-tier leak is the `node_type` policy multiplier `m_type` on `d` (Core near zero, Working below one, Episodic one, Archive excluded), not an independent calibrated rate. The evidence prior `P_i` is decay-exempt and is untouched by leakage. Debug lifecycle nodes may be inert. Core identity may be protected.

## Time Unit

Between-query dissipation uses days unless explicitly stated otherwise. Query-local RWR iterations are dimensionless and must not be merged with this time scale.

## `d` Parameter

`d` is calibrated from re-access hazard:

```text
log P(reaccess after t) = c - d * log(t)
```

Common ACT-R defaults are acceptable priors, but production graphs should fit `d` from observed access logs. `d` is the only free decay parameter: it is the exponent of the base-level sum, and there is no separate decay multiplier knob. Forgetting lives in `B_i` governed by `d`, and the per-tier leak follows from `d` together with the `node_type` policy multiplier `m_type`.

## Readout Work: Decay-First Is Intrinsic

A committed access appends a trace stamped at `now`, and `B_i` always ages prior traces to `now` before adding the new one. Decay-first ordering is therefore intrinsic to how `B_i` is computed:

```text
B_i = ln( Σ_j (now − t_j)^(−d·m_type) )   // appends a fresh trace at now
```

There is no separate decay-then-reinforce scalar step and no `reinforce(A, work)` function. An old site cannot gain fresh strength without first paying accumulated leakage, because the prior traces are aged to `now` inside the same sum that adds the new trace.

## Access Timestamp And Traces

The access-trace history is load-bearing: the trace timestamps `t_j` are the input to `B_i`, and a committed access appends a new trace. `accessed_at` remains telemetry (a convenience marker of the most recent access); it is not retained action itself and is not the input to `B_i`. The engine may update `accessed_at` on committed access, but query-only retrieval leaves both the traces and `accessed_at` unchanged.

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
- flush status.

## Relation To Deletion

Dissipation makes sites less likely to be read out. It does not remove them. Deletion is an explicit storage operation or retention policy decision, not the default result of forgetting.

## Failure Conditions

- Maintaining `B_i` by incremental scalar decay instead of recomputing it from the trace history violates the model.
- Charging the evidence prior `P_i` with use-driven base-level decay is an error; `P_i` is decay-exempt.
- Non-finite retained action or salience becomes an error.
- Protected tiers must not decay under ordinary tick.
- Read-only retrieval must not append a trace or update access time.
- A low-salience site must remain retrievable by precise id or targeted query.

## Cost

Computing `B_i` is `O(W)` in the size of the trace window, with `W ≤ 32`, so per-site cost is bounded by the trace cap. Batch tick is `O(node_count · W)` unless an adapter provides an index or incremental queue. Recomputing `B_i` for a single touched site is `O(W)`. Edge leakage is proportional to scanned or indexed idle edges.

## Related Documents

- Access and commit ordering are defined in [interactions.md](interactions.md).
- Salience projection is defined in [graph-model.md](../02-knowledge-model/graph-model.md).
- Observability counters are defined in [observability.md](../07-quality-gates/observability.md).
