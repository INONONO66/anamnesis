# Dissipation

Dissipation is the between-query process that lowers retained action for unused sites and weak unused edges. It is not deletion. A decayed site can still be reactivated by precise access.

The persistent quantity is retained action `A_i`, which decomposes into two terms:

```text
A_i = B_i + P_i
```

`B_i` is the multi-trace ACT-R base-level activation over the node's access-trace history; it owns forgetting and use-driven reinforcement and is computed on demand from the trace history. Each access trace carries its own decay rate, fixed at the moment the trace is created (Pavlik & Anderson 2005), so massed re-presentation forgets fast and spaced re-presentation stays durable. `P_i` is a separate persistent evidence prior (encoding surprise, feedback / social reinforcement, peer trust) that does NOT undergo base-level decay. Public salience is the bounded logistic projection of the sum:

```text
s_i = logistic(B_i + P_i)
```

## Design Goals

| Goal | Meaning |
|---|---|
| power-law memory | Use ACT-R-like multi-trace base-level decay, not arbitrary linear fading; with per-trace activation-dependent decay the trace sum reproduces power-law forgetting and the spacing effect |
| decay-first by construction | A committed access appends a trace stamped at `now`, and `B_i` ages prior traces to `now` before adding it, so ordering is intrinsic |
| prior separation | The evidence prior `P_i` (encoding surprise, feedback, peer trust) is decay-exempt and never charged against the use-driven base level |
| tier compatibility | Allow protected tiers while preserving dynamics for ordinary sites |
| telemetry separation | Access timestamps support reports; the access-trace history is the load-bearing input to `B_i` |
| no deletion by default | Low-salience sites remain addressable |

## Leakage Model

Forgetting lives entirely in `B_i`, the multi-trace ACT-R base-level activation over the node's access-trace history:

```text
B_i = ln( Σ_j (now − t_j)^(−d_j) )

where each trace j stores its own decay rate d_j, fixed at creation:
  m_j = ln( Σ_{k existing} (t_j − t_k)^(−d_k) )   // activation from prior traces, evaluated at t_j; empty history ⇒ m_j = −∞
  d_j = m_type · ( c · e^{m_j} + α )              // computed once, then stored immutably with the trace
```

The `t_j` are the node's access traces (a creation trace plus each committed access; a bounded 32-trace window). `B_i` is computed on demand from the trace history; it is NOT maintained by incremental scalar decay. It falls as traces age (power-law) and rises when a committed access appends a fresh trace. Because each `d_j` is set from the activation present when its trace is laid down, a trace created during massed re-presentation (high `m_j`) gets a high `d_j` and decays fast, while a trace created after a spacing gap (low `m_j`) gets `d_j` near the floor `m_type·α` and stays durable. This is what makes the multi-trace sum reproduce power-law forgetting AND the spacing effect (see the [crossover caveat](#spacing-and-testing) below). There is no `decay(A_i, Δt)` scalar shift and no separate maintenance step that mutates a stored reservoir: aging is recomputed from `now` against the traces.

Decay is governed by two calibrated priors rather than a single exponent: `α` (the floor decay rate when activation is zero) and `c` (the activation sensitivity). Node type does not supply a separate decay knob: per-tier leak is the `node_type` policy multiplier `m_type` applied as an OUTER factor on the per-trace rate, `d_j = m_type·(c·e^{m_j}+α)` (Core zero, Working below one, Episodic one, Archive excluded). A type with `m_type = 0` yields `d_j = 0` for every trace, so its `B_i` is permanent. The evidence prior `P_i` is decay-exempt and is untouched by leakage. Debug lifecycle nodes may be inert. Core identity may be protected.

## Time Unit

Between-query dissipation uses days unless explicitly stated otherwise. Query-local RWR iterations are dimensionless and must not be merged with this time scale.

## Decay Parameters

The single global decay exponent is replaced by two calibrated priors that together determine each per-trace `d_j`:

```text
d_j = m_type · ( c · e^{m_j} + α )
```

- `α` (`DECAY_INTERCEPT`, calibrated default `0.40`) is the floor decay rate that applies when activation is zero (`e^{m_j} → 0`). A single-trace node — one with no prior traces to raise its activation — forgets along the slope `−m_type·α`.
- `c` (`DECAY_SCALE`, calibrated default `2.0`) is the activation sensitivity: how steeply the decay rate climbs as the activation `m_j` present at trace creation rises. It is the smallest scale that gives a robust recency-controlled spacing margin at a delayed test.

These are the only two free decay parameters; they replace the former single exponent and there is no separate decay multiplier knob. Common ACT-R-derived values are acceptable priors, but production graphs may refit `α` and `c` from observed access logs. Forgetting lives in `B_i` governed by these two priors, and the per-tier leak follows from them together with the `node_type` policy multiplier `m_type`.

## Readout Work: Decay-First Is Intrinsic

A committed access appends a trace stamped at `now`, and `B_i` always ages prior traces to `now` before adding the new one. The new trace's own decay rate `d_now` is computed from the activation of the existing traces (evaluated at `now`) before the trace is appended, then frozen with it. Decay-first ordering is therefore intrinsic to how `B_i` is computed:

```text
m_now = ln( Σ_j (now − t_j)^(−d_j) )      // activation from existing traces, before appending
d_now = m_type · ( c · e^{m_now} + α )    // frozen with the new trace
B_i   = ln( Σ_j (now − t_j)^(−d_j) )      // including the freshly appended (now, d_now) trace
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

## Spacing And Testing

The spacing effect is genuine here, not stipulated. Each trace's decay rate `d_j` is set by the activation `m_j` present when the trace is created, so the schedule of presentations shapes the decay profile of the resulting trace window:

- Massed re-presentation lands new traces while activation is still high, so those traces get high `d_j` and decay quickly. Later strength is weak.
- Spaced re-presentation lands new traces after activation has fallen, so those traces get `d_j` near the floor `m_type·α` and stay durable. Later strength is stronger.

This is a true activation-dependent mechanism, not a recency artifact: holding the final presentation time fixed (recency-controlled), spaced practice still ends above clustered practice. The advantage emerges only at a sufficiently delayed test — there is a retention-interval crossover where clustered practice can lead at short delays and spaced practice overtakes it later. That crossover is the documented human spacing × retention-interval interaction, which a pure recency model cannot produce, so it is positive evidence rather than a defect.

The testing effect is a different phenomenon and is NOT cleanly reproduced by this model. Activation-dependent decay treats every presentation alike regardless of whether it was a study or a retrieval, so it does not capture the human test-vs-restudy advantage at equal timing. Do not claim the model reproduces the testing effect; see the [ADR-0008 caveat](../adr/0008-powerlaw-dissipation.md).

## Failure Conditions

- Maintaining `B_i` by incremental scalar decay instead of recomputing it from the trace history violates the model.
- Charging the evidence prior `P_i` with use-driven base-level decay is an error; `P_i` is decay-exempt.
- Per-trace decay rates `d_j` must be computed at trace-creation time from the activation of existing traces and stored with the trace; recomputing them from activation at read time is an error.
- A massed re-presentation (high activation at creation) must produce a high `d_j` (fast decay); a spaced re-presentation (low activation at creation) must produce a low `d_j` near `m_type·α` (durable strength). Collapsing all traces to one shared decay rate destroys the spacing effect.
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
