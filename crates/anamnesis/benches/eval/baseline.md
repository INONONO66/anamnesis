# Search Quality & Latency Baseline

This document records the re-derived quality floors and latency baseline for
`Engine::search()` on the **Bayesian conductive-network model** (the
ACT-R → conductive migration). The numbers here are re-recorded on the new model
— they are not carried over from the old force-directed physics, and the golden
floors are **re-derived, not loosened**.

Two automated regression judges enforce these floors as ordinary `cargo test`
gates (benchmarks.md "Regression Judgment"):

- `judge_latency_regression` — latency is judged by **p95 for each fixture**.
- `judge_quality_regression` — output quality is judged by **bucket shape and
  tension presence** for golden queries, plus aggregate `precision@5` /
  `recall@10`.

Both run over the deterministic composed multi-tier fixtures in
`benches/eval/composed_fixtures.rs`.

```bash
cargo test --test eval_regression -- --nocapture   # the two regression judges
cargo test --test eval_golden    -- --nocapture     # the Tier-B golden floors
cargo bench --bench search_latency                  # informational 100k-node latency
```

## Run metadata

| Field          | Value                                       |
|----------------|---------------------------------------------|
| Date (UTC)     | 2026-06-06                                  |
| Branch         | `impl/act-r-memory-migration`               |
| Model          | Bayesian conductive-network (additive RWR + readout + frustration + energy) |
| Crate version  | `anamnesis 0.4.0`                           |
| Profile        | `test` (`opt-level = 0`, `debug = true`)    |
| Host           | Darwin 25.5.0 / arm64                        |

## Composed multi-tier fixtures

`build_composed_tiers()` builds three deterministic, self-contained tiers that
share one golden core and scale only the count of filler nodes. The golden core
exercises the full content variety benchmarks.md requires:

- identity sites (an agent persona, peer `7`),
- semantic / procedural / decision / convention / gotcha knowledge,
- episodic and event memory fragments (provenance via `ExtractedFrom`),
- an entity hub (`auth`) fanning to its members,
- a contradiction pair (`Contradicts`, both endpoints co-valid so the tension
  surfaces and neither side is suppressed — ADR-0006),
- scoped private (`client/acme`) and universal knowledge,
- stale (untouched, ~90 days old) and recently accessed (`touch`ed) sites.

| Tier   | Filler nodes | Total nodes (≈) |
|--------|-------------:|----------------:|
| small  | 0            | 23              |
| medium | 400          | 423             |
| large  | 2,000        | 2,023           |

Determinism: no embeddings, no randomness, fixed ingest order and fixed
timestamps, so `NodeId` allocation is stable across runs and machines. The
`judge_quality_regression_is_deterministic` test asserts identical golden
top-10 ordering across two independent builds.

## Re-derived quality floors

Measured on the `small` tier (filler never collides with golden keywords, so
adding filler does not change the golden outcome — the shape assertions are
tier-stable). Aggregated over the golden quality cases:

| Metric            | Observed | Floor (re-derived) |
|-------------------|---------:|-------------------:|
| precision@5       | 0.80     | **0.55**           |
| recall@10         | 1.00     | **0.80**           |

Per-case bucket shape & tension behavior (all asserted):

| Case                  | Query     | rel | P@5  | R@10 | knowledge | memory | tension |
|-----------------------|-----------|----:|-----:|-----:|:---------:|:------:|:-------:|
| caching.cluster       | `caching` |  5  | 1.00 | 1.00 | yes       | no     | no      |
| auth.cluster          | `auth`    |  5  | 1.00 | 1.00 | yes       | no     | no      |
| logging.contradiction | `logging` |  2  | 0.40 | 1.00 | yes       | yes    | yes     |

The `logging.contradiction` case is the cross-cutting shape check: the surfaced
tension selects `KnowledgeWithProvenance` packaging, which pulls the
`ExtractedFrom` episodic memory into the memory bucket — so a single case proves
both tension presence (frustration.md / ADR-0006) and the memory bucket shape.

The floors sit conservatively below the observed values: a legitimate ranking
shuffle that preserves cluster recall still passes, while a real regression (a
cluster dropping out of the top-k, or a tension being silently suppressed) trips
the gate. Failures are categorized as **context-shape changes**.

## Re-recorded per-fixture p95 latency

`judge_latency_regression` warms up `5` iterations then collects `40` timed
`Engine::search("caching")` samples per tier; p95 uses `idx = floor(0.95*n)`
capped at `n-1`. The floors carry generous headroom over the observed p95 so
machine-to-machine variance does not produce false performance-budget failures,
while an order-of-magnitude blow-up still trips the gate.

| Tier   | Observed p95 | Floor (re-recorded) |
|--------|-------------:|--------------------:|
| small  | ~2.5 ms      | **50 ms**           |
| medium | ~26 ms       | **120 ms**          |
| large  | ~115 ms      | **400 ms**          |

Failures are categorized as **performance-budget failures**. (Observed numbers
are on the unoptimized `test` profile; a release/bench build is substantially
faster — the floors gate gross regressions, not micro-jitter.)

## Informational 100k-node latency (`search_latency` bench)

`benches/eval/search_latency.rs` remains an **informational** end-to-end latency
baseline over a 100,000-node fixture (no automated gate). It performs 20 warmup
iterations then 100 timed samples of `engine.search(input.clone())`; percentiles
use `idx = floor(p * n)` capped at `n − 1`.

```rust
SearchInput {
    text: "rust ownership pattern".to_string(),
    scope: ScopePath::new("dev/rust").unwrap(),
    entity_tags: vec!["rust".to_string()],
    limit: 10,
    seed_limit: Some(5),
    ..Default::default()
}
```

This input activates four pipeline stages: text candidate collection, entity-tag
candidate collection, RRF fusion of the two source lists, and additive-RWR graph
recall starting from five fused seeds. Vector candidate collection is skipped
because no embedding is supplied.

```bash
cargo bench --bench search_latency
```

The bench prints a P50/P95/P99 block to stderr and runs a Criterion benchmark
(`search_end_to_end_100k`) afterward; Criterion's HTML report lands in
`target/criterion/search_end_to_end_100k/`.

### Notes on what is *not* measured

- **No vector similarity load.** Embeddings are intentionally absent from the
  fixtures; these are text + entity + graph-recall numbers, not pure-vector or
  hybrid numbers.
- **Cold caches not isolated.** Warmup primes allocator and CPU branch caches;
  no attempt is made to clear them between samples.
- **No throughput report.** Per the plan these are latency baselines; a
  throughput baseline, if wanted later, should live alongside this file rather
  than blending the two.
