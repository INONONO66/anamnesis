use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::collections::{HashMap, VecDeque};

use anamnesis::graph::node::Origin;
use anamnesis::{
    Edge, EdgeId, EdgeType, KnowledgeType, Node, NodeId, SqliteStorage, StorageAdapter, Timestamp,
};

fn make_node(id: NodeId) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Semantic,
        name: format!("node-{}", id.0),
        summary: None,
        content: format!("Content for node {}", id.0),
        embedding: None,
        created_at: Timestamp(1000),
        updated_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        salience: 0.8,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: anamnesis::graph::MemoryTier::Auto,
        origin: Origin {
            agent_id: "bench-agent".to_string(),
            session_id: "bench-session".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        entity_tags: vec![],
        metadata: HashMap::new(),
    }
}

fn make_edge(id: EdgeId, source: NodeId, target: NodeId) -> Edge {
    Edge {
        id,
        source,
        target,
        edge_type: EdgeType::Semantic,
        weight: 0.8,
        created_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: HashMap::new(),
    }
}

fn bench_storage_set_get_node(c: &mut Criterion) {
    c.bench_function("storage_set_then_get_node", |b| {
        b.iter(|| {
            let mut storage = SqliteStorage::new().unwrap();
            let id = storage.next_node_id();
            storage.set_node(black_box(make_node(id))).unwrap();
            let node = storage.get_node(id).unwrap();
            black_box(&node.name);
        })
    });
}

fn bench_storage_hot_fields(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_hot_fields");

    group.bench_function("get_set_salience", |b| {
        let mut storage = SqliteStorage::new().unwrap();
        let id = storage.next_node_id();
        storage.set_node(make_node(id)).unwrap();
        b.iter(|| {
            storage.set_salience(id, black_box(0.42)).unwrap();
            black_box(storage.get_salience(id).unwrap())
        })
    });

    group.bench_function("get_set_accessed_at", |b| {
        let mut storage = SqliteStorage::new().unwrap();
        let id = storage.next_node_id();
        storage.set_node(make_node(id)).unwrap();
        b.iter(|| {
            storage
                .set_accessed_at(id, black_box(Timestamp(5000)))
                .unwrap();
            black_box(storage.get_accessed_at(id).unwrap())
        })
    });

    group.bench_function("get_node_type", |b| {
        let mut storage = SqliteStorage::new().unwrap();
        let id = storage.next_node_id();
        storage.set_node(make_node(id)).unwrap();
        b.iter(|| black_box(storage.get_node_type(id).unwrap()))
    });

    group.finish();
}

fn bench_storage_adjacency(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_adjacency");
    for size in [10usize, 100, 1_000] {
        group.bench_with_input(BenchmarkId::new("edges_from", size), &size, |b, &size| {
            let mut storage = SqliteStorage::new().unwrap();
            let hub = storage.next_node_id();
            storage.set_node(make_node(hub)).unwrap();
            for _ in 0..size {
                let target = storage.next_node_id();
                storage.set_node(make_node(target)).unwrap();
                let eid = storage.next_edge_id();
                storage.set_edge(make_edge(eid, hub, target)).unwrap();
            }
            b.iter(|| black_box(storage.edges_from(hub)))
        });
    }
    group.finish();
}

fn bench_storage_delete_node(c: &mut Criterion) {
    c.bench_function("storage_delete_and_reuse", |b| {
        b.iter(|| {
            let mut storage = SqliteStorage::new().unwrap();
            let id = storage.next_node_id();
            storage.set_node(make_node(id)).unwrap();
            storage.delete_node(id).unwrap();
            let reused = storage.next_node_id();
            storage.set_node(black_box(make_node(reused))).unwrap();
        })
    });
}

criterion_group!(
    benches,
    bench_storage_set_get_node,
    bench_storage_hot_fields,
    bench_storage_adjacency,
    bench_storage_delete_node
);
criterion_main!(benches);
