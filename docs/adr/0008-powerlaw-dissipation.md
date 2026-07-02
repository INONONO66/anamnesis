# 0008. Forgetting Is Multi-Trace Base-Level Decay With A Separate Evidence Prior

*Superseded in part by [ADR-0014](0014-shrink-to-product.md) (v0.10.0 shrink): "peer trust" is listed below as one component of the decay-exempt evidence prior `P_i`; the peer/trust subsystem that fed it was removed (readout trust term now a neutral `1.0`), so `P_i` carries only encoding surprise and feedback. ADR-0014 also discloses the by-design decay-rate coarsenings that adjusted per-type `m_type` multipliers. The multi-trace base-level model itself stands.*

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

`B_i` is the multi-trace ACT-R base-level activation over the node's access-trace history. Following Pavlik & Anderson (2005), decay is activation-dependent: EACH trace stores its own decay rate, computed once at the moment the trace is created:

```text
B_i = ln( Σ_j (now − t_j)^(−d_j) )

where each trace j is a pair (t_j, d_j):
  m_j = ln( Σ_{k existing} (t_j − t_k)^(−d_k) )   // activation from prior traces, evaluated at j's creation time
  d_j = m_type · ( c · e^{m_j} + α )              // per-trace decay computed at creation, never recomputed
```

The `t_j` are the timestamps of the node's access traces (a creation trace plus each committed access; a bounded 32-trace window). The creation trace has an empty prior history, so `m_j = −∞`, `e^{m_j} = 0`, and `d_j = m_type·α` (the floor). Elapsed time `now − t_j` is floored to a minimum positive delta `Δ_min` (1 ms in the reference implementation), so a committed access stamped at `now` contributes `Δ_min^(−d_j)` instead of diverging. `m_type` is the `node_type` policy multiplier; it is the OUTER factor, so `m_type = 0` types decay-exempt (`d_j = 0`, permanent). Instead of one global decay prior `d`, there are TWO calibrated priors:

- `α` (intercept): floor decay rate when activation is zero.
- `c` (scale): how much current activation increases the per-trace decay rate.

`B_i` owns forgetting and use-driven reinforcement: it falls as traces age and rises when a committed access appends a fresh trace stamped at `now` (with its own `d_now` computed from current activation). It is computed on demand from the trace history, not maintained by incremental scalar decay.

`P_i` is a separate persistent prior holding encoding surprise (`P_i ← k·eps` at allocation, ADR-0009), feedback / social reinforcement (`dP_i = eta·(lambda − predicted_i)`), and peer trust. `P_i` does NOT undergo base-level decay; it is a decay-exempt evidence offset.

Public salience is the bounded logistic projection of the sum:

```text
s_i = logistic(B_i + P_i)
```

Decay-first ordering is intrinsic: a committed access appends a trace stamped at `now`, and `B_i` ages prior traces to `now` before adding it, so there is no separate decay-then-reinforce scalar step. Low salience hides a site from broad retrieval but does not delete it.

## Consequences

Benefits:

- Matches ACT-R base-level memory shape.
- The activation-dependent multi-trace sum reproduces power-law forgetting AND the spacing effect genuinely, which a single scalar reservoir cannot. The testing effect is NOT cleanly reproduced by activation-dependent decay alone; see the caveat below.
- Preserves source fragments for reactivation.
- Makes stale knowledge less likely without erasing provenance.
- Separates use-driven forgetting (`B_i`) from durable evidence (`P_i`), so encoding surprise, feedback, and peer trust are not eroded by disuse.

Tradeoffs:

- Full tick can be expensive on large graphs.
- Trace-window management: each node carries a bounded 32-trace history of `(t_j, d_j)` pairs that must be appended on committed access and capped.
- Per-trace decay computation: `d_j` must be computed at trace-creation time from the current activation `m_j` and persisted alongside `t_j`; it is never recomputed at readout.
- `B_i` is recomputed from traces on demand rather than read from a stored scalar.
- Decay constants must be calibrated (now `α` and `c` rather than a single `d`).

## Spacing And The Honest Testing Caveat

The spacing effect — stronger long-term retention for spaced practice than for massed practice — is genuinely reproduced by activation-dependent decay, and it emerges from the per-trace mechanism rather than from recency:

- Massed re-presentation occurs at high activation `m_j`, producing a high `d_j`, so that trace decays fast and contributes little durable strength.
- Spaced re-presentation occurs at low activation `m_j`, producing a low `d_j` (≈ `m_type·α`), so that trace is durable and lifts later strength.

This is a true spacing × retention-interval interaction: spaced practice wins only at a sufficiently DELAYED test. Holding the last study event fixed (recency-controlled), an early test favors clustered/massed practice and a later test favors spaced practice (the crossover is around a moderate retention interval). A pure recency model cannot produce such a crossover, so the crossover is positive evidence that the win comes from activation-dependent `d_j`, not from recency. The single-trace forgetting curve remains log-linear with slope `−m_type·α`.

The testing effect — that a retrieval attempt aids retention more than an equivalent restudy at matched timing — is NOT cleanly reproduced by this model, and we do not claim it. To the base-level sum a presentation is a presentation regardless of whether it was a test or a restudy, so activation-dependent decay alone cannot dissociate the two; capturing it would require additional mechanisms (elaboration, transfer-appropriate processing) beyond what is modeled here. What the engine DOES express is a distinct commitment principle: a committed retrieval appends a durable trace and raises `B_i`, whereas a read-only retrieval mutates nothing. That is an engine invariant about commitment, explicitly NOT the human testing effect.

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

Rejected. A single maintained scalar updated by `decay(A_i, Δt)` then a reinforcement add reproduces only plain power-law forgetting; it cannot express the spacing effect (which requires per-trace, activation-dependent decay) and forces forgetting and durable evidence onto one reservoir that disuse then erodes. (The testing effect remains unresolved even under activation-dependent decay; see the caveat above.)
