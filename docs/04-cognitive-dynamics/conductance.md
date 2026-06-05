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

These coefficients are not laws and not four independent knobs. They are one calibrated regression vector `beta_coupling` over the normalized NPMI features `{sim, entity, scope, type}` (the example above normalizes to sum 1) jointly fit at cold start, and each `beta_f` is replaced by the measured `npmi_f` as co-activation data accrues — so the prior decays toward a fully data-derived seed. Cold-start edges are weak paths that let activation flow for the first time; committed co-activation determines the later magnitude.

## Post-Commit Plasticity: Bounded Hebbian Update

Read-only retrieval never changes conductance. A caller must commit that a result was actually used. Only then do path flux and co-readout pairs leave work behind.

```text
a_i = clamp(a_i(Q) - b, 0, 1)
a_j = clamp(a_j(Q) - b, 0, 1)

path_flux_ij = path_used_ij * I_ij
co_flux_ij   = co_readout_ij * min(a_i, a_j)
```

Path current `I_ij` carries reach from the query field. Reach is parameterized by the single mean associative reach `L` (the restart rate `alpha = 1 / (L + 1)` follows from it); see [activation-flow.md](../05-context-retrieval/activation-flow.md).

```text
C_ij' = C_ij
  + eta * path_flux_ij * (1 - C_ij)
  + eta * co_flux_ij  * (1 - C_ij)
  - eta_leak * idle_edge_leakage_ij

edge_weight_ij = project_weight(C_ij')
```

The `(1 - C_ij)` term is the Oja bound: it bounds the update and prevents raw Hebbian runaway. The target remains associative log-LR: as use accumulates, cold-start priors are replaced by measured co-activation strength.

The learning rate is a single rate `eta` derived from one behavioral specification: the target co-activation count `N` at which conductance should reach the saturation target. The saturation target `0.5` is a fixed Oja/symmetric convention (any value in `(0,1)` works; it is not a free knob), so:

```text
eta = 1 - 0.5^(1/N)
```

Path flux and co-readout flux share this single `eta` at the core. Splitting into `eta_path` and `eta_pair` is an optional data-justified refinement of the same `N` — fit a separate `N_pair > N_path` only once data shows co-readout strengthens more weakly than committed path flux — not two independent base constants.

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
