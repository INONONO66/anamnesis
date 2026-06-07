# 0005. Additive Activation Flow, Never Max-Path Only

- Status: Accepted
- Date: 2026-06-05
- Related: [activation-flow](../05-context-retrieval/activation-flow.md), [readout-scoring](../04-cognitive-dynamics/readout-scoring.md)

## Context

When several paths converge on one site, retrieval must decide whether to sum their contributions or keep only the strongest path. The stored quantities already determine the answer: odds-form Bayes adds evidence.

## Decision

Use additive Random-Walk-with-Restart / Personalized PageRank:

```text
a_next = alpha * seed(Q) + (1 - alpha) * transpose(P) * a
```

Multiple incoming paths are summed. Do not use max-path as the core activation operation.

`Contradicts` edges are excluded from propagation and routed to frustration. Contradiction is not evidence to add or subtract in activation flow.

`alpha` is derived from the single associative-reach prior `L`:

```text
alpha = 1 / (L + 1)
```

Equivalently, if influence should decay to factor `f` after `h_half` hops, `alpha = 1 - f^(1/h_half)` expresses the same single reach degree of freedom. `L` is the only free prior; see [ADR-0010](0010-calibrated-priors-not-laws.md).

## Consequences

Benefits:

- Preserves cumulative evidence from weak and indirect paths.
- Converges because the operator contraction modulus is `(1 - alpha) < 1`.
- Fan effect comes from row normalization.
- Per-hop attenuation comes from restart.

Tradeoffs:

- Requires iterative computation.
- Absolute activation values depend on graph size.
- Large graphs may need sparse approximations.
- Reach remains a calibrated prior.

## Alternatives Considered

### Max-path only

Rejected. It loses repeated weak evidence and violates the additive log-LR meaning.

### Naive additive propagation without restart

Rejected. It may diverge and has no bounded reach.

### Add or subtract contradiction activation

Rejected. That moves truth judgment into propagation and can hide conflicts.
