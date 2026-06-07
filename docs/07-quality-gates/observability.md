# Observability

Observability explains why the engine returned a context, whether the graph is healthy, and whether performance budgets were exceeded. The core library does not embed a log server; it returns structured reports.

## Observation Surfaces

| Surface | Purpose |
|---|---|
| search trace | Candidate collection, field construction, activation flow, readout |
| tick report | Dissipation volume and projection deltas |
| reflect report | Cross-agent entity linking results |
| commit trace | Work integrated into reservoirs |
| snapshot list | Experiment and checkpoint state |

## Search Trace

Trace should include:

- input summary,
- candidate source counts,
- seed distribution,
- RWR iterations and residual,
- excluded contradiction edges,
- readout score components,
- budget use and truncation,
- selected tensions,
- scope and trust gates.

Trace may expose more internal scores than the final context body. It is for debugging and evaluation.

## Graph Health

| Metric | Meaning |
|---|---|
| node_count | Total sites |
| edge_count | Total edges |
| orphan_ratio | Fraction of disconnected sites |
| contradiction_ratio | Fraction of tension edges |
| salience_entropy | Diversity of salience projections |
| conductance_entropy | Diversity of conductance projections |
| average_degree | Mean graph degree |
| scope_distribution | Site count by scope |
| stale_ratio | Sites not accessed recently |

Definitions, so the metrics are computed identically across environments:

- **disconnected site (orphan).** A site with structural degree `0` — no incoming or outgoing retained edge (`degree = in-edges + out-edges`). `orphan_ratio = disconnected_sites / node_count`, defined as `0` for an empty graph (`node_count = 0`). A too-strict conductance threshold suppresses edge creation (it gates `coupling_seed >= threshold` at link time), which is what the Operational Warnings table attributes to a high orphan ratio.
- **entropy metrics.** Shannon entropy `H = -sum_k p_k * log2(p_k)` over the projection histogram — projections binned into fixed buckets and normalized to sum to 1 (`salience_entropy` over `salience s_i`, `conductance_entropy` over the bounded `edge_weight = project_weight(C_ij)`), reported in bits. Low entropy means the projections have collapsed onto a few buckets.
- **degree.** The graph is directed (activation flow normalizes outgoing edges; see [activation-flow.md](../05-context-retrieval/activation-flow.md)), so `average_degree` is the mean total degree (in-edges + out-edges) per site.
- **stale site.** A site whose `now - accessed_at` exceeds the configured stale window. `accessed_at` advances only on committed access; query-only retrieval leaves it unchanged (see [dissipation.md](../04-cognitive-dynamics/dissipation.md)). The conductance threshold and stale window are operational [EngineConfig](../01-system-architecture/overview.md#engineconfig) knobs, not calibrated priors.

## Invariant Checks

The engine should expose checks for:

- public projections within closed bounds (salience `s_i` and edge weight `w_ij` in `[0, 1]`, including clamped boundary values `0` and `1`),
- adjacency consistency,
- missing origins,
- invalid validity intervals,
- dangling edges,
- inaccessible private-scope leakage,
- non-finite hot fields,
- snapshot/restore consistency.

## Operational Warnings

| Warning | Likely Cause | Action |
|---|---|---|
| high orphan ratio | Conductance threshold too strict | Recalibrate threshold or candidate generation |
| high contradiction ratio | Over-linking entities or stale facts | Review tension handling |
| low entropy | Salience projections collapsed | Inspect dissipation and reinforcement |
| dense graph | Excess edge proposal | Apply edge budget / leakage |
| stale core | Important identity not accessed | Inspect packaging policy |

## Related Documents

- Performance measurement is defined in [benchmarks.md](benchmarks.md).
- Readout trace is defined in [readout-scoring.md](../04-cognitive-dynamics/readout-scoring.md).
- Storage scan cost is defined in [storage.md](../03-persistence/storage.md).
