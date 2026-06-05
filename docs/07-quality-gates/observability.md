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

## Invariant Checks

The engine should expose checks for:

- projection values inside range,
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
