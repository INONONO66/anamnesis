//! Tier B golden retrieval evaluation.
//!
//! Locks aggregate precision@5 and recall@10 floors for `Engine::search()`
//! against the deterministic fixture in [`fixtures::build_golden_engine`].
//! The floors are intentionally conservative — they protect search
//! correctness without locking specific NodeIds, so legitimate ranking
//! changes that preserve cluster recall can still pass.
//!
//! Run as: `cargo test --test eval_golden -- --nocapture`.

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::HashSet;

use anamnesis::NodeId;
use anamnesis::query::SearchInput;

use fixtures::{GoldenFixture, build_golden_fixture};

const PRECISION_AT_5_FLOOR: f64 = 0.60;
const RECALL_AT_10_FLOOR: f64 = 0.70;

struct GoldenCase {
    label: &'static str,
    query: &'static str,
    expected: Vec<NodeId>,
}

fn golden_cases(fx: &GoldenFixture) -> Vec<GoldenCase> {
    let cluster_a1 = &[
        "rust.result.type",
        "rust.result.unwrap",
        "rust.result.panic",
        "rust.result.question",
        "rust.result.anyhow",
    ];
    let cluster_a2 = &[
        "rust.tokio.runtime",
        "rust.tokio.async_std",
        "rust.tokio.axum",
        "rust.tokio.futures",
        "rust.tokio.scheduler",
    ];
    let cluster_a3 = &[
        "rust.types.ownership",
        "rust.types.borrow",
        "rust.types.lifetime",
        "rust.types.shared",
        "rust.types.interior",
    ];
    let cluster_b1 = &[
        "japan.city.tokyo",
        "japan.city.kyoto",
        "japan.city.osaka",
        "japan.city.sapporo",
        "japan.city.fukuoka",
    ];
    let cluster_b2 = &[
        "japan.transport.shinkansen",
        "japan.transport.jrpass.tourist",
        "japan.transport.jrpass.abroad",
        "japan.transport.station",
        "japan.transport.express",
    ];
    let cluster_b3 = &[
        "japan.cuisine.ramen",
        "japan.cuisine.sushi",
        "japan.cuisine.tempura",
        "japan.cuisine.okonomiyaki",
        "japan.cuisine.izakaya",
    ];
    let cluster_c1 = &[
        "llm.transformer.architecture",
        "llm.transformer.self",
        "llm.transformer.multihead",
        "llm.transformer.vaswani",
        "llm.transformer.scaled",
    ];
    let cluster_c2 = &[
        "llm.positional.sinusoidal",
        "llm.positional.rope",
        "llm.positional.alibi",
        "llm.positional.learned",
        "llm.positional.nope",
    ];
    let cluster_c3 = &[
        "llm.align.rlhf",
        "llm.align.dpo",
        "llm.align.constitutional",
        "llm.align.sft",
        "llm.align.rlaif",
    ];
    let cluster_c4 = &[
        "llm.open.llama2",
        "llm.open.llama3",
        "llm.open.mistral",
        "llm.open.mixtral",
        "llm.open.qwen",
    ];

    vec![
        GoldenCase {
            label: "rust.errors.broad-result",
            query: "Result",
            expected: fx.ids_for(cluster_a1),
        },
        GoldenCase {
            label: "rust.errors.broad-errors",
            query: "errors",
            expected: fx.ids_for(cluster_a1),
        },
        GoldenCase {
            label: "rust.tokio.broad-tokio",
            query: "tokio",
            expected: fx.ids_for(cluster_a2),
        },
        GoldenCase {
            label: "rust.tokio.broad-executor",
            query: "executor",
            expected: fx.ids_for(cluster_a2),
        },
        GoldenCase {
            label: "rust.types.broad-ownership",
            query: "ownership",
            expected: fx.ids_for(cluster_a3),
        },
        GoldenCase {
            label: "rust.types.broad-borrow",
            query: "borrow",
            expected: fx.ids_for(cluster_a3),
        },
        GoldenCase {
            label: "japan.city.broad-city",
            query: "city",
            expected: fx.ids_for(cluster_b1),
        },
        GoldenCase {
            label: "japan.city.broad-prefecture",
            query: "prefecture",
            expected: fx.ids_for(cluster_b1),
        },
        GoldenCase {
            label: "japan.transport.broad-shinkansen",
            query: "shinkansen",
            expected: fx.ids_for(cluster_b2),
        },
        GoldenCase {
            label: "japan.transport.broad-rail",
            query: "rail",
            expected: fx.ids_for(cluster_b2),
        },
        GoldenCase {
            label: "japan.cuisine.broad-cuisine",
            query: "cuisine",
            expected: fx.ids_for(cluster_b3),
        },
        GoldenCase {
            label: "japan.cuisine.broad-dish",
            query: "dish",
            expected: fx.ids_for(cluster_b3),
        },
        GoldenCase {
            label: "llm.transformer.broad-transformer",
            query: "transformer",
            expected: fx.ids_for(cluster_c1),
        },
        GoldenCase {
            label: "llm.transformer.broad-attention",
            query: "attention",
            expected: fx.ids_for(cluster_c1),
        },
        GoldenCase {
            label: "llm.positional.broad-positional",
            query: "positional",
            expected: fx.ids_for(cluster_c2),
        },
        GoldenCase {
            label: "llm.positional.broad-encoding",
            query: "encoding",
            expected: fx.ids_for(cluster_c2),
        },
        GoldenCase {
            label: "llm.align.broad-alignment",
            query: "alignment",
            expected: fx.ids_for(cluster_c3),
        },
        GoldenCase {
            label: "llm.align.broad-human",
            query: "human",
            expected: fx.ids_for(cluster_c3),
        },
        GoldenCase {
            label: "llm.open.broad-open",
            query: "open",
            expected: fx.ids_for(cluster_c4),
        },
        GoldenCase {
            label: "llm.open.broad-weights",
            query: "weights",
            expected: fx.ids_for(cluster_c4),
        },
        GoldenCase {
            label: "japan.transport.mid-jrpass",
            query: "JR pass",
            expected: fx.ids_for(&[
                "japan.transport.jrpass.tourist",
                "japan.transport.jrpass.abroad",
            ]),
        },
        GoldenCase {
            label: "llm.open.mid-llama",
            query: "llama",
            expected: fx.ids_for(&["llm.open.llama2", "llm.open.llama3"]),
        },
        GoldenCase {
            label: "rust.errors.narrow-unwrap",
            query: "unwrap",
            expected: fx.ids_for(&["rust.result.unwrap"]),
        },
        GoldenCase {
            label: "rust.errors.narrow-panic",
            query: "panic",
            expected: fx.ids_for(&["rust.result.panic"]),
        },
        GoldenCase {
            label: "rust.tokio.narrow-axum",
            query: "axum",
            expected: fx.ids_for(&["rust.tokio.axum"]),
        },
        GoldenCase {
            label: "rust.types.narrow-lifetime",
            query: "lifetime",
            expected: fx.ids_for(&["rust.types.lifetime"]),
        },
        GoldenCase {
            label: "japan.cuisine.narrow-ramen",
            query: "ramen",
            expected: fx.ids_for(&["japan.cuisine.ramen"]),
        },
        GoldenCase {
            label: "llm.positional.narrow-rotary",
            query: "rotary",
            expected: fx.ids_for(&["llm.positional.rope"]),
        },
        GoldenCase {
            label: "llm.align.narrow-rlhf",
            query: "RLHF",
            expected: fx.ids_for(&["llm.align.rlhf"]),
        },
        GoldenCase {
            label: "llm.open.narrow-mistral",
            query: "mistral",
            expected: fx.ids_for(&["llm.open.mistral"]),
        },
    ]
}

struct CaseOutcome {
    label: &'static str,
    query: &'static str,
    relevant: usize,
    top5: Vec<NodeId>,
    top10: Vec<NodeId>,
    precision_at_5: f64,
    recall_at_10: f64,
}

fn evaluate(fx: &GoldenFixture, case: &GoldenCase) -> CaseOutcome {
    let result = fx
        .engine
        .search(SearchInput {
            text: case.query.to_string(),
            limit: 10,
            seed_limit: Some(10),
            ..Default::default()
        })
        .expect("search should succeed");

    let ranked: Vec<NodeId> = result
        .package
        .knowledge
        .iter()
        .chain(result.package.memories.iter())
        .chain(result.package.identity.iter())
        .map(|fragment| fragment.node_id)
        .collect();

    let top5: Vec<NodeId> = ranked.iter().take(5).copied().collect();
    let top10: Vec<NodeId> = ranked.iter().take(10).copied().collect();

    let expected: HashSet<NodeId> = case.expected.iter().copied().collect();
    let top5_hits = top5.iter().filter(|id| expected.contains(*id)).count();
    let top10_hits = top10.iter().filter(|id| expected.contains(*id)).count();

    let precision_at_5 = top5_hits as f64 / 5.0;
    let recall_at_10 = if expected.is_empty() {
        0.0
    } else {
        top10_hits as f64 / expected.len() as f64
    };

    CaseOutcome {
        label: case.label,
        query: case.query,
        relevant: case.expected.len(),
        top5,
        top10,
        precision_at_5,
        recall_at_10,
    }
}

#[test]
fn golden_eval_meets_locked_floors() {
    let fixture = build_golden_fixture();
    let cases = golden_cases(&fixture);
    assert!(
        cases.len() >= 30,
        "golden suite must have at least 30 cases, got {}",
        cases.len()
    );

    let outcomes: Vec<CaseOutcome> = cases.iter().map(|case| evaluate(&fixture, case)).collect();

    let total = outcomes.len() as f64;
    let aggregate_precision_at_5: f64 =
        outcomes.iter().map(|o| o.precision_at_5).sum::<f64>() / total;
    let aggregate_recall_at_10: f64 = outcomes.iter().map(|o| o.recall_at_10).sum::<f64>() / total;

    let floors_met = aggregate_precision_at_5 >= PRECISION_AT_5_FLOOR
        && aggregate_recall_at_10 >= RECALL_AT_10_FLOOR;

    if !floors_met {
        println!("Tier B golden — per-query breakdown:");
        for outcome in &outcomes {
            println!(
                "  [{}] query={:?} rel={} P@5={:.3} R@10={:.3} top5={:?} top10={:?}",
                outcome.label,
                outcome.query,
                outcome.relevant,
                outcome.precision_at_5,
                outcome.recall_at_10,
                outcome.top5,
                outcome.top10,
            );
        }
    }

    println!("Tier B golden — aggregate:");
    println!("  cases:        {}", outcomes.len());
    println!(
        "  precision@5:  {:.4}  (floor {:.2})",
        aggregate_precision_at_5, PRECISION_AT_5_FLOOR
    );
    println!(
        "  recall@10:    {:.4}  (floor {:.2})",
        aggregate_recall_at_10, RECALL_AT_10_FLOOR
    );

    assert!(
        aggregate_precision_at_5 >= PRECISION_AT_5_FLOOR,
        "aggregate precision@5 = {aggregate_precision_at_5:.4} below floor {PRECISION_AT_5_FLOOR:.2}"
    );
    assert!(
        aggregate_recall_at_10 >= RECALL_AT_10_FLOOR,
        "aggregate recall@10 = {aggregate_recall_at_10:.4} below floor {RECALL_AT_10_FLOOR:.2}"
    );
}

#[test]
fn golden_fixture_is_deterministic() {
    let first = build_golden_fixture();
    let second = build_golden_fixture();
    let cases = golden_cases(&first);

    let first_outcomes: Vec<Vec<NodeId>> = cases
        .iter()
        .map(|case| evaluate(&first, case).top10)
        .collect();
    let cases_second = golden_cases(&second);
    let second_outcomes: Vec<Vec<NodeId>> = cases_second
        .iter()
        .map(|case| evaluate(&second, case).top10)
        .collect();

    assert_eq!(
        first_outcomes, second_outcomes,
        "golden top-10 ordering must be deterministic across builds"
    );
}
