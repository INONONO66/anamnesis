# Search End-to-End Latency Baseline

This document records an **informational** latency baseline for
`Engine::search()` measured with `benches/eval/search_latency.rs`.

> **No CI regression gate.** Numbers here are a reference point for
> humans, not a fail-on-regression threshold. Future commits may move
> these numbers up or down without a build break. If you want a hard
> regression check, that is explicitly out of scope per the plan.

## Run metadata

| Field          | Value                                       |
|----------------|---------------------------------------------|
| Date (UTC)     | 2026-05-01                                  |
| Git commit     | `60630be` (`60630be915af192143fdb0ba41412b7e3ef0ed32`) |
| Branch         | `main`                                      |
| Crate version  | `anamnesis 0.1.0`                           |
| Rust toolchain | `rustc 1.92.0 (ded5c06cf 2025-12-08)`       |
| Profile        | `bench` (`opt-level = 3`, `debug = true`)   |
| Host           | Darwin 25.2.0 / arm64 / macOS 26.2          |

## Fixture shape

| Field                | Value                                                                |
|----------------------|----------------------------------------------------------------------|
| Storage backend      | `InMemoryStorage` (default)                                          |
| `num_nodes`          | 100,000                                                              |
| Scopes               | `dev/rust` (40,000), `travel/japan` (30,000), `research/llm` (30,000) |
| Knowledge types      | `Semantic`, `Entity`, `Semantic` (one per scope, in order)           |
| Embeddings           | None (skipped intentionally, see below)                              |
| Edges                | None (search-only fixture; spreading runs on text/entity seeds)      |
| Engine config        | `novelty_threshold = 0.0`, `confidence_threshold = 0.0`, `dedup_enabled = false`, `max_nodes = 200_000` |
| Build time           | ~0.05 s on the recorded host                                         |

Why no embeddings: `Engine::ingest()` runs attraction on every observation
that ships with an embedding. Attraction's similarity scan is O(N) per
ingest, so feeding 100,000 embedded observations would make the build
itself O(N²) and dominate runtime. The latency we want to baseline is
`Engine::search()` end-to-end, not embedding-driven attraction during
ingest, so the fixture intentionally omits embeddings. Vector candidate
collection is therefore inactive in this baseline; text, entity, scope,
and graph recall paths are exercised.

## Search input shape

```rust
SearchInput {
    text: "rust ownership pattern".to_string(),
    scope: ScopePath::new("dev/rust").unwrap(),
    entity_tags: vec!["rust".to_string()],
    limit: 10,
    seed_limit: Some(5),
    ..Default::default() // agent_id = None, query_embedding = None,
                          // now = Timestamp(0), context = None
}
```

This input activates four pipeline stages: text candidate collection (via
`InMemoryStorage::text_search` over 100k nodes), entity-tag candidate
collection (`nodes_by_entity_tag("rust")` returns 40k candidates), RRF
fusion of the two source lists, and graph recall starting from five
fused seeds. Vector candidate collection is skipped because no embedding
is supplied.

## Measured latency

The bench performs **20 warmup iterations** followed by **100 timed
samples** of `engine.search(input.clone())`. Samples are sorted in
ascending order; percentiles use the convention `idx = floor(p * n)`
capped at `n − 1`.

| Statistic       | Value      |
|-----------------|------------|
| min             | 71.900 ms  |
| **P50** (median)| **79.038 ms** |
| **P95**         | **80.895 ms** |
| **P99**         | **81.583 ms** |
| max             | 81.583 ms  |

### Criterion-supported percentile mapping

Criterion does not directly emit P50/P95/P99 — its standard report is
mean ± standard deviation, with a 95 % bootstrap confidence interval on
the slope estimator. For completeness, the same run also produces a
Criterion bench (`search_end_to_end_100k`) whose 95 % CI was:

```
search_end_to_end_100k  time:   [77.211 ms 77.979 ms 78.533 ms]
                                  ^lower    ^median   ^upper
```

Mapping conventions used in this baseline:

- **P50** is reported from the direct timing loop's 100 sorted samples
  (`samples[50]`). This is the closest available observed value to
  Criterion's median estimate.
- **P95** and **P99** are reported from the same direct timing loop
  (`samples[95]` and `samples[99]`). Criterion's 95 % CI upper bound
  (78.533 ms above) is *not* the same as P95 — it is the upper bound on
  the mean estimator at 95 % confidence — so the percentile values from
  the direct loop are reported instead.
- The Criterion CI bounds and the direct-loop percentiles are both
  shown in this document so a reader can correlate them, but only the
  direct-loop percentiles are normative for "P50/P95/P99 of search
  latency".

Both measurements share a single fixture instance to avoid building
100k nodes twice.

## How to reproduce

```bash
cargo bench --bench search_latency
```

The bench prints the percentile block above to stderr and runs the
Criterion benchmark afterward. Criterion's HTML report lands in
`target/criterion/search_end_to_end_100k/`.

## Notes on what is *not* measured

- **No vector similarity load.** Embeddings are intentionally absent
  from the fixture; this baseline is a text + entity + graph-recall
  number, not a pure-vector or hybrid number.
- **No edges in the fixture.** The spread step still runs over fused
  seeds, but with no inter-node edges in the fixture the activation
  cone is empty after the seed step. Adding edges would change the
  cost profile of `run_graph_recalls()` and is left to a future
  benchmark variant.
- **Cold caches not isolated.** The 20-iteration warmup is enough to
  prime allocator and CPU branch caches; no attempt is made to clear
  them between samples or between `search()` invocations.
- **No throughput report.** Per the plan this is a latency baseline; if
  a throughput baseline is wanted later it should live alongside this
  file as `throughput_baseline.md` rather than blending the two.
