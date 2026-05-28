//! Informational latency baseline for `Engine::search()` end-to-end.
//!
//! Builds a deterministic 100_000-node cognitive graph spanning three
//! scopes (`dev/rust`, `travel/japan`, `research/llm`) and measures the
//! end-to-end cost of `engine.search()` for a representative text query.
//! Results are recorded in `benches/eval/baseline.md` and are
//! **informational only** — there is no automated regression gate on
//! this benchmark.
//!
//! Two independent measurements run in sequence:
//!
//! 1. A direct timing loop that collects 100 raw samples and prints
//!    P50 / P95 / P99 to stderr. These percentiles are computed from
//!    the sorted sample slice and are the values copied into
//!    `baseline.md`.
//! 2. A Criterion benchmark that exposes mean / median / 95 % CI bounds
//!    via the standard `target/criterion/...` reports for anyone who
//!    wants Criterion's full statistical view.
//!
//! Determinism: the fixture is built without random numbers, without
//! embeddings, and with a fixed ingest order. Every run on the same
//! revision allocates `NodeId` values identically.

#![cfg_attr(test, allow(dead_code))]

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::Criterion;

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::SearchInput;
use anamnesis::{Engine, EngineConfig};

/// Total nodes ingested into the latency fixture.
const NUM_NODES: usize = 100_000;
/// Warmup iterations before the timed loop. Helps stabilize allocator
/// caches and CPU branch prediction prior to sample collection.
const WARMUP_ITERATIONS: usize = 20;
/// Number of timed samples used to compute P50 / P95 / P99.
const NUM_SAMPLES: usize = 100;

/// Build the deterministic 100k-node fixture.
///
/// - Three scopes: `dev/rust` (40k), `travel/japan` (30k), `research/llm`
///   (30k). Sizes sum to `NUM_NODES`.
/// - Fixed text seeds per scope so `text_search` has predictable hits.
/// - No embeddings — keeps ingest at O(N) by skipping attraction's
///   per-observation similarity scan.
/// - Perception thresholds relaxed so every observation lands in the
///   graph; dedup disabled for the same reason.
fn build_fixture() -> Engine {
    let config = EngineConfig::new()
        .with_max_nodes(NUM_NODES * 2)
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    let scope_specs: [(&str, usize, &str, &[&str], KnowledgeType); 3] = [
        (
            "dev/rust",
            40_000,
            "rust ownership pattern",
            &["rust", "ownership"],
            KnowledgeType::Semantic,
        ),
        (
            "travel/japan",
            30_000,
            "travel japan city",
            &["japan", "city"],
            KnowledgeType::Entity,
        ),
        (
            "research/llm",
            30_000,
            "llm transformer model",
            &["llm", "transformer"],
            KnowledgeType::Semantic,
        ),
    ];

    let mut ts: u64 = 1_000;
    for (scope_idx, (scope_str, count, content_seed, tags, kt)) in scope_specs.iter().enumerate() {
        let scope = ScopePath::new(*scope_str).expect("valid scope path");
        let entity_tags: Vec<String> = tags.iter().map(|s| (*s).to_string()).collect();
        let session_id = format!("session-{scope_idx}");
        for i in 0..*count {
            let observation = Observation {
                name: format!("{content_seed}-{scope_idx}-{i}"),
                summary: None,
                content: format!("{content_seed} fragment {i}"),
                embedding: None,
                confidence: 0.9,
                node_type: kt.clone(),
                entity_tags: entity_tags.clone(),
                origin: Origin {
                    peer_id: anamnesis::graph::types::PeerId(0),
                    source_kind: anamnesis::peer::SourceKind::AgentObservation,
                    session_id: session_id.clone(),
                    scope: scope.clone(),
                    confidence: 0.9,
                },
                timestamp: Timestamp(ts),
                valid_from: None,
                valid_until: None,
            };
            ts += 1;
            engine.ingest(observation).expect("ingest should succeed");
        }
    }

    engine
}

/// Build the search input used by every measurement and benchmark.
///
/// Targets the `dev/rust` cluster with a multi-token query so all four
/// flag-driven sub-stages of the search pipeline are exercised:
/// text decomposition, scope weighting, entity-tag candidate collection,
/// and graph recall on the fused seeds.
fn make_search_input() -> SearchInput {
    SearchInput {
        text: "rust ownership pattern".to_string(),
        scope: ScopePath::new("dev/rust").expect("valid scope"),
        entity_tags: vec!["rust".to_string()],
        limit: 10,
        seed_limit: Some(5),
        ..Default::default()
    }
}

/// Run a warmup pass plus `NUM_SAMPLES` timed iterations and return the
/// sorted samples. The returned slice is sorted in ascending order so
/// that percentiles can be looked up by index.
fn measure_samples(engine: &Engine) -> Vec<Duration> {
    let input = make_search_input();

    for _ in 0..WARMUP_ITERATIONS {
        let result = engine
            .search(input.clone())
            .expect("warmup search should succeed");
        let _ = black_box(result);
    }

    let mut samples: Vec<Duration> = Vec::with_capacity(NUM_SAMPLES);
    for _ in 0..NUM_SAMPLES {
        let start = Instant::now();
        let result = engine.search(input.clone()).expect("search should succeed");
        let elapsed = start.elapsed();
        let _ = black_box(result);
        samples.push(elapsed);
    }
    samples.sort();
    samples
}

/// Look up a percentile from a sorted sample slice.
///
/// Uses the simple convention `index = floor(p * n)` capped at `n - 1`,
/// which corresponds to the upper sample at each percentile bucket.
/// This is reported as the closest available observed value relative to
/// Criterion's mean/median+CI report and matches what `baseline.md`
/// records.
fn percentile(sorted: &[Duration], p: f64) -> Duration {
    debug_assert!((0.0..=1.0).contains(&p));
    let n = sorted.len();
    if n == 0 {
        return Duration::ZERO;
    }
    let raw = (n as f64 * p).floor() as usize;
    let idx = raw.min(n - 1);
    sorted[idx]
}

/// Pretty-print percentiles to stderr in a copy-paste friendly form for
/// `baseline.md`.
fn print_baseline_summary(samples: &[Duration]) {
    let p50 = percentile(samples, 0.50);
    let p95 = percentile(samples, 0.95);
    let p99 = percentile(samples, 0.99);
    let min = samples.first().copied().unwrap_or(Duration::ZERO);
    let max = samples.last().copied().unwrap_or(Duration::ZERO);

    eprintln!();
    eprintln!("=== Engine::search() end-to-end latency baseline ===");
    eprintln!("  num_nodes:     {NUM_NODES}");
    eprintln!("  warmup:        {WARMUP_ITERATIONS}");
    eprintln!("  samples:       {}", samples.len());
    eprintln!("  min:           {:>8.3} ms", to_ms(min));
    eprintln!("  P50 (median):  {:>8.3} ms", to_ms(p50));
    eprintln!("  P95:           {:>8.3} ms", to_ms(p95));
    eprintln!("  P99:           {:>8.3} ms", to_ms(p99));
    eprintln!("  max:           {:>8.3} ms", to_ms(max));
    eprintln!();
}

fn to_ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Register a Criterion bench function so users get the standard
/// mean / median + 95 % confidence interval report under
/// `target/criterion/search_end_to_end_100k/`. The fixture is shared
/// with the percentile measurement to avoid building 100k nodes twice.
fn run_criterion(engine: &Engine, criterion: &mut Criterion) {
    let input = make_search_input();
    criterion.bench_function("search_end_to_end_100k", |b| {
        b.iter(|| {
            let result = engine
                .search(black_box(input.clone()))
                .expect("search should succeed");
            black_box(result);
        });
    });
}

#[cfg(not(test))]
fn main() {
    eprintln!("Building deterministic {NUM_NODES}-node fixture (no embeddings)...");
    let build_start = Instant::now();
    let engine = build_fixture();
    let build_secs = build_start.elapsed().as_secs_f64();
    eprintln!(
        "Fixture built in {:.2} s (node_count = {})",
        build_secs, NUM_NODES
    );

    eprintln!(
        "Collecting {NUM_SAMPLES} timed samples after {WARMUP_ITERATIONS} warmup iterations..."
    );
    let samples = measure_samples(&engine);
    print_baseline_summary(&samples);

    eprintln!("Running Criterion benchmark for mean / median / CI report...");
    let mut criterion = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(10))
        .configure_from_args();
    run_criterion(&engine, &mut criterion);
    criterion.final_summary();
}

#[cfg(test)]
fn main() {}
