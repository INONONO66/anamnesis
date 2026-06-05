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
RWR alpha: alpha = 1 - f^(1/h_half)
readout temperature: fit from target entropy or accepted context labels
decay exponent d: fit from re-access hazard or ACT-R prior
```

The docs must not present these values as universal laws. Refit them when graph topology, agent behavior, or embedding geometry changes.

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
