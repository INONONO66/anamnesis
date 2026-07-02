use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, KnowledgeType, Timestamp};
use anamnesis::graph::ScopePath;
use anamnesis::graph::node::Origin;

// KPI targets: 10K nodes < 10ms, 50K nodes < 50ms

fn build_engine_with_nodes(n: usize) -> Engine {
    let config = EngineConfig::new()
        .with_max_nodes(n * 2)
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    for i in 0..n {
        let obs = Observation {
            name: format!("node-{i}"),
            summary: None,
            content: format!("content {i}"),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::engine::SourceKind::AgentObservation,
                session_id: "bench".to_string(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(1000 + i as u64),
            valid_from: None,
            valid_until: None,
        };

        let _ = engine.ingest(obs);
    }

    engine
}

fn bench_tick_scaling(c: &mut Criterion) {
    let sizes = [1_000usize, 10_000, 50_000, 100_000];

    let mut group = c.benchmark_group("tick_scaling");

    for &size in &sizes {
        group.bench_with_input(BenchmarkId::new("tick", size), &size, |b, &size| {
            let mut engine = build_engine_with_nodes(size);
            let mut ts: u64 = 2_000_000;
            b.iter(|| {
                ts += 86_400_000;
                engine.tick(black_box(Timestamp(ts))).unwrap();
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_tick_scaling);
criterion_main!(benches);
