use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EdgeType, EngineConfig, IngestResult, KnowledgeType, Timestamp};
use anamnesis::graph::node::Origin;

fn make_bench_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(-1.0)
            .with_confidence_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn make_observation(i: u64) -> Observation {
    let angle = i as f64 * 0.1;
    Observation {
        name: format!("node-{i}"),
        summary: None,
        content: format!("Content for observation {i}"),
        embedding: Some(vec![angle.cos(), angle.sin(), 0.1 * i as f64]),
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["bench".to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "bench-session".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000 + i),
        valid_from: None,
        valid_until: None,
    }
}

fn bench_ingest_single(c: &mut Criterion) {
    c.bench_function("ingest_single", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            let mut engine = make_bench_engine();
            counter += 1;
            engine.ingest(black_box(make_observation(counter))).unwrap()
        })
    });
}

fn bench_ingest_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("ingest_batch");
    for size in [10usize, 100, 500] {
        group.bench_with_input(BenchmarkId::new("nodes", size), &size, |b, &size| {
            b.iter(|| {
                let mut engine = make_bench_engine();
                for i in 0..size {
                    engine
                        .ingest(black_box(make_observation(i as u64)))
                        .unwrap();
                }
            })
        });
    }
    group.finish();
}

fn bench_link(c: &mut Criterion) {
    c.bench_function("link_two_nodes", |b| {
        b.iter(|| {
            let mut engine = make_bench_engine();
            let IngestResult::Created(ids1) = engine.ingest(make_observation(0)).unwrap() else {
                panic!("expected Created");
            };
            let IngestResult::Created(ids2) = engine.ingest(make_observation(1)).unwrap() else {
                panic!("expected Created");
            };
            engine
                .link(black_box(ids1[0]), black_box(ids2[0]), EdgeType::Semantic)
                .unwrap()
        })
    });
}

fn bench_link_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("link_chain");
    for size in [10usize, 100, 500] {
        group.bench_with_input(BenchmarkId::new("edges", size), &size, |b, &size| {
            b.iter(|| {
                let mut engine = make_bench_engine();
                let IngestResult::Created(prev_ids) = engine.ingest(make_observation(0)).unwrap()
                else {
                    panic!("expected Created");
                };
                let mut prev = prev_ids;
                for i in 1..size {
                    let IngestResult::Created(curr_ids) =
                        engine.ingest(make_observation(i as u64)).unwrap()
                    else {
                        panic!("expected Created");
                    };
                    engine
                        .link(prev[0], curr_ids[0], EdgeType::Temporal)
                        .unwrap();
                    prev = curr_ids;
                }
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_ingest_single,
    bench_ingest_batch,
    bench_link,
    bench_link_chain
);
criterion_main!(benches);
