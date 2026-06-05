use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::collections::{HashMap, VecDeque};

use anamnesis::graph::Graph;
use anamnesis::graph::node::Origin;
use anamnesis::{Edge, EdgeId, EdgeType, KnowledgeType, Node, NodeId, Timestamp};

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
        retained_action: 0.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: anamnesis::graph::MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
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
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: HashMap::new(),
    }
}

fn bench_add_nodes(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_add_nodes");
    for size in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("count", size), &size, |b, &size| {
            b.iter(|| {
                let mut graph = Graph::new();
                for _ in 0..size {
                    let id = graph.next_node_id();
                    graph.add_node(black_box(make_node(id))).unwrap();
                }
            })
        });
    }
    group.finish();
}

fn bench_add_edges(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_add_edges");
    for size in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("count", size), &size, |b, &size| {
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
            })
        });
    }
    group.finish();
}

fn bench_get_node(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_get_node");
    for size in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("graph_size", size), &size, |b, &size| {
            let mut graph = Graph::new();
            let mut node_ids = Vec::with_capacity(size);
            for _ in 0..size {
                let id = graph.next_node_id();
                graph.add_node(make_node(id)).unwrap();
                node_ids.push(id);
            }
            let mut idx = 0usize;
            b.iter(|| {
                let id = node_ids[idx % size];
                idx += 1;
                black_box(graph.get_node(id).unwrap())
            })
        });
    }
    group.finish();
}

fn bench_edges_from(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_edges_from");
    for fanout in [1usize, 10, 50] {
        group.bench_with_input(BenchmarkId::new("fanout", fanout), &fanout, |b, &fanout| {
            let mut graph = Graph::new();
            let hub_id = graph.next_node_id();
            graph.add_node(make_node(hub_id)).unwrap();
            for _ in 0..fanout {
                let target_id = graph.next_node_id();
                graph.add_node(make_node(target_id)).unwrap();
                let eid = graph.next_edge_id();
                graph.add_edge(make_edge(eid, hub_id, target_id)).unwrap();
            }
            b.iter(|| black_box(graph.edges_from(hub_id)))
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_add_nodes,
    bench_add_edges,
    bench_get_node,
    bench_edges_from
);
criterion_main!(benches);
