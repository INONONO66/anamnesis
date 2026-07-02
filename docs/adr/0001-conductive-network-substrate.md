# 0001. Adopt Spreading Activation As The Theory And Conductive Network As The Representation

*Superseded in part by [ADR-0014](0014-shrink-to-product.md) (v0.10.0 shrink): the "peer trust" contribution to the evidence prior `P_i` referenced below is no longer a live signal — the peer/trust subsystem was removed and the readout trust term is a neutral `1.0`. The `B_i + P_i` decomposition itself stands.*

- Status: Accepted
- Date: 2026-06-05
- Related: [overview](../04-cognitive-dynamics/overview.md), [activation-flow](../05-context-retrieval/activation-flow.md)

## Context

Anamnesis needs one coherent substrate for explaining how a memory graph is read and how actual use changes it. Physics-flavored terms are useful only if they justify state changes rather than merely renaming behavior.

The tempting alternative is force-directed graph language: gravity, springs, and repulsion. That vocabulary is good for visual layout, but it does not naturally provide conserved mass, metric distance, inverse-square laws, or meaningful equilibrium for retrieval.

## Decision

Use **spreading activation (ACT-R)** as the theoretical substrate. Express the activation process as a **conductive network**: query fields drive current through conductance, multiple paths add evidence, and committed use changes the reservoirs.

The persistent quantities are:

- **retained action `A_i = B_i + P_i`**: log prior need-odds, a composite of two terms. Only `B_i` is the ACT-R base-level activation, `B_i = ln( Σ_j (now − t_j)^(−d_j) )` over the node's access traces (each trace carries its own activation-dependent decay rate `d_j`); it owns forgetting and use-driven reinforcement. `P_i` is a separate, decay-exempt evidence prior (encoding surprise, feedback / social reinforcement, peer trust).
- **conductance `C_ij`**: associative strength, log likelihood ratio.

The query-local quantity is:

- **activation `a_i`**: transient response, never persisted.

Core axiom:

```text
total_i = A_i + sum_j W_j * S_ji
        = (B_i + P_i) + sum_j W_j * S_ji
        = log prior odds + sum log likelihood ratios
        = log posterior odds
```

Activation flow settles through additive RWR:

```text
a_next = alpha * seed(Q) + (1 - alpha) * transpose(P) * a
```

Force-directed gravity/spring/repulsion language is rejected as retrieval substrate. Importance emerges from topology, conductance, and access history.

## Consequences

Benefits:

- Flow and stored landscape use one vocabulary.
- Importance is emergent rather than hand-assigned mass.
- Fan effect, per-hop attenuation, and convergence fall out of RWR normalization and restart.
- Predictions remain falsifiable: power-law forgetting and the spacing effect (reproduced by the activation-dependent multi-trace base-level sum), the fan effect, and RWR fixed-point convergence. (The testing effect is not claimed; see [ADR-0008](0008-powerlaw-dissipation.md).)

Tradeoffs:

- The ontology is richer: potential, conductance, current, dissipation, and frustration must remain consistent.
- Energy language must be used carefully because directed RWR is driven-dissipative.
- Fast query-time flow and slow between-query forgetting must stay separate.
- Constants are calibrated priors, not laws.

## Alternatives Considered

### Force-directed retrieval vocabulary

Rejected. It describes graph layout, not memory retrieval. It cannot justify cognitive state deltas without arbitrary mass and distance assumptions.

### Pure ACT-R numbers without conductive vocabulary

Rejected. It gives correct magnitudes but weaker intuition for flow, impedance, and frustration. The conductive frame preserves ACT-R meaning while adding a coherent representation.
