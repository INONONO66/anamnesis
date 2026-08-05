# Readout Scoring

Readout scoring chooses which activated sites become context. Activation flow returns a transient response over the graph; readout turns that response into ranked, budgeted output.

This document is the authoritative definition of the readout score. [overview.md](overview.md) defers to the form given here.

The scoring layer must respect the reservoir/projection boundary. It reads retained action, salience, conductance-derived activation, impedance, scope, and stress. It does not directly set any persistent quantity.

## Input Signals

| Signal | Meaning |
|---|---|
| `a_i` | Query-local activation response |
| `phi_i` | Potential bias from the query field |
| `s_i` | Salience projection |
| `A_i` | Retained action (prior need-odds); read input and tie-breaker |
| `Z_i` | Effective impedance |
| `scope_weight_i` | Scope applicability used for ranking; not authorization |
| `trust_weight_i` | Compatibility input; the current engine supplies a neutral value because it has no peer-trust subsystem |
| `stress_i` | Frustration attached to selected contradictions |
| resolution level | L0/L1/L2 content cost |

## Combined Score

```text
readout_score_i =
    w_a     * logit(a_i)
  + w_phi   * phi_i
  + w_s     * logit(s_i)
  - w_z     * Z_i
  + w_scope * scope_weight_i
  + w_trust * trust_weight_i
  - w_stress * stress_i
```

Scores are for ranking within a query. Because RWR values shrink as graph size grows, downstream selection should use top-k or percentiles rather than a fixed absolute activation threshold.

## Coefficients

The seven coefficients are one calibrated re-ranking regression object, not seven independent knobs. The score is additive in log-odds space: it reads as a posterior log-odds, `posterior = prior + sum of evidence`, where each term contributes its evidence to the ranking. The default is unit coefficients (`w_* = 1`), which recovers the plain additive log-odds sum; the regression is then fit from accepted readout data or target entropy per [ADR-0010](../adr/0010-calibrated-priors-not-laws.md). They are calibrated priors, not universal laws.

| Coefficient | Calibration Source |
|---|---|
| `w_a` | retrieval acceptance labels |
| `w_phi` | query-field alignment labels |
| `w_s` | historical usefulness / re-access |
| `w_z` | impedance penalty needed to avoid isolated noise |
| `w_scope` | scope-ranking acceptance labels |
| `w_trust` | reserved compatibility coefficient; neutral in the current engine |
| `w_stress` | consumer tolerance for contradiction bundles |

## Lamp Analogy

Activation is how much current reaches a lamp. Retained action is how ready the lamp is to light. Impedance is how costly it is to reach. Readout selects lamps bright enough and useful enough for the current prompt.

## Bucket Handling

| Bucket | Handling |
|---|---|
| identity | Include stable identity priors and current state when relevant |
| knowledge | Prefer high-score semantic/procedural/decision sites |
| memories | Include episodic fragments when they explain provenance or recent context |
| tensions | Include eligible contradiction bundles when both endpoints were activated independently |

The `ContextPackage` should preserve a balanced shape instead of letting one bucket consume all budget.

`EdgeType::Contradicts` never carries activation. It is excluded from the RWR
transition matrix and evaluated by the frustration channel only after both
endpoints are active through other cues or supportive paths.

## Proposed: Evidence-Aware Selection (ADR-0015)

Under the evidence-complete pipeline proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
the current node score would remain one input rather than the complete selection
objective. The proposed selector would group overlapping representations by
canonical source and add:

- marginal requested-slot coverage,
- typed-relation compatibility,
- temporal and scope consistency,
- provenance completeness,
- source novelty, and
- token cost.

The node score would continue to order relevance within a source-aware candidate
surface. A lower-ranked source could replace a redundant tail representation
when it fills a missing slot or completes a valid evidence chain. It could not
replace the stable direct-query prefix for diversity alone. Routing-only facts
would remain ineligible without their authoritative sources, and a proposed
chain hop with incompatible scope eligibility, time, or provenance would not
affect delivered evidence.

## Ordering Stability

When scores tie, use deterministic tie-breakers:

1. higher retained action,
2. lower impedance,
3. more recent committed access,
4. stable node id.

## Trace

Readout trace should include:

- candidate count,
- selected count,
- score components,
- bucket assignment,
- resolution downgrade,
- token budget use,
- truncation reason,
- tension inclusion.

## Failure Conditions

- Absolute activation thresholds break on large graphs.
- Readout must not mutate storage.
- Scope weights must be finite and applied consistently; the caller, not
  readout scoring, excludes unauthorized data.
- An eligible contradiction between independently active endpoints must not be
  silently hidden.
- Resolution downgrade must preserve provenance.
- A current atomic routing fact must not be rendered without its authoritative
  raw source.

## Cost

Scoring is linear in the activated candidate set. Token budgeting is linear if each site reports estimated token cost.

## Related Documents

- Activation flow is defined in [activation-flow.md](../05-context-retrieval/activation-flow.md).
- Potential bias is defined in [potential-landscape.md](potential-landscape.md).
- Frustration is defined in [frustration.md](frustration.md).
- Packaging is defined in [pipeline.md](../05-context-retrieval/pipeline.md).
