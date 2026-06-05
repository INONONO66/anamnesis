use anamnesis::graph::node::Origin;
use anamnesis::graph::scope::ScopePath;
use anamnesis::graph::{Edge, EdgeId, EdgeType, KnowledgeType, Node, NodeId, Timestamp};
use anamnesis::mechanics::topology;
use anamnesis::storage::{SqliteStorage, StorageAdapter};

fn make_origin(_agent: &str, session: &str, scope: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session.to_string(),
        scope: ScopePath::new(scope).expect("valid scope"),
        confidence: 0.9,
    }
}

fn make_node(id: u64, agent: &str, session: &str, scope: &str) -> Node {
    Node {
        id: NodeId(id),
        node_type: KnowledgeType::Semantic,
        name: format!("node-{}", id),
        summary: None,
        content: format!("content-{}", id),
        embedding: None,
        created_at: Timestamp(1000),
        updated_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        salience: 0.5,
        retained_action: 0.0,
        access_count: 0,
        access_history: Default::default(),
        tier: Default::default(),
        origin: make_origin(agent, session, scope),
        entity_tags: vec![],
        metadata: Default::default(),
    }
}

#[test]
fn test_degree_in_out_total() {
    let mut storage = SqliteStorage::new().unwrap();
    let node1 = make_node(1, "agent-1", "session-1", "project-a");
    let node2 = make_node(2, "agent-1", "session-1", "project-a");
    let node3 = make_node(3, "agent-1", "session-1", "project-a");

    storage.set_node(node1).expect("set node1");
    storage.set_node(node2).expect("set node2");
    storage.set_node(node3).expect("set node3");

    let edge1 = Edge {
        id: EdgeId(1),
        source: NodeId(1),
        target: NodeId(2),
        edge_type: EdgeType::Semantic,
        weight: 0.8,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };
    let edge2 = Edge {
        id: EdgeId(2),
        source: NodeId(3),
        target: NodeId(1),
        edge_type: EdgeType::Semantic,
        weight: 0.7,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };

    storage.set_edge(edge1).expect("set edge1");
    storage.set_edge(edge2).expect("set edge2");

    assert_eq!(topology::degree_out(&storage, NodeId(1)), 1);
    assert_eq!(topology::degree_in(&storage, NodeId(1)), 1);
    assert_eq!(topology::degree(&storage, NodeId(1)), 2);

    assert_eq!(topology::degree_out(&storage, NodeId(2)), 0);
    assert_eq!(topology::degree_in(&storage, NodeId(2)), 1);
    assert_eq!(topology::degree(&storage, NodeId(2)), 1);

    assert_eq!(topology::degree_out(&storage, NodeId(3)), 1);
    assert_eq!(topology::degree_in(&storage, NodeId(3)), 0);
    assert_eq!(topology::degree(&storage, NodeId(3)), 1);
}

#[test]
fn test_is_orphan() {
    let mut storage = SqliteStorage::new().unwrap();
    let node = make_node(1, "agent-1", "session-1", "project-a");
    storage.set_node(node).expect("set node");

    assert!(topology::is_orphan(&storage, NodeId(1)));
}

#[test]
fn test_bridge_score_cross_scope() {
    let mut storage = SqliteStorage::new().unwrap();

    let node1 = make_node(1, "agent-1", "session-1", "project-a");
    let node2 = make_node(2, "agent-1", "session-1", "project-a");
    let node3 = make_node(3, "agent-1", "session-1", "project-b");

    storage.set_node(node1).expect("set node1");
    storage.set_node(node2).expect("set node2");
    storage.set_node(node3).expect("set node3");

    let edge1 = Edge {
        id: EdgeId(1),
        source: NodeId(1),
        target: NodeId(2),
        edge_type: EdgeType::Semantic,
        weight: 0.8,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };
    let edge2 = Edge {
        id: EdgeId(2),
        source: NodeId(1),
        target: NodeId(3),
        edge_type: EdgeType::Semantic,
        weight: 0.7,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };

    storage.set_edge(edge1).expect("set edge1");
    storage.set_edge(edge2).expect("set edge2");

    let score = topology::bridge_score(&storage, NodeId(1), 10).expect("bridge score");
    assert!(score > 0.2 && score < 0.3, "expected ~0.225, got {}", score);
}

#[test]
fn test_support_score_filters_edges() {
    let mut storage = SqliteStorage::new().unwrap();

    let node1 = make_node(1, "agent-1", "session-1", "project-a");
    let node2 = make_node(2, "agent-1", "session-1", "project-a");
    let node3 = make_node(3, "agent-1", "session-1", "project-a");
    let node4 = make_node(4, "agent-1", "session-1", "project-a");

    storage.set_node(node1).expect("set node1");
    storage.set_node(node2).expect("set node2");
    storage.set_node(node3).expect("set node3");
    storage.set_node(node4).expect("set node4");

    let edge1 = Edge {
        id: EdgeId(1),
        source: NodeId(2),
        target: NodeId(1),
        edge_type: EdgeType::Supports,
        weight: 0.8,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };
    let edge2 = Edge {
        id: EdgeId(2),
        source: NodeId(3),
        target: NodeId(1),
        edge_type: EdgeType::Contradicts,
        weight: 0.7,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };
    let edge3 = Edge {
        id: EdgeId(3),
        source: NodeId(4),
        target: NodeId(1),
        edge_type: EdgeType::ReinforcedBy,
        weight: 0.9,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };

    storage.set_edge(edge1).expect("set edge1");
    storage.set_edge(edge2).expect("set edge2");
    storage.set_edge(edge3).expect("set edge3");

    let score = topology::support_score(&storage, NodeId(1)).expect("support score");
    assert!(
        (score - 2.0 / 3.0).abs() < 1e-10,
        "expected 2/3, got {}",
        score
    );
}

#[test]
fn test_support_score_no_incoming() {
    let mut storage = SqliteStorage::new().unwrap();
    let node = make_node(1, "agent-1", "session-1", "project-a");
    storage.set_node(node).expect("set node");

    let score = topology::support_score(&storage, NodeId(1)).expect("support score");
    assert_eq!(score, 0.0);
}

#[test]
fn test_bridge_score_zero_reference() {
    let mut storage = SqliteStorage::new().unwrap();
    let node = make_node(1, "agent-1", "session-1", "project-a");
    storage.set_node(node).expect("set node");

    let score = topology::bridge_score(&storage, NodeId(1), 0).expect("bridge score");
    assert_eq!(score, 0.0);
}

#[test]
fn test_support_score_all_supportive_edges() {
    let mut storage = SqliteStorage::new().unwrap();

    let node1 = make_node(1, "agent-1", "session-1", "project-a");
    let node2 = make_node(2, "agent-1", "session-1", "project-a");
    let node3 = make_node(3, "agent-1", "session-1", "project-a");

    storage.set_node(node1).expect("set node1");
    storage.set_node(node2).expect("set node2");
    storage.set_node(node3).expect("set node3");

    let edge1 = Edge {
        id: EdgeId(1),
        source: NodeId(2),
        target: NodeId(1),
        edge_type: EdgeType::ConsolidatedFrom,
        weight: 0.8,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };
    let edge2 = Edge {
        id: EdgeId(2),
        source: NodeId(3),
        target: NodeId(1),
        edge_type: EdgeType::ExtractedFrom,
        weight: 0.7,
        conductance: 0.0,
        edge_source: anamnesis::graph::edge::EdgeSource::Auto,
        created_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        metadata: Default::default(),
    };

    storage.set_edge(edge1).expect("set edge1");
    storage.set_edge(edge2).expect("set edge2");

    let score = topology::support_score(&storage, NodeId(1)).expect("support score");
    assert_eq!(score, 1.0);
}
