# Conductance

Conductance `C_ij` measures how easily activation current can flow between two memory sites. Cognitively, it is the associative strength by which cue `j` raises the odds that target `i` is needed now.

```text
C_ij = associative strength S_ji
     = ln(P(i needed | j cued) / P(i needed))
```

An edge is not merely a "related to" marker. Under a query field, it is a conductive path and a likelihood-ratio term in posterior-odds computation. Public `edge weight` is only a bounded projection of `C_ij`.

Core rule: **conductance is never set directly**. It changes only as the integrated result of physical/cognitive interactions.

## Design Goals

| Goal | Description |
|---|---|
| cold-start coupling | Create calibrated log-LR priors from semantic, entity, scope, and type features |
| path-dependent plasticity | Repeated committed flux strengthens used paths |
| bounded projection | Public weight stays in a closed range |
| density control | Not every fragment links to every fragment; fan effect suppresses hubs |
| provenance | Metadata explains why a path exists |

## Inputs

- Newly created memory site.
- Candidate neighboring sites.
- Embedding similarity.
- Entity tag overlap.
- Scope compatibility.
- Edge type affinity.
- Committed retrieval trace containing path current and co-readout pairs.

## Cold Start: Coupling As Calibrated Log-LR Prior

When an observation creates a new site, no co-activation history exists yet. Initial `C_ij` is therefore a calibrated prior estimated from features. Ideally each feature contributes pointwise mutual information:

```text
coupling_seed(i,j) = sum_f beta_f * pmi_f(i,j)

pmi_f(i,j) = ln(P(i,j co-needed | feature f fires) / P(i,j co-needed))
npmi_f     = pmi_f / (-ln P(i,j co-needed))
```

Before enough data exists, use calibrated priors:

```text
coupling_seed =
    0.45 * sim_npmi
  + 0.25 * entity_npmi
  + 0.15 * scope_npmi
  + 0.15 * type_npmi

if coupling_seed >= conductance_threshold:
    create edge with C_ij = initialize_conductance(coupling_seed)
```

These coefficients are not laws. They are initial regression weights over normalized features and should be refit when agent statistics change. Cold-start edges are weak paths that let activation flow for the first time; committed co-activation determines the later magnitude.

## Post-Commit Plasticity: Bounded Hebbian Update

Read-only retrieval never changes conductance. A caller must commit that a result was actually used. Only then do path flux and co-readout pairs leave work behind.

```text
a_i = clamp(a_i(Q) - b, 0, 1)
a_j = clamp(a_j(Q) - b, 0, 1)

path_flux_ij = path_used_ij * I_ij
co_flux_ij   = co_readout_ij * min(a_i, a_j)

C_ij' = C_ij
  + eta_path * path_flux_ij * (1 - C_ij)
  + eta_pair * co_flux_ij  * (1 - C_ij)
  - eta_leak * idle_edge_leakage_ij

edge_weight_ij = project_weight(C_ij')
```

The `(1 - C_ij)` term bounds the update and prevents raw Hebbian runaway. The target remains associative log-LR: as use accumulates, cold-start priors are replaced by measured co-activation strength.

Learning rates are derived from behavioral specifications. If conductance should reach `0.5` after `N` co-activations:

```text
eta = 1 - 0.5^(1/N)
```

## Density Control

| Control | Reason |
|---|---|
| top-k candidates | Bound ingest cost |
| minimum coupling | Avoid noisy paths |
| scope check | Prevent private-memory leakage |
| edge leakage | Remove unused weak coupling over time |
| fan effect | Row normalization reduces each outgoing path from high-fan-out sources |
| Oja bound | Prevent weight explosion |

Importance is emergent. There is no separate gravity or mass boost.

## Output

Conductance updates return affected edge ids and a trace containing candidate counts, created edges, feature contributions, integrated flux, leakage count, rejection reasons, and the calibration source for `eta`.

## Related Documents

- Graph types are defined in [graph-model.md](../02-knowledge-model/graph-model.md).
- Ingest and surprise gating are described in [perception.md](perception.md).
- Read-only vs commit boundaries are defined in [interactions.md](interactions.md).
- Contradiction stress is described in [frustration.md](frustration.md).
- Edge leakage is described in [dissipation.md](dissipation.md).
