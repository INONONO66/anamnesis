# 0010. Constants Are Calibrated Priors, Not Laws

- Status: Accepted
- Date: 2026-06-05
- Related: [overview](../04-cognitive-dynamics/overview.md), [benchmarks](../07-quality-gates/benchmarks.md)

## Context

The conductive frame borrows physical vocabulary. That can clarify the system, but it can also create false authority if constants are treated like physical laws.

## Decision

Every constant must be one of:

1. **Derived** from a behavioral or mathematical specification.
2. **Fitted** from observed agent data.
3. **Declared** as a calibrated prior until data exists.

Examples:

```text
conductance feature weights: regression coefficients over NPMI features
Hebbian eta: eta = 1 - target^(1/N)
RWR alpha: alpha = 1 / (L + 1)
readout temperature: fit from target entropy or accepted context labels
decay exponent d: fit from re-access hazard or ACT-R prior
```

The docs must not present these values as universal laws. Refit them when graph topology, agent behavior, or embedding geometry changes.

### Irreducible Priors

The free-prior set is minimal. These are the only declared behavioral priors; everything else derives from them.

Free behavioral priors:

- `d` — decay exponent (power-law base-level aging).
- `L` — mean associative reach (hops influence travels before negligible).
- `N` — target co-activation count to reach the conductance saturation target.
- `k` — surprise-to-charge gain (Bayesian surprise into initial retained-action charge).
- `beta_coupling` — cold-start pairwise coupling regression vector over NPMI features.
- `beta_phi` — unary potential-bias feature-weight vector (`beta_prior` fixed `= 1` by design, for odds additivity).
- `w_readout` — readout re-ranking coefficient object.
- `edge_type_factor` — ordinal per-type within-row conductance prior.

Declared density/temperature knobs: `tau` (seed softmax temperature), `conductance_threshold` (cold-start edge density), `eta_leak` (idle-edge leak rate), `b` (activation-budget threshold, `= 0` by convention).

Numerical guards (not behavioral): `LOGIT_BACKFILL_EPS`, `C_MAX`, `rwr_tolerance`.

Everything else derives:

- `alpha = 1 / (L + 1)` from `L`.
- every `eta = 1 - 0.5^(1/N)` from `N`.
- surprise charge `dA_i = k * eps` from `k`.
- projection salience/weight `= logistic` of the reservoirs.
- `theta_sep = 1 - q95(distinct-pair similarity)` from the encoder.
- `coupling_seed` from `beta_coupling`.
- `phi_i` / `seed_i` from `beta_phi`.
- `readout_score` from `w_readout`.

## Physics-Borrowing Guardrails

| Guardrail | Rule |
|---|---|
| J1 | The metaphor must identify a real variable or invariant |
| J2 | The formula must justify a state change or bound |
| J3 | Limitations must be explicit where the physics analogy breaks |

Examples:

- Conductance is acceptable because it maps to associative log-LR and transition normalization.
- Energy is acceptable only with the symmetric-coupling caveat.
- Gravity/mass force is rejected because it does not justify retrieval deltas.

## Consequences

Benefits:

- Prevents arbitrary magic numbers from hiding under physical language.
- Makes calibration and benchmarking part of the design.
- Keeps the spec falsifiable.

Tradeoffs:

- More documentation burden.
- Some defaults remain priors until data exists.
- Consumers may need to refit values for their own graphs.

## Alternatives Considered

### Treat defaults as universal constants

Rejected. Agent memory statistics are not physical constants.

### Avoid all constants

Rejected. Ranking, decay, and convergence require numeric choices.

### Let every consumer freely tune everything

Rejected. Without unit discipline, behavior becomes unexplainable.
