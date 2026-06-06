# 0008. Forgetting Is Multi-Trace Base-Level Decay With A Separate Evidence Prior

- Status: Accepted
- Date: 2026-06-05
- Related: [dissipation](../04-cognitive-dynamics/dissipation.md), [temporal-model](../02-knowledge-model/temporal-model.md), [surprise-gated-perception](0009-surprise-gated-perception.md)

## Context

Memory should fade naturally without deleting source fragments. Human and ACT-R memory literature favors power-law decay over arbitrary linear fading. Forgetting and use-driven reinforcement belong together, but encoding surprise, feedback, and peer trust are evidence that should not be eroded by disuse.

## Decision

Decompose persistent node strength into a base-level term and a decay-exempt evidence prior:

```text
A_i = B_i + P_i
```

`B_i` is the multi-trace ACT-R base-level activation over the node's access-trace history:

```text
B_i = ln( Σ_j (now − t_j)^(−d·m_type) )
```

The `t_j` are the node's access traces (a creation trace plus each committed access; a bounded 32-trace window), and `m_type` is the `node_type` policy multiplier on the single decay prior `d`. `B_i` owns forgetting and use-driven reinforcement: it falls as traces age and rises when a committed access appends a fresh trace stamped at `now`. It is computed on demand from the trace history, not maintained by incremental scalar decay.

`P_i` is a separate persistent prior holding encoding surprise (`P_i ← k·eps` at allocation, ADR-0009), feedback / social reinforcement (`dP_i = eta·(lambda − predicted_i)`), and peer trust. `P_i` does NOT undergo base-level decay; it is a decay-exempt evidence offset.

Public salience is the bounded logistic projection of the sum:

```text
s_i = logistic(B_i + P_i)
```

Decay-first ordering is intrinsic: a committed access appends a trace stamped at `now`, and `B_i` ages prior traces to `now` before adding it, so there is no separate decay-then-reinforce scalar step. Low salience hides a site from broad retrieval but does not delete it.

## Consequences

Benefits:

- Matches ACT-R base-level memory shape.
- The multi-trace sum reproduces power-law forgetting AND the testing and spacing effects, which a single scalar reservoir cannot.
- Preserves source fragments for reactivation.
- Makes stale knowledge less likely without erasing provenance.
- Separates use-driven forgetting (`B_i`) from durable evidence (`P_i`), so encoding surprise, feedback, and peer trust are not eroded by disuse.

Tradeoffs:

- Full tick can be expensive on large graphs.
- Trace-window management: each node carries a bounded 32-trace history that must be appended on committed access and capped.
- `B_i` is recomputed from traces on demand rather than read from a stored scalar.
- Decay constants must be calibrated.

## Alternatives Considered

### Linear decay

Rejected. It lacks cognitive basis and behaves poorly across long horizons.

### Delete on threshold

Rejected. It destroys recoverability and provenance.

### Separate archive tiers only

Rejected. Tiering alone is a policy label, not a dynamics model.

### Reinforce before decay

Structurally impossible under this model. There is no scalar reinforce step: a committed access appends a trace stamped at `now`, and `B_i` ages all prior traces to `now` inside the same sum that adds the new trace. A stale site cannot avoid accumulated leakage because aging is recomputed from `now` every time `B_i` is read.

### Single scalar reservoir with decay-then-reinforce

Rejected. A single maintained scalar updated by `decay(A_i, Δt)` then a reinforcement add reproduces only plain power-law forgetting; it cannot express the testing and spacing effects and forces forgetting and durable evidence onto one reservoir that disuse then erodes.
