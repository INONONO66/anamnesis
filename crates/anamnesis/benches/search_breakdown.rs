use std::hint::black_box;
use std::time::Duration;

use criterion::Criterion;

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::EngineConfig;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::SearchInput;

const NUM_NODES: usize = 100_000;

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
                    source_kind: anamnesis::engine::SourceKind::AgentObservation,
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

fn bench_search_breakdown(c: &mut Criterion) {
    let engine = build_fixture();

    let mut group = c.benchmark_group("search_breakdown");

    group.bench_function("text_only", |b| {
        b.iter(|| {
            let input = SearchInput {
                text: "rust ownership".to_string(),
                limit: 10,
                ..Default::default()
            };
            let result = engine
                .search(black_box(input))
                .expect("search should succeed");
            black_box(result);
        });
    });

    group.bench_function("entity_only", |b| {
        b.iter(|| {
            let input = SearchInput {
                text: "rust".to_string(),
                entity_tags: vec!["rust".to_string()],
                limit: 10,
                ..Default::default()
            };
            let result = engine
                .search(black_box(input))
                .expect("search should succeed");
            black_box(result);
        });
    });

    group.bench_function("text_plus_entity", |b| {
        b.iter(|| {
            let input = SearchInput {
                text: "rust ownership".to_string(),
                entity_tags: vec!["rust".to_string()],
                limit: 10,
                ..Default::default()
            };
            let result = engine
                .search(black_box(input))
                .expect("search should succeed");
            black_box(result);
        });
    });

    group.bench_function("full_search", |b| {
        b.iter(|| {
            let input = SearchInput {
                text: "rust ownership pattern".to_string(),
                scope: ScopePath::new("dev/rust").expect("valid scope"),
                entity_tags: vec!["rust".to_string()],
                limit: 10,
                seed_limit: Some(5),
                ..Default::default()
            };
            let result = engine
                .search(black_box(input))
                .expect("search should succeed");
            black_box(result);
        });
    });

    group.finish();
}

fn main() {
    let mut criterion = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5))
        .configure_from_args();
    bench_search_breakdown(&mut criterion);
    criterion.final_summary();
}
