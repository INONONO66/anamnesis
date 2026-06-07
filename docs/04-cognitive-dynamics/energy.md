# Energy

Energy is the scalar objective used to explain and stabilize readout over an active subsystem. It is not a conserved Hamiltonian for the whole engine.

The strict caveat is central: energy minimization is mathematically exact only under symmetric coupling. Directed RWR is driven-dissipative; its true fixed point is the stationary activation vector.

## Role

Energy helps answer:

- why this group of sites was read out,
- why strongly connected active sites cohere,
- why contradictory bundles separate,
- why high-impedance sites are expensive to include.

It is used for readout explanation and ranking, not as a persistent reservoir.

## Objective Shape

```text
E(S | Q) =
    - field_alignment(S, Q)
    - conductive_support(S)
    + impedance_regularization(S)
    + frustration_penalty(S)
```

| Term | Meaning |
|---|---|
| `field_alignment` | How well selected sites align with the query field |
| `conductive_support` | How strongly selected sites support each other through conductance |
| `impedance_regularization` | Cost of lighting isolated or cold sites |
| `frustration_penalty` | Stress from contradictory sites active together |

The four leading `-`/`+` term coefficients are structural descent-direction signs (`+/-1`), not tunable magnitudes. They encode that alignment and conductive support lower energy while impedance and frustration raise it; there are no per-term weights to fit.

The result is query-local and is not stored.

## Two Layers

| Layer | Inputs | Output |
|---|---|---|
| embedding energy | query vector, site embedding | seed distribution and candidate ranking |
| conductive energy | retained action, conductance, stress, impedance | stabilized site response |

Embedding proposes where current should be injected. Conductance determines how that injection settles through the graph.

## Symmetric Coupling: True Lyapunov

For a symmetric subgraph (`C_ij = C_ji`), graph energy has a strict Dirichlet form. The Laplacian is built from the **non-negative flow-side projection** `G_ij = project_conductance(C_ij) = logistic(C_ij)` in `(0, 1)`, not the raw reservoir: `C_ij` is an unbounded log-LR that may be negative, and negative weights would break the positive-semidefiniteness that makes this a valid Lyapunov function. See [conductance.md](conductance.md).

```text
G_ij  = project_conductance(C_ij)
L_sym = D - G
D_ii  = sum_j G_ij
a^T L_sym a = 1/2 * sum_{i,j} G_ij * (a_i - a_j)^2
```

This measures how much the activation pattern violates the conductive structure. In this limited case, "energy descent" and "attractor" language is exact.

## Directed RWR: Driven-Dissipative

Real activation flow is directed:

```text
a = alpha * seed(Q) + (1 - alpha) * P^T * a
P = row_normalized_conductance
```

There is a continuous-time intuition:

```text
da/dt = -L_rw * a + alpha * s(t)
L_rw  = I - (1 - alpha) * P^T
```

Dissipation is not a separate additive term: it is the state-proportional leak carried by the identity part of `-L_rw * a`. The fixed point `da/dt = 0` is exactly the discrete RWR solution `a* = alpha * (I - (1 - alpha) P^T)^{-1} * s`. The day-scale forgetting of node strength is owned by the multi-trace base level `B_i = ln( Σ_j (now − t_j)^(−d_j) )` (older traces age, each at its own per-trace rate; see [dissipation.md](dissipation.md)), not by a scalar `decay(A_i, Δt)` shift; this is a distinct process and must not be folded into this dimensionless activation flow.

But once query injection and dissipation exist, no conservation law applies. Use separate symbols for symmetric coupling Laplacian and directed transition matrix. The energy/Lyapunov reading is exact only under symmetric coupling (`C_ij = C_ji`); under directed RWR the true fixed point is the RWR stationary activation vector `a*`, and energy is an interpretive descent objective rather than a quantity the dynamics literally minimize. The stationary vector `a*` is primary; energy explains and stabilizes readout around it.

## Restart Alpha

`alpha` is derived from the single mean associative reach `L`:

```text
alpha = 1 / (L + 1)
```

Equivalently, if influence should decay to `f` after `h_half` hops, `alpha = 1 - f^(1/h_half)` expresses the same single reach degree of freedom. See [activation-flow.md](../05-context-retrieval/activation-flow.md).

Smaller `alpha` means longer reach and slower convergence. Larger diffusion risks over-smoothing by weakly activating too much of the graph.

## Stop Rules

- Stop when `||a^(t+1) - a^(t)||_1` is below tolerance.
- Stop at max iterations and return `truncated = true`.
- Promote non-finite values to error traces.
- For tiny token budgets, allow one-step readout.

## Impedance `Z_i`

`Z_i` estimates how expensive it is to light site `i`. On the hot path, it is approximated from RWR or heat-kernel response. Exact effective resistance requires a Laplacian pseudoinverse and is offline-only.

Importance remains emergent: low-impedance, well-connected, often-used sites are easier to activate without adding a gravity term.

## Example

For query `"why did search return strange results?"`, activation may light a hypothesis, evidence, and a decision. Evidence and decision support each other through low-impedance paths. A refuted hypothesis creates stress with the decision and is separated into tension. The final trace decomposes why the context bundle was selected.

## Related Documents

- Frustration penalty is defined in [frustration.md](frustration.md).
- Conductance and transition normalization are defined in [conductance.md](conductance.md).
- Retained action and dissipation are defined in [dissipation.md](dissipation.md).
- Query potential is defined in [potential-landscape.md](potential-landscape.md).
- Readout is defined in [readout-scoring.md](readout-scoring.md).
