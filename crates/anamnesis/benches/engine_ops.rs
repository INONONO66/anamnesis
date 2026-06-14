use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EdgeType, EngineConfig, IngestResult, KnowledgeType, Timestamp};
use anamnesis::graph::node::Origin;

fn make_bench_engine() -> Engine {
    // Disable novelty/dedup gating so benchmarks can focus on operation cost.
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(-1.0)
            .with_confidence_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn make_observation(i: u64) -> Observation {
    // Distinct embeddings per node to avoid trivial deduplication
    let angle = i as f64 * 0.1;
    Observation {
        name: format!("obs-{i}"),
        summary: None,
        content: format!("Observation content {i}"),
        embedding: Some(vec![angle.cos(), angle.sin(), 0.1 * i as f64]),
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["bench".to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "bench-session".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000 + i),
        valid_from: None,
        valid_until: None,
    }
}

fn bench_touch(c: &mut Criterion) {
    c.bench_function("touch_single", |b| {
        let mut engine = make_bench_engine();
        let IngestResult::Created(ids) = engine.ingest(make_observation(0)).unwrap() else {
            panic!("expected Created");
        };
        let node_id = ids[0];
        b.iter(|| engine.touch(black_box(node_id), Timestamp::now()).unwrap())
    });
}

fn bench_touch_repeated(c: &mut Criterion) {
    let mut group = c.benchmark_group("touch_repeated");
    for count in [10usize, 100, 1_000] {
        group.bench_with_input(BenchmarkId::new("touches", count), &count, |b, &count| {
            let mut engine = make_bench_engine();
            let IngestResult::Created(ids) = engine.ingest(make_observation(0)).unwrap() else {
                panic!("expected Created");
            };
            let node_id = ids[0];
            b.iter(|| {
                for _ in 0..count {
                    engine.touch(black_box(node_id), Timestamp::now()).unwrap();
                }
            })
        });
    }
    group.finish();
}

fn bench_ingest_link_workflow(c: &mut Criterion) {
    let mut group = c.benchmark_group("ingest_link_workflow");
    for size in [10usize, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("nodes_with_links", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut engine = make_bench_engine();
                    let mut all_ids = Vec::with_capacity(size);
                    for i in 0..size {
                        let IngestResult::Created(ids) = engine
                            .ingest(black_box(make_observation(i as u64)))
                            .unwrap()
                        else {
                            panic!("expected Created");
                        };
                        all_ids.push(ids[0]);
                    }
                    for i in 0..(size - 1) {
                        engine
                            .link(all_ids[i], all_ids[i + 1], EdgeType::Semantic)
                            .unwrap();
                    }
                    engine.touch(all_ids[0], Timestamp::now()).unwrap();
                    engine.touch(all_ids[size - 1], Timestamp::now()).unwrap();
                })
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_touch,
    bench_touch_repeated,
    bench_ingest_link_workflow
);
criterion_main!(benches);
