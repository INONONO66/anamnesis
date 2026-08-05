# Evidence Feedback And Provenance

Anamnesis preserves producer provenance and can integrate explicit consumer
feedback into the evidence prior. It does not infer peer trust, authenticate
producers, or decide truth by majority.

## Origin

Every source carries `peer_id`, `source_kind`, `session_id`, `scope`, and
`confidence`. These fields explain where a memory came from and participate in
admission, visibility, and audit. `PeerId` is an opaque provenance id and does
not contribute a producer-reputation score.

## Feedback-based work

Feedback is a committed interaction about returned memory evidence. It updates
the decay-exempt evidence prior `P_i` through bounded prediction error:

```text
delta P_i = eta * (lambda - predicted_i)
```

Positive feedback can raise `P_i`; negative feedback can lower it. The update
is separate from access-based reinforcement. Confirmed use appends an access
trace to the base-level term `B_i`; read-only retrieval changes neither term.
Neither operation updates a producer profile or treats agreement between
producers as corroboration.

The spacing effect comes from the access-trace model, not feedback. Each new
trace receives an activation-dependent decay rate, so spaced presentations are
more durable than massed presentations under the documented retention
conditions. See [dissipation.md](dissipation.md).

## Learning rate

The prediction-error learning rate is derived from the declared target
co-activation count:

```text
eta = 1 - 0.5^(1 / N)
```

`N` and target `lambda` are calibrated policy under
[ADR-0010](../adr/0010-calibrated-priors-not-laws.md). Feedback never writes a
bounded salience projection directly.

## Contradiction and provenance

Feedback may change how strongly evidence is recalled, but it does not erase an
origin or resolve a contradiction. `Contradicts` preserves both endpoints and
their provenance; acceptance remains an external consumer decision. Derived
claims and observations proposed by
[ADR-0015](../adr/0015-evidence-grounded-formation-and-chain-retrieval.md)
must cite raw evidence regardless of feedback strength.

## Safety rules

- Scope visibility overrides confidence and feedback.
- Feedback updates are bounded, explicit, and traceable.
- Retrieval without commit is mutation-free.
- Producer identity does not imply trust or authorization.
- Contradiction remains visible until an explicit source-backed update or
  supersession changes eligibility.
- Agreement among producer ids is not a promotion or ranking rule.

## Failure conditions

- Private scope leakage through a derived record.
- Feedback applied without a matching commit trace.
- Direct salience editing in place of an evidence-prior or access-trace update.
- Origin removal during consolidation, supersession, or rendering.
- Treating producer identity or confidence as a truth verdict.

## Related documents

- Origin fields are defined in
  [peer-identity.md](../02-knowledge-model/peer-identity.md).
- Interaction boundaries are defined in [interactions.md](interactions.md).
- Readout integration is defined in [readout-scoring.md](readout-scoring.md).
- Contradiction behavior is defined in [frustration.md](frustration.md).
