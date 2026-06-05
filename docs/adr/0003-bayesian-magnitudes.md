# 0003. Magnitudes Come From Bayes

- Status: Accepted
- Date: 2026-06-05
- Related: [overview](../04-cognitive-dynamics/overview.md), [conductance](../04-cognitive-dynamics/conductance.md)

## Context

Cognitive memory systems often introduce constants and weights because they feel plausible. Anamnesis needs a discipline for deciding what each magnitude means and how it can change.

## Decision

Use odds-form Bayes as the unit discipline:

```text
retained action A_i = log prior need-odds
conductance C_ij    = log likelihood ratio
posterior           = prior + sum evidence
```

Every persistent delta must be traceable to one of:

- Bayesian surprise / prediction error,
- ACT-R base-level activation,
- associative log likelihood ratio,
- bounded Hebbian co-activation,
- calibrated prior fitted from observed behavior.

The engine must identify whether a constant is derived or calibrated. It must not present calibrated priors as physical laws.

## Examples

| Quantity | Source |
|---|---|
| initial retained action | precision-weighted surprise |
| route reinforcement | Rescorla-Wagner prediction error |
| conductance prior | feature PMI / calibrated regression |
| conductance update | co-activation flux with Oja bound |
| restart rate | associative reach |
| decay exponent | re-access hazard / ACT-R prior |

## Consequences

Benefits:

- Fewer arbitrary knobs.
- Stronger traceability for why state changed.
- Better calibration story as usage data accumulates.

Tradeoffs:

- Some values require fitting or explicit prior declarations.
- Implementers must document units and calibration sources.
- Simple-looking API calls may map to richer internal events.

## Alternatives Considered

### Hand-tuned weights

Rejected. Useful for prototypes, but not a defensible spec.

### Pure symbolic rules

Rejected. They avoid numeric arbitrariness but cannot rank, decay, or converge.

### Let storage values be user-defined scores

Rejected. It would make behavior unpredictable across consumers.
