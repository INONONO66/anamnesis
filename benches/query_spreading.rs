use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rand::SeedableRng;
use rand::rngs::StdRng;

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::{
    EdgeType, Engine, EngineConfig, IngestResult, KnowledgeType, Query, QueryConfig, Timestamp,
};

fn make_bench_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(-1.0)
            .with_confidence_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn make_observation(i: u64, rng: &mut StdRng) -> Observation {
    use rand::Rng;

    let angle = i as f64 * 0.1;
    let noise: f64 = rng.gen_range(0.0..0.1);

    Observation {
        name: format!("node-{i}"),
        summary: None,
        content: format!("Content for observation {i}"),
        embedding: Some(vec![
            angle.cos() + noise,
            angle.sin() + noise,
            0.1 * i as f64,
        ]),
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

fn build_graph(node_count: usize) -> (Engine, Vec<anamnesis::NodeId>) {
    let mut engine = make_bench_engine();
    let mut rng = StdRng::seed_from_u64(42);
    let mut node_ids = Vec::with_capacity(node_count);

    for i in 0..node_count {
        let obs = make_observation(i as u64, &mut rng);
        match engine.ingest(obs).unwrap() {
            IngestResult::Created(ids) => node_ids.push(ids[0]),
            IngestResult::Reinforced { existing_id, .. } => node_ids.push(existing_id),
            IngestResult::CreatedWithConflict {
                node_ids: conflict_ids,
                ..
            } => node_ids.push(conflict_ids[0]),
        }
    }

    for i in 0..node_count {
        if i + 1 < node_count {
            let _ = engine.link(node_ids[i], node_ids[i + 1], EdgeType::Temporal, 0.8);
        }

        if i > 0 && i % 5 == 0 && i >= 5 {
            let _ = engine.link(node_ids[i], node_ids[i - 5], EdgeType::Semantic, 0.7);
        }
    }

    (engine, node_ids)
}

fn bench_spreading_100(c: &mut Criterion) {
    c.bench_function("spreading_100_nodes", |b| {
        b.iter_batched(
            || {
                let (engine, node_ids) = build_graph(100);
                (engine, node_ids[0])
            },
            |(engine, seed)| {
                let config = QueryConfig::default();
                let query = Query::Associative {
                    seed: black_box(seed),
                    budget: 50,
                };
                let _ = engine.query(&query, &config);
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_spreading_1k(c: &mut Criterion) {
    c.bench_function("spreading_1k_nodes", |b| {
        b.iter_batched(
            || {
                let (engine, node_ids) = build_graph(1_000);
                (engine, node_ids[0])
            },
            |(engine, seed)| {
                let config = QueryConfig::default();
                let query = Query::Associative {
                    seed: black_box(seed),
                    budget: 200,
                };
                let _ = engine.query(&query, &config);
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_spreading_10k(c: &mut Criterion) {
    c.bench_function("spreading_10k_nodes", |b| {
        b.iter_batched(
            || {
                let (engine, node_ids) = build_graph(10_000);
                (engine, node_ids[0])
            },
            |(engine, seed)| {
                let config = QueryConfig::default();
                let query = Query::Associative {
                    seed: black_box(seed),
                    budget: 500,
                };
                let _ = engine.query(&query, &config);
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_additive_rwr(c: &mut Criterion) {
    let mut group = c.benchmark_group("additive_rwr");

    group.bench_with_input(BenchmarkId::new("1k_nodes", "rwr"), &(), |b, _| {
        b.iter_batched(
            || {
                let (engine, node_ids) = build_graph(1_000);
                (engine, node_ids[0])
            },
            |(engine, seed)| {
                let config = QueryConfig::default();
                let query = Query::Associative {
                    seed: black_box(seed),
                    budget: 200,
                };
                let _ = engine.query(&query, &config);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_spreading_100,
    bench_spreading_1k,
    bench_spreading_10k,
    bench_additive_rwr
);
criterion_main!(benches);
