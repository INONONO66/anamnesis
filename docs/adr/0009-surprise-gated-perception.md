# 0009. Ingest Magnitude Is Bayesian Surprise

- Status: Accepted
- Date: 2026-06-05
- Related: [perception](../04-cognitive-dynamics/perception.md), [conductance](../04-cognitive-dynamics/conductance.md)

## Context

If every new fragment receives the same initial salience, duplicates and noise can become as strong as belief-changing observations. If low novelty is rejected, repeated useful evidence cannot reinforce memory.

## Decision

Separate rejection from routing:

- Reject only untrusted or unaffordable observations.
- Allocate a new site when novelty exceeds the separation threshold.
- Route familiar observations to an existing site and reinforce it.

For allocated sites, the initial evidence prior `P_i` is surprise-gated:

```text
eps = (embedding_obs - embedding_pred)^T Sigma^-1 (embedding_obs - embedding_pred)
P_i_init = k * eps
```

`eps` is a computable proxy for Bayesian surprise. It measures how much the observation moves the graph's expectation.

## Consequences

Benefits:

- Familiar useful input reinforces instead of disappearing.
- Noisy or redundant fragments receive a small initial evidence prior `P_i`.
- Belief-changing input receives a stronger initial evidence prior `P_i`.
- Because `P_i` is a decay-exempt evidence offset (it does not undergo base-level use-driven decay), encoding surprise persists as a stable prior rather than bleeding away with the base-level term `B_i`.
- Perception aligns with the `A_i = B_i + P_i` decomposition: allocation also stamps a creation trace that seeds `B_i`.

Tradeoffs:

- Requires a calibrated novelty threshold.
- Precision estimates are approximations unless variance is stored.
- Paraphrase routing may need advisory adjudication near the boundary.

## Alternatives Considered

### Reject low novelty

Rejected. It blocks repetition and spacing effects.

### Flat initial salience

Rejected. It cannot distinguish noise from meaningful surprise.

### Let caller choose initial salience

Rejected. It bypasses the `A_i = B_i + P_i` decomposition and the logistic projection `s_i = logistic(B_i + P_i)`.
