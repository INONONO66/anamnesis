//! Comprehensive tests for SqliteStorage Clone implementation.
//!
//! Verifies that cloned storage is fully independent from the original,
//! preserves all data structures (nodes, edges, hot fields, indexes),
//! and maintains consistency across SoA arrays and secondary indexes.

use anamnesis::graph::node::Origin;
use anamnesis::graph::{Edge, EdgeId, EdgeType, KnowledgeType, Node, NodeId, Timestamp};
use anamnesis::storage::{SqliteStorage, StorageAdapter};
use std::collections::HashMap;

/// Helper to create a test node with given ID and type.
fn make_node(id: NodeId, node_type: KnowledgeType, name: &str) -> Node {
    Node {
        id,
        node_type,
        name: name.to_string(),
        summary: Some(format!("Summary of {}", name)),
        content: format!("Full content of {}", name),
        embedding: Some(vec![0.1, 0.2, 0.3]),
        created_at: Timestamp(1000),
        updated_at: Timestamp(1000),
        accessed_at: Timestamp(1000),
        valid_from: None,
        valid_until: None,
        access_count: 0,
        access_history: Default::default(),
        salience: 0.75,
        retained_action: 0.0,
        evidence_prior: 0.0,
        tier: Default::default(),
        entity_tags: vec!["tag1".to_string(), "tag2".to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::new("project-1").expect("valid scope"),
            confidence: 0.9,
        },
        metadata: HashMap::new(),
    }
}

/// Helper to create a test edge.
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

#[test]
fn clone_independence_nodes() {
    // Create original storage with nodes
    let mut original = SqliteStorage::new().unwrap();
    let node1 = make_node(NodeId(0), KnowledgeType::Semantic, "node1");
    let node2 = make_node(NodeId(1), KnowledgeType::Decision, "node2");

    original.set_node(node1).unwrap();
    original.set_node(node2).unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Verify both have same node count
    assert_eq!(original.node_count(), 2);
    assert_eq!(cloned.node_count(), 2);

    // Mutate original: add a new node
    let node3 = make_node(NodeId(2), KnowledgeType::Episodic, "node3");
    original.set_node(node3).unwrap();

    // Verify original has 3 nodes, cloned still has 2
    assert_eq!(original.node_count(), 3);
    assert_eq!(cloned.node_count(), 2);

    // Verify cloned still has original nodes
    assert!(cloned.get_node(NodeId(0)).is_ok());
    assert!(cloned.get_node(NodeId(1)).is_ok());
    assert!(cloned.get_node(NodeId(2)).is_err());
}

#[test]
fn clone_independence_edges() {
    // Create original storage with nodes and edges
    let mut original = SqliteStorage::new().unwrap();
    let node1 = make_node(NodeId(0), KnowledgeType::Semantic, "node1");
    let node2 = make_node(NodeId(1), KnowledgeType::Decision, "node2");

    original.set_node(node1).unwrap();
    original.set_node(node2).unwrap();

    let edge1 = make_edge(EdgeId(0), NodeId(0), NodeId(1));
    original.set_edge(edge1).unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Verify both have same edge count
    assert_eq!(original.edge_count(), 1);
    assert_eq!(cloned.edge_count(), 1);

    // Mutate original: add a new edge
    let edge2 = make_edge(EdgeId(1), NodeId(1), NodeId(0));
    original.set_edge(edge2).unwrap();

    // Verify original has 2 edges, cloned still has 1
    assert_eq!(original.edge_count(), 2);
    assert_eq!(cloned.edge_count(), 1);

    // Verify cloned still has original edge
    assert!(cloned.get_edge(EdgeId(0)).is_ok());
    assert!(cloned.get_edge(EdgeId(1)).is_err());
}

#[test]
fn clone_soa_hot_fields_consistency() {
    // Create original storage with a node
    let mut original = SqliteStorage::new().unwrap();
    let node = make_node(NodeId(0), KnowledgeType::Semantic, "test");
    original.set_node(node).unwrap();

    // Set hot fields
    original.set_salience(NodeId(0), 0.95).unwrap();
    original
        .set_accessed_at(NodeId(0), Timestamp(5000))
        .unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Verify hot fields match in cloned storage
    assert_eq!(cloned.get_salience(NodeId(0)).unwrap(), 0.95);
    assert_eq!(cloned.get_accessed_at(NodeId(0)).unwrap(), Timestamp(5000));

    // Verify node type is preserved
    assert_eq!(
        cloned.get_node_type(NodeId(0)).unwrap(),
        &KnowledgeType::Semantic
    );

    // Verify node content matches
    let cloned_node = cloned.get_node(NodeId(0)).unwrap();
    assert_eq!(cloned_node.name, "test");
    assert_eq!(cloned_node.salience, 0.95);
    assert_eq!(cloned_node.accessed_at, Timestamp(5000));
}

#[test]
fn clone_soa_hot_fields_independence() {
    // Create original storage with a node
    let mut original = SqliteStorage::new().unwrap();
    let node = make_node(NodeId(0), KnowledgeType::Semantic, "test");
    original.set_node(node).unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Mutate hot fields in original
    original.set_salience(NodeId(0), 0.5).unwrap();
    original
        .set_accessed_at(NodeId(0), Timestamp(9999))
        .unwrap();

    // Verify cloned storage is unaffected
    assert_eq!(cloned.get_salience(NodeId(0)).unwrap(), 0.75); // original value
    assert_eq!(cloned.get_accessed_at(NodeId(0)).unwrap(), Timestamp(1000)); // original value

    // Verify original was mutated
    assert_eq!(original.get_salience(NodeId(0)).unwrap(), 0.5);
    assert_eq!(
        original.get_accessed_at(NodeId(0)).unwrap(),
        Timestamp(9999)
    );
}

#[test]
fn clone_adjacency_index_preservation() {
    // Create original storage with nodes and edges
    let mut original = SqliteStorage::new().unwrap();
    let node1 = make_node(NodeId(0), KnowledgeType::Semantic, "node1");
    let node2 = make_node(NodeId(1), KnowledgeType::Decision, "node2");
    let node3 = make_node(NodeId(2), KnowledgeType::Episodic, "node3");

    original.set_node(node1).unwrap();
    original.set_node(node2).unwrap();
    original.set_node(node3).unwrap();

    // Create edges: 0→1, 0→2, 1→2
    let edge1 = make_edge(EdgeId(0), NodeId(0), NodeId(1));
    let edge2 = make_edge(EdgeId(1), NodeId(0), NodeId(2));
    let edge3 = make_edge(EdgeId(2), NodeId(1), NodeId(2));

    original.set_edge(edge1).unwrap();
    original.set_edge(edge2).unwrap();
    original.set_edge(edge3).unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Verify adjacency_out (outgoing edges)
    let out_0 = cloned.edges_from(NodeId(0));
    assert_eq!(out_0.len(), 2);
    assert!(out_0.contains(&EdgeId(0)));
    assert!(out_0.contains(&EdgeId(1)));

    let out_1 = cloned.edges_from(NodeId(1));
    assert_eq!(out_1.len(), 1);
    assert!(out_1.contains(&EdgeId(2)));

    // Verify adjacency_in (incoming edges)
    let in_1 = cloned.edges_to(NodeId(1));
    assert_eq!(in_1.len(), 1);
    assert!(in_1.contains(&EdgeId(0)));

    let in_2 = cloned.edges_to(NodeId(2));
    assert_eq!(in_2.len(), 2);
    assert!(in_2.contains(&EdgeId(1)));
    assert!(in_2.contains(&EdgeId(2)));
}

#[test]
fn clone_secondary_indexes_preservation() {
    // Create original storage with nodes having different tags and types
    let mut original = SqliteStorage::new().unwrap();

    let mut node1 = make_node(NodeId(0), KnowledgeType::Semantic, "node1");
    node1.entity_tags = vec!["auth".to_string(), "security".to_string()];
    node1.origin.peer_id = anamnesis::graph::types::PeerId(1);
    node1.origin.scope = anamnesis::graph::ScopePath::new("project-1").expect("valid scope");

    let mut node2 = make_node(NodeId(1), KnowledgeType::Decision, "node2");
    node2.entity_tags = vec!["auth".to_string()];
    node2.origin.peer_id = anamnesis::graph::types::PeerId(2);
    node2.origin.scope = anamnesis::graph::ScopePath::new("project-1").expect("valid scope");

    let mut node3 = make_node(NodeId(2), KnowledgeType::Semantic, "node3");
    node3.entity_tags = vec!["database".to_string()];
    node3.origin.peer_id = anamnesis::graph::types::PeerId(3);
    node3.origin.scope = anamnesis::graph::ScopePath::new("project-2").expect("valid scope");

    original.set_node(node1).unwrap();
    original.set_node(node2).unwrap();
    original.set_node(node3).unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Verify entity_tag_index
    let auth_nodes = cloned.nodes_by_entity_tag("auth");
    assert_eq!(auth_nodes.len(), 2);
    assert!(auth_nodes.contains(&NodeId(0)));
    assert!(auth_nodes.contains(&NodeId(1)));

    let db_nodes = cloned.nodes_by_entity_tag("database");
    assert_eq!(db_nodes.len(), 1);
    assert!(db_nodes.contains(&NodeId(2)));

    // Verify type_index
    let semantic_nodes = cloned.nodes_by_type(&KnowledgeType::Semantic);
    assert_eq!(semantic_nodes.len(), 2);
    assert!(semantic_nodes.contains(&NodeId(0)));
    assert!(semantic_nodes.contains(&NodeId(2)));

    let decision_nodes = cloned.nodes_by_type(&KnowledgeType::Decision);
    assert_eq!(decision_nodes.len(), 1);
    assert!(decision_nodes.contains(&NodeId(1)));

    // Verify peer_index
    let peer1_nodes = cloned.nodes_by_peer(anamnesis::graph::types::PeerId(1));
    assert_eq!(peer1_nodes.len(), 1);
    assert!(peer1_nodes.contains(&NodeId(0)));
    let peer2_nodes = cloned.nodes_by_peer(anamnesis::graph::types::PeerId(2));
    assert_eq!(peer2_nodes.len(), 1);
    assert!(peer2_nodes.contains(&NodeId(1)));

    // Verify project_index
    let proj1_nodes =
        cloned.nodes_by_scope(&anamnesis::graph::ScopePath::new("project-1").expect("valid scope"));
    assert_eq!(proj1_nodes.len(), 2);
    assert!(proj1_nodes.contains(&NodeId(0)));
    assert!(proj1_nodes.contains(&NodeId(1)));

    let proj2_nodes =
        cloned.nodes_by_scope(&anamnesis::graph::ScopePath::new("project-2").expect("valid scope"));
    assert_eq!(proj2_nodes.len(), 1);
    assert!(proj2_nodes.contains(&NodeId(2)));
}

#[test]
fn clone_text_search_preservation() {
    // Create original storage with nodes
    let mut original = SqliteStorage::new().unwrap();

    let mut node1 = make_node(NodeId(0), KnowledgeType::Semantic, "authentication system");
    node1.content = "The authentication system uses OAuth2 for security".to_string();

    let mut node2 = make_node(NodeId(1), KnowledgeType::Decision, "database choice");
    node2.content = "We chose PostgreSQL for the database".to_string();

    original.set_node(node1).unwrap();
    original.set_node(node2).unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Verify text search works on cloned storage
    let auth_results = cloned.text_search("authentication", 10);
    assert!(!auth_results.is_empty());
    assert!(auth_results.iter().any(|(id, _)| *id == NodeId(0)));

    let db_results = cloned.text_search("database", 10);
    assert!(!db_results.is_empty());
    assert!(db_results.iter().any(|(id, _)| *id == NodeId(1)));

    let oauth_results = cloned.text_search("oauth", 10);
    assert!(!oauth_results.is_empty());
    assert!(oauth_results.iter().any(|(id, _)| *id == NodeId(0)));
}

#[test]
fn clone_all_node_ids_and_edge_ids() {
    // Create original storage with multiple nodes and edges
    let mut original = SqliteStorage::new().unwrap();

    for i in 0..5 {
        let node = make_node(NodeId(i), KnowledgeType::Semantic, &format!("node{}", i));
        original.set_node(node).unwrap();
    }

    for i in 0..4 {
        let edge = make_edge(EdgeId(i), NodeId(i), NodeId(i + 1));
        original.set_edge(edge).unwrap();
    }

    // Clone the storage
    let cloned = original.clone();

    // Verify all_node_ids
    let original_node_ids = original.all_node_ids();
    let cloned_node_ids = cloned.all_node_ids();

    assert_eq!(original_node_ids.len(), 5);
    assert_eq!(cloned_node_ids.len(), 5);
    assert_eq!(original_node_ids, cloned_node_ids);

    // Verify all_edge_ids
    let original_edge_ids = original.all_edge_ids();
    let cloned_edge_ids = cloned.all_edge_ids();

    assert_eq!(original_edge_ids.len(), 4);
    assert_eq!(cloned_edge_ids.len(), 4);
    assert_eq!(original_edge_ids, cloned_edge_ids);
}

#[test]
fn clone_with_deleted_nodes_and_edges() {
    // Create original storage with nodes and edges
    let mut original = SqliteStorage::new().unwrap();

    let node1 = make_node(NodeId(0), KnowledgeType::Semantic, "node1");
    let node2 = make_node(NodeId(1), KnowledgeType::Decision, "node2");
    let node3 = make_node(NodeId(2), KnowledgeType::Episodic, "node3");

    original.set_node(node1).unwrap();
    original.set_node(node2).unwrap();
    original.set_node(node3).unwrap();

    let edge1 = make_edge(EdgeId(0), NodeId(0), NodeId(1));
    let edge2 = make_edge(EdgeId(1), NodeId(1), NodeId(2));

    original.set_edge(edge1).unwrap();
    original.set_edge(edge2).unwrap();

    // Delete a node and edge from original
    original.delete_edge(EdgeId(0)).unwrap();
    original.delete_node(NodeId(1)).unwrap();

    // Clone the storage
    let cloned = original.clone();

    // Verify cloned storage reflects deletions
    assert_eq!(cloned.node_count(), 2);
    assert_eq!(cloned.edge_count(), 1);

    assert!(cloned.get_node(NodeId(0)).is_ok());
    assert!(cloned.get_node(NodeId(1)).is_err());
    assert!(cloned.get_node(NodeId(2)).is_ok());

    assert!(cloned.get_edge(EdgeId(0)).is_err());
    assert!(cloned.get_edge(EdgeId(1)).is_ok());

    // Verify adjacency is correct after deletion
    let out_0 = cloned.edges_from(NodeId(0));
    assert_eq!(out_0.len(), 0); // edge to node 1 was deleted

    let in_2 = cloned.edges_to(NodeId(2));
    assert_eq!(in_2.len(), 1);
    assert!(in_2.contains(&EdgeId(1)));
}

#[test]
fn clone_id_allocation_independence() {
    // Create original storage and allocate some IDs
    let mut original = SqliteStorage::new().unwrap();

    let id1 = original.next_node_id();
    let id2 = original.next_node_id();

    // Clone the storage (counter is now at 2)
    let mut cloned = original.clone();

    // Allocate new IDs from both (both have counter = 2)
    let id3_original = original.next_node_id();
    let id3_cloned = cloned.next_node_id();

    // Both should allocate the same next ID (they share the counter state at clone time)
    assert_eq!(id3_original, id3_cloned);
    assert_eq!(id1, NodeId(0));
    assert_eq!(id2, NodeId(1));
    assert_eq!(id3_original, NodeId(2));

    // Further allocations will diverge because each maintains independent counter state
    let id4_original = original.next_node_id();
    let id4_cloned = cloned.next_node_id();

    // Both allocate the next sequential ID from their independent counters
    assert_eq!(id4_original, NodeId(3));
    assert_eq!(id4_cloned, NodeId(3));
}
