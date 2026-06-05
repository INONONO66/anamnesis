# Potential Landscape

The potential landscape describes how a query field biases the graph before activation flow starts. It does not store a new state. It builds the restart seed distribution for RWR and explains why some basins are easier to enter.

## Intuition

A query acts like a battery: it imposes a semantic potential difference over memory sites. Sites aligned with the query receive higher initial potential. Conductance then determines where activation flows.

```text
query field Q -> potential bias phi_i -> seed distribution e_i -> activation flow
```

## Bias Inputs

| Input | Meaning |
|---|---|
| text match | Lexical cue from query text |
| embedding similarity | Semantic alignment |
| explicit seed | Caller-provided node id |
| entity overlap | Shared entity tags |
| scope relation | Visibility and relevance |
| retained action | Prior need-odds |
| identity prior | Active agent identity context |

## Potential Bias

The seed score is a log-linear combination over calibrated features:

```text
phi_i =
    beta_text     * text_score_i
  + beta_embed    * embedding_score_i
  + beta_entity   * entity_overlap_i
  + beta_scope    * scope_weight_i
  + beta_prior    * A_i
  + beta_identity * identity_bias_i
```

Then normalize to an RWR restart distribution:

```text
seed_i = softmax(phi_i / tau)
```

`beta` and `tau` are calibrated priors. They can be fit from accepted readout data.

## Uses

- Build initial candidates for `search`.
- Convert multiple cues into one query field.
- Bias activation toward scoped and identity-relevant memories.
- Produce trace explanations for why a site received current.

## Boundary

Potential bias is query-local. It does not mutate retained action or conductance. A site with high potential can still fail readout if conductance and impedance do not support it. A low-potential site can still light up through strong graph paths.

## Cost

Potential construction is linear in the candidate set. Candidate generation should narrow the set before expensive embedding or entity scoring.

## Related Documents

- Activation flow is defined in [activation-flow.md](../05-context-retrieval/activation-flow.md).
- Search pipeline is defined in [pipeline.md](../05-context-retrieval/pipeline.md).
- Readout scoring is defined in [readout-scoring.md](readout-scoring.md).
