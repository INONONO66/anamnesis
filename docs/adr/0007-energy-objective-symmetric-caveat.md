# 0007. Energy Is An Objective; Strict Minimization Requires Symmetry

- Status: Accepted
- Date: 2026-06-05
- Related: [energy](../04-cognitive-dynamics/energy.md), [activation-flow](../05-context-retrieval/activation-flow.md)

## Context

Energy language is useful for explaining stable readout, but it is easy to overclaim. Directed activation flow is not a conservative physical system.

## Decision

Use energy as an interpretive readout objective:

```text
E(S | Q) =
    - field_alignment
    - conductive_support
    + impedance_regularization
    + frustration_penalty
```

Under symmetric conductance, the Dirichlet form is a true Lyapunov energy:

```text
a^T L a = 1/2 * sum_{i,j} C_ij * (a_i - a_j)^2
```

Under directed RWR, the true fixed point is:

```text
a = alpha * seed(Q) + (1 - alpha) * P^T * a
```

Energy then explains and ranks around the stationary vector; it is not a conserved Hamiltonian.

## Consequences

Benefits:

- Gives a scalar explanation for readout bundles.
- Keeps contradiction stress in the same objective.
- Avoids false conservation claims.

Tradeoffs:

- Documentation must distinguish symmetric and directed cases.
- Implementations should report RWR residual, not only energy value.
- Offline analysis can use exact resistance/Dirichlet tools; hot path should use approximations.

## Alternatives Considered

### Claim global energy minimization for all graphs

Rejected. Directed RWR breaks the required symmetry.

### Avoid energy language entirely

Rejected. It loses a useful explanation surface for readout and frustration.

### Force all edges to be symmetric

Rejected. Cue-target association is naturally directed and needs fan effect.
