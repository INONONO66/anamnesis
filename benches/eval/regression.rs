//! Phase 6 quality-gate regression judges over composed multi-tier fixtures.
//!
//! Two automated judges, run as ordinary `cargo test` gates against the
//! deterministic [`build_composed_tiers`] fixtures
//! (benchmarks.md "Regression Judgment"):
//!
//! - [`judge_latency_regression`] — latency is judged by **p95 for each
//!   fixture**. Each tier collects timed `Engine::search()` samples and asserts
//!   the per-fixture p95 stays under a re-recorded floor.
//! - [`judge_quality_regression`] — output quality is judged by **bucket shape
//!   and tension presence** for golden queries, plus aggregate `precision@5`
//!   and `recall@10`. The floors are **re-derived on the current model**
//!   (not loosened) and recorded in `benches/eval/baseline.md`.
//!
//! Failures are categorized as performance-budget failures or context-shape
//! changes, exactly as the spec requires. The fixtures carry no embeddings,
//! no randomness, and a fixed ingest order, so every measurement is
//! reproducible on the same revision.
//!
//! Run as: `cargo test --test eval_regression -- --nocapture`.

#[path = "composed_fixtures.rs"]
mod composed_fixtures;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anamnesis::NodeId;
use anamnesis::query::SearchInput;

use composed_fixtures::{AGENT_PEER_ID, ComposedTier, build_composed_tiers};

// ── re-derived golden floors (current model) ────────────────────────────────
//
// These are recorded on the NEW Bayesian conductive-network model, not carried
// over from the old physics. They are conservative below the observed values so
// legitimate ranking changes that preserve cluster recall still pass, while a
// real regression (a cluster dropping out of the top-k) trips the gate.

/// Aggregate precision@5 floor over the golden quality cases.
const PRECISION_AT_5_FLOOR: f64 = 0.55;
/// Aggregate recall@10 floor over the golden quality cases.
const RECALL_AT_10_FLOOR: f64 = 0.80;

// ── re-recorded per-fixture p95 latency floors (current model) ──────────────
//
// Per-fixture p95 of `Engine::search()` end-to-end on the recorded host. The
// floors carry generous headroom over the observed p95 so machine-to-machine
// variance does not produce false performance-budget failures, while a true
// blow-up (an order-of-magnitude regression) still trips the gate.

/// p95 latency budget (ms) for the `small` tier (golden core only).
const LATENCY_P95_FLOOR_SMALL_MS: f64 = 50.0;
/// p95 latency budget (ms) for the `medium` tier (+400 filler).
const LATENCY_P95_FLOOR_MEDIUM_MS: f64 = 120.0;
/// p95 latency budget (ms) for the `large` tier (+2000 filler).
const LATENCY_P95_FLOOR_LARGE_MS: f64 = 400.0;

fn latency_floor_for(label: &str) -> f64 {
    match label {
        "small" => LATENCY_P95_FLOOR_SMALL_MS,
        "medium" => LATENCY_P95_FLOOR_MEDIUM_MS,
        "large" => LATENCY_P95_FLOOR_LARGE_MS,
        other => panic!("no latency floor for tier {other}"),
    }
}

// ── golden quality cases (bucket shape + tension behavior) ──────────────────

/// One golden retrieval case declaring its expected bucket mix and tension
/// behavior (benchmarks.md "Search Scenario").
struct QualityCase {
    label: &'static str,
    query: &'static str,
    /// Whether the query carries the agent persona (so identity may be packaged).
    agent: bool,
    /// Expected relevant set (precision/recall is measured against this).
    expected: Vec<&'static str>,
    /// Whether at least one knowledge fragment must be packaged.
    expect_knowledge: bool,
    /// Whether at least one memory (episodic/event) fragment must be packaged.
    expect_memory: bool,
    /// Whether a contradiction tension must surface (both endpoints active,
    /// neither suppressed — frustration.md / ADR-0006).
    expect_tension: bool,
}

fn quality_cases() -> Vec<QualityCase> {
    vec![
        QualityCase {
            // Broad cluster query: the five "caching" knowledge members must
            // dominate the top-k (KnowledgeOnly packaging — no provenance trigger).
            label: "caching.cluster",
            query: "caching",
            agent: false,
            expected: vec![
                "k.cache.semantic",
                "k.cache.procedure",
                "k.cache.decision",
                "k.cache.convention",
                "k.cache.gotcha",
            ],
            expect_knowledge: true,
            expect_memory: false,
            expect_tension: false,
        },
        QualityCase {
            // Broad cluster anchored by an entity hub.
            label: "auth.cluster",
            query: "auth",
            agent: false,
            expected: vec![
                "k.auth.hub",
                "k.auth.jwt",
                "k.auth.session",
                "k.auth.oauth",
                "k.auth.mfa",
            ],
            expect_knowledge: true,
            expect_memory: false,
            expect_tension: false,
        },
        QualityCase {
            // Both logging claims co-activate; the Contradicts edge must surface
            // a tension and neither endpoint is suppressed (ADR-0006). The surfaced
            // tension selects KnowledgeWithProvenance packaging, which pulls the
            // ExtractedFrom episodic memory into the memory bucket — so this single
            // case exercises both tension presence and the memory bucket shape.
            label: "logging.contradiction",
            query: "logging",
            agent: false,
            expected: vec!["x.claim.old", "x.claim.new"],
            expect_knowledge: true,
            expect_memory: true,
            expect_tension: true,
        },
    ]
}

struct CaseOutcome {
    label: &'static str,
    query: &'static str,
    relevant: usize,
    precision_at_5: f64,
    recall_at_10: f64,
    has_knowledge: bool,
    has_memory: bool,
    has_tension: bool,
    top10: Vec<NodeId>,
}

fn evaluate_quality(tier: &ComposedTier, case: &QualityCase) -> CaseOutcome {
    let input = SearchInput {
        text: case.query.to_string(),
        agent_id: case.agent.then(|| AGENT_PEER_ID.to_string()),
        limit: 10,
        seed_limit: Some(10),
        ..Default::default()
    };
    let result = tier.engine.search(input).expect("search should succeed");

    // Ranking across the three content buckets, in package order.
    let ranked: Vec<NodeId> = result
        .package
        .knowledge
        .iter()
        .chain(result.package.memories.iter())
        .chain(result.package.identity.iter())
        .map(|f| f.node_id)
        .collect();

    let top5: Vec<NodeId> = ranked.iter().take(5).copied().collect();
    let top10: Vec<NodeId> = ranked.iter().take(10).copied().collect();

    let expected: HashSet<NodeId> = tier.ids_for(&case.expected).into_iter().collect();
    let p5_hits = top5.iter().filter(|id| expected.contains(*id)).count();
    let r10_hits = top10.iter().filter(|id| expected.contains(*id)).count();

    let precision_at_5 = p5_hits as f64 / 5.0;
    let recall_at_10 = if expected.is_empty() {
        0.0
    } else {
        r10_hits as f64 / expected.len() as f64
    };

    CaseOutcome {
        label: case.label,
        query: case.query,
        relevant: case.expected.len(),
        precision_at_5,
        recall_at_10,
        has_knowledge: !result.package.knowledge.is_empty(),
        has_memory: !result.package.memories.is_empty(),
        // A tension surfaces when both contradiction endpoints are active with
        // positive stress and neither has been suppressed (ADR-0006).
        has_tension: result.package.tensions.iter().any(|t| t.stress > 0.0),
        top10,
    }
}

#[test]
fn judge_quality_regression() {
    let tiers = build_composed_tiers();
    let cases = quality_cases();

    // The quality judge runs on the small (golden-core) tier: filler never
    // collides with the golden keywords, so adding filler does not change the
    // golden outcome — the bucket-shape and tension assertions are tier-stable.
    let tier = tiers
        .iter()
        .find(|t| t.label == "small")
        .expect("small tier present");

    let outcomes: Vec<CaseOutcome> = cases.iter().map(|c| evaluate_quality(tier, c)).collect();

    println!("Quality regression — per-case breakdown (tier=small):");
    for o in &outcomes {
        println!(
            "  [{}] query={:?} rel={} P@5={:.3} R@10={:.3} K={} M={} T={} top10={:?}",
            o.label,
            o.query,
            o.relevant,
            o.precision_at_5,
            o.recall_at_10,
            o.has_knowledge,
            o.has_memory,
            o.has_tension,
            o.top10,
        );
    }

    let n = outcomes.len() as f64;
    let agg_p5: f64 = outcomes.iter().map(|o| o.precision_at_5).sum::<f64>() / n;
    let agg_r10: f64 = outcomes.iter().map(|o| o.recall_at_10).sum::<f64>() / n;
    println!(
        "Quality regression — aggregate: P@5={agg_p5:.4} (floor {PRECISION_AT_5_FLOOR:.2}) \
         R@10={agg_r10:.4} (floor {RECALL_AT_10_FLOOR:.2})"
    );

    // 1) Aggregate ranking quality (context-shape changes).
    assert!(
        agg_p5 >= PRECISION_AT_5_FLOOR,
        "aggregate precision@5 = {agg_p5:.4} below re-derived floor {PRECISION_AT_5_FLOOR:.2}"
    );
    assert!(
        agg_r10 >= RECALL_AT_10_FLOOR,
        "aggregate recall@10 = {agg_r10:.4} below re-derived floor {RECALL_AT_10_FLOOR:.2}"
    );

    // 2) Per-case bucket shape + tension behavior (context-shape changes).
    for (case, o) in cases.iter().zip(&outcomes) {
        if case.expect_knowledge {
            assert!(
                o.has_knowledge,
                "[{}] expected a knowledge fragment but none packaged",
                case.label
            );
        }
        if case.expect_memory {
            assert!(
                o.has_memory,
                "[{}] expected a memory fragment but none packaged",
                case.label
            );
        }
        assert_eq!(
            o.has_tension, case.expect_tension,
            "[{}] tension presence mismatch (got {}, want {})",
            case.label, o.has_tension, case.expect_tension
        );
    }
}

#[test]
fn judge_quality_regression_is_deterministic() {
    let a = build_composed_tiers();
    let b = build_composed_tiers();
    let cases = quality_cases();
    let ta = a.iter().find(|t| t.label == "small").unwrap();
    let tb = b.iter().find(|t| t.label == "small").unwrap();

    let oa: Vec<Vec<NodeId>> = cases
        .iter()
        .map(|c| evaluate_quality(ta, c).top10)
        .collect();
    let ob: Vec<Vec<NodeId>> = cases
        .iter()
        .map(|c| evaluate_quality(tb, c).top10)
        .collect();
    assert_eq!(oa, ob, "golden top-10 ordering must be deterministic");
}

// ── latency judge (p95 per fixture) ─────────────────────────────────────────

/// Warmup iterations before the timed loop, to prime allocator/branch caches.
const WARMUP_ITERATIONS: usize = 5;
/// Timed samples per fixture used to compute the per-fixture p95.
const NUM_SAMPLES: usize = 40;

fn latency_input() -> SearchInput {
    SearchInput {
        text: "caching".to_string(),
        limit: 10,
        seed_limit: Some(10),
        ..Default::default()
    }
}

fn measure_p95_ms(tier: &ComposedTier) -> f64 {
    let input = latency_input();
    for _ in 0..WARMUP_ITERATIONS {
        let _ = tier.engine.search(input.clone()).expect("warmup search");
    }
    let mut samples: Vec<Duration> = Vec::with_capacity(NUM_SAMPLES);
    for _ in 0..NUM_SAMPLES {
        let start = Instant::now();
        let r = tier.engine.search(input.clone()).expect("search");
        let elapsed = start.elapsed();
        std::hint::black_box(&r);
        samples.push(elapsed);
    }
    samples.sort();
    // p95 with the `idx = floor(p*n)` convention, capped at n-1.
    let idx = ((samples.len() as f64) * 0.95).floor() as usize;
    let idx = idx.min(samples.len() - 1);
    samples[idx].as_secs_f64() * 1000.0
}

#[test]
fn judge_latency_regression() {
    let tiers = build_composed_tiers();

    println!("Latency regression — p95 per fixture:");
    for tier in &tiers {
        let p95 = measure_p95_ms(tier);
        let floor = latency_floor_for(tier.label);
        println!(
            "  [{}] nodes≈{} p95={:.3} ms (floor {:.1} ms)",
            tier.label,
            tier.filler_count + 22,
            p95,
            floor,
        );
        assert!(
            p95 <= floor,
            "[{}] p95 = {:.3} ms exceeds re-recorded floor {:.1} ms (performance-budget failure)",
            tier.label,
            p95,
            floor,
        );
    }
}
