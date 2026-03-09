use anamnesis::graph::edge::EdgeType;
use anamnesis::graph::{Edge, Graph, Node};

#[test]
fn test_graph_creation() {
    let graph = Graph::new();
    let nodes: Vec<_> = graph.nodes().collect();
    assert_eq!(nodes.len(), 0);
}

#[test]
fn test_add_node() {
    let mut graph = Graph::new();
    let node = Node::new("test".to_string(), "concept".to_string());
    let id = graph.add_node(node);

    assert_eq!(id, 1);
    assert!(graph.get_node(id).is_some());
}

#[test]
fn test_add_edge() {
    let mut graph = Graph::new();
    let node1 = Node::new("node1".to_string(), "concept".to_string());
    let node2 = Node::new("node2".to_string(), "concept".to_string());

    let id1 = graph.add_node(node1);
    let id2 = graph.add_node(node2);

    let edge = Edge::new(id1, id2, EdgeType::Semantic, 0.8);
    let edge_id = graph.add_edge(edge);

    assert_eq!(edge_id, 1);
    assert!(graph.get_edge(edge_id).is_some());
}

#[test]
fn test_edges_from() {
    let mut graph = Graph::new();
    let node1 = Node::new("node1".to_string(), "concept".to_string());
    let node2 = Node::new("node2".to_string(), "concept".to_string());
    let node3 = Node::new("node3".to_string(), "concept".to_string());

    let id1 = graph.add_node(node1);
    let id2 = graph.add_node(node2);
    let id3 = graph.add_node(node3);

    let edge1 = Edge::new(id1, id2, EdgeType::Semantic, 0.8);
    let edge2 = Edge::new(id1, id3, EdgeType::Causal, 0.6);

    graph.add_edge(edge1);
    graph.add_edge(edge2);

    let edges = graph.edges_from(id1);
    assert_eq!(edges.len(), 2);
}
