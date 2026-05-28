use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{MemoryTier, ScopePath};
use anamnesis::{Engine, EngineConfig, IngestResult, KnowledgeType, Timestamp};

// KPI targets: 10K nodes < 10ms, 50K nodes < 50ms

fn build_engine_with_nodes(n: usize, core_pct: usize) -> Engine {
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
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "bench".to_string(),
                scope: ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(1000 + i as u64),
        };

        if let Ok(IngestResult::Created(ids)) = engine.ingest(obs) {
            if core_pct > 0 && i * 100 / n < core_pct {
                let _ = engine.set_tier(ids[0], MemoryTier::Core);
            }
        }
    }

    engine
}

fn bench_tick_scaling(c: &mut Criterion) {
    let sizes = [1_000usize, 10_000, 50_000, 100_000];
    let core_pcts = [0usize, 25, 50];

    let mut group = c.benchmark_group("tick_scaling");

    for &size in &sizes {
        for &pct in &core_pcts {
            group.bench_with_input(
                BenchmarkId::new(format!("tick_{size}_core{pct}"), pct),
                &(size, pct),
                |b, &(size, pct)| {
                    let mut engine = build_engine_with_nodes(size, pct);
                    let mut ts: u64 = 2_000_000;
                    b.iter(|| {
                        ts += 86_400_000;
                        engine.tick(black_box(Timestamp(ts))).unwrap();
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_tick_scaling);
criterion_main!(benches);
