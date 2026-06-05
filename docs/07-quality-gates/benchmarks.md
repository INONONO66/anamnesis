# Benchmark Design

Benchmarks verify that implementations satisfy the performance budget implied by this SSOT. Measurements must be reproducible and must state graph size and query shape.

## Measurement Targets

| Target | Input Size | Metrics |
|---|---|---|
| ingest | node count, candidate count | observations per second, edge proposals |
| conductance | candidate count | coupling proposals per second |
| tick | node count | nodes scanned per second |
| search | seed count, graph degree | p50, p95 latency |
| activation flow | edge count | iterations, residual, current-map time |
| packaging | token budget | selected sites, truncation |
| snapshot | graph size | clone time and memory |
| storage | CRUD batch | operations per second |

## Fixture Graphs

| Size | Nodes | Edges | Purpose |
|---|---:|---:|---|
| small | 1k | 5k | local development |
| medium | 100k | 1M | expected serious project memory |
| large | 1M+ | 10M+ | scalability exploration |

Fixtures should include:

- identity sites,
- semantic and procedural knowledge,
- episodic fragments,
- entity hubs,
- contradiction pairs,
- scoped private and universal knowledge,
- stale and recently accessed sites.

## Search Scenario

Each benchmark query should declare:

- text cue,
- optional embedding,
- scope,
- temporal filter,
- expected bucket mix,
- expected tension behavior,
- token budget.

## Performance Budgets

| Operation | Small | Medium |
|---|---:|---:|
| ingest | interactive | bounded by top-k candidate scan |
| associative query | sub-second target | p95 budget required |
| tick full scan | seconds or less | batch or incremental path required |
| snapshot | interactive | background recommended |

Exact thresholds are project-calibrated. The benchmark must make regressions visible.

## Quality Counters

Fast retrieval is not enough. Benchmarks also store:

| Counter | Meaning |
|---|---|
| selected_identity | identity bucket count |
| selected_knowledge | knowledge bucket count |
| selected_memory | memory bucket count |
| tension_count | returned tension count |
| budget_used | token budget utilization |
| truncation_count | resolution downgrade count |
| residual | activation-flow convergence residual |

## Regression Judgment

- Latency is judged by p95 for each fixture.
- Output quality is judged by bucket shape and tension presence for golden queries.
- Failures are categorized as performance-budget failures or context-shape changes.
- Large-graph optimization must not change the public API.

## Related Documents

- Observability reports are defined in [observability.md](observability.md).
- Query flow is defined in [pipeline.md](../05-context-retrieval/pipeline.md).
- Storage cost is defined in [storage.md](../03-persistence/storage.md).
