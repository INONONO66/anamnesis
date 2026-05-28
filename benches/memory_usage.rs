use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::collections::{HashMap, VecDeque};

use anamnesis::graph::Graph;
use anamnesis::graph::node::Origin;
use anamnesis::{Edge, EdgeId, EdgeType, KnowledgeType, Node, NodeId, Timestamp};

fn make_node(id: NodeId) -> Node {
    Node {
        id,
        node_type: KnowledgeType::Episodic,
        name: format!("episode-{}", id.0),
        summary: None,
        content: format!("Raw conversation turn content for node {}", id.0),
        embedding: Some(vec![0.1, 0.2, 0.3, 0.4, 0.5]),
        created_at: Timestamp(1000),
        updated_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        salience: 1.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: anamnesis::graph::MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "bench-session".to_string(),
            scope: anamnesis::graph::ScopePath::new("bench-project").expect("valid scope"),
            confidence: 0.85,
        },
        entity_tags: vec!["entity-a".to_string(), "entity-b".to_string()],
        metadata: HashMap::new(),
    }
}

fn make_edge(id: EdgeId, source: NodeId, target: NodeId) -> Edge {
    Edge {
        id,
        source,
        target,
        edge_type: EdgeType::Causal,
        weight: 0.75,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: HashMap::new(),
    }
}

fn bench_node_creation_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_creation_cost");
    for size in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("nodes", size), &size, |b, &size| {
            b.iter(|| {
                let mut graph = Graph::new();
                for _ in 0..size {
                    let id = graph.next_node_id();
                    graph.add_node(black_box(make_node(id))).unwrap();
                }
                black_box(graph.node_count())
            })
        });
    }
    group.finish();
}

fn bench_edge_creation_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_creation_cost");
    for size in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("edges", size), &size, |b, &size| {
            b.iter(|| {
                let mut graph = Graph::new();
                let mut node_ids = Vec::with_capacity(size + 1);
                for _ in 0..=size {
                    let id = graph.next_node_id();
                    graph.add_node(make_node(id)).unwrap();
                    node_ids.push(id);
                }
                for i in 0..size {
                    let eid = graph.next_edge_id();
                    graph
                        .add_edge(black_box(make_edge(eid, node_ids[i], node_ids[i + 1])))
                        .unwrap();
                }
                black_box(graph.edge_count())
            })
        });
    }
    group.finish();
}

fn bench_dense_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("dense_graph");
    for size in [50usize, 100, 200] {
        group.bench_with_input(
            BenchmarkId::new("nodes_fully_connected", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut graph = Graph::new();
                    let mut node_ids = Vec::with_capacity(size);
                    for _ in 0..size {
                        let id = graph.next_node_id();
                        graph.add_node(make_node(id)).unwrap();
                        node_ids.push(id);
                    }
                    for i in 0..size {
                        for j in (i + 1)..size {
                            let eid = graph.next_edge_id();
                            graph
                                .add_edge(black_box(make_edge(eid, node_ids[i], node_ids[j])))
                                .unwrap();
                        }
                    }
                    black_box((graph.node_count(), graph.edge_count()))
                })
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_node_creation_cost,
    bench_edge_creation_cost,
    bench_dense_graph
);
criterion_main!(benches);
