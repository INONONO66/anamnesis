//! Topology helpers — graph structure analysis.
//!
//! All functions are pure: no side effects, no storage mutation.
//!
//! Provides degree analysis, bridge scoring, and support scoring for nodes
//! in the cognitive graph.

use crate::error::Error;
use crate::graph::{EdgeType, NodeId};
use crate::storage::StorageAdapter;

/// Returns the number of incoming edges to a node.
///
/// # Arguments
/// - `storage`: The storage backend
/// - `id`: The node ID to analyze
///
/// # Returns
/// The count of incoming edges (edges where this node is the target).
pub fn degree_in<S: StorageAdapter>(storage: &S, id: NodeId) -> usize {
    storage.edges_to(id).len()
}

/// Returns the number of outgoing edges from a node.
///
/// # Arguments
/// - `storage`: The storage backend
/// - `id`: The node ID to analyze
///
/// # Returns
/// The count of outgoing edges (edges where this node is the source).
pub fn degree_out<S: StorageAdapter>(storage: &S, id: NodeId) -> usize {
    storage.edges_from(id).len()
}

/// Returns the total degree of a node (incoming + outgoing edges).
///
/// # Arguments
/// - `storage`: The storage backend
/// - `id`: The node ID to analyze
///
/// # Returns
/// The total degree (in + out).
pub fn degree<S: StorageAdapter>(storage: &S, id: NodeId) -> usize {
    degree_in(storage, id) + degree_out(storage, id)
}

/// Returns true if a node is an orphan (has no incoming or outgoing edges).
///
/// # Arguments
/// - `storage`: The storage backend
/// - `id`: The node ID to analyze
///
/// # Returns
/// True if the node has degree 0, false otherwise.
pub fn is_orphan<S: StorageAdapter>(storage: &S, id: NodeId) -> bool {
    degree(storage, id) == 0
}

/// Computes the bridge score for a node.
///
/// Bridge score measures how well a node bridges different parts of the graph.
/// It combines three factors:
/// - Degree score (0.50 weight): normalized degree relative to reference degree
/// - Scope diversity (0.25 weight): fraction of neighbors with different scopes
/// - Entity diversity (0.25 weight): fraction of unique entity tags across neighbors
///
/// # Arguments
/// - `storage`: The storage backend
/// - `id`: The node ID to analyze
/// - `d_ref`: Reference degree for normalization (typically the average or max degree)
///
/// # Returns
/// Bridge score clamped to [0, 1].
///
/// # Errors
/// Returns an error if the node does not exist.
pub fn bridge_score<S: StorageAdapter>(
    storage: &S,
    id: NodeId,
    d_ref: usize,
) -> Result<f64, Error> {
    // Get the node to access its scope
    let node = storage.get_node(id)?;
    let node_scope = &node.origin.scope;

    // Compute degree score: min(degree / d_ref, 1.0) * 0.5
    let d = degree(storage, id);
    let degree_score = if d_ref == 0 {
        0.0
    } else {
        ((d as f64) / (d_ref as f64)).min(1.0) * 0.5
    };

    // Collect all neighbors (both incoming and outgoing)
    let mut neighbor_ids = Vec::new();
    for edge_id in storage.edges_from(id) {
        if let Ok(edge) = storage.get_edge(*edge_id) {
            neighbor_ids.push(edge.target);
        }
    }
    for edge_id in storage.edges_to(id) {
        if let Ok(edge) = storage.get_edge(*edge_id) {
            neighbor_ids.push(edge.source);
        }
    }

    // Deduplicate neighbors
    neighbor_ids.sort();
    neighbor_ids.dedup();

    if neighbor_ids.is_empty() {
        // No neighbors: degree_score is the only component
        return Ok(degree_score.clamp(0.0, 1.0));
    }

    // Compute scope diversity: unique scopes / total neighbors
    let mut unique_scopes = std::collections::HashSet::new();
    for neighbor_id in &neighbor_ids {
        if let Ok(neighbor) = storage.get_node(*neighbor_id) {
            if neighbor.origin.scope != *node_scope {
                unique_scopes.insert(neighbor.origin.scope.clone());
            }
        }
    }
    let scope_diversity = (unique_scopes.len() as f64) / (neighbor_ids.len() as f64) * 0.25;

    // Compute entity diversity: unique entity tags across neighbors / max entity tags
    let mut all_entity_tags = std::collections::HashSet::new();
    for neighbor_id in &neighbor_ids {
        if let Ok(neighbor) = storage.get_node(*neighbor_id) {
            for tag in &neighbor.entity_tags {
                all_entity_tags.insert(tag.clone());
            }
        }
    }
    // Assume max entity tags per node is 10 (reasonable upper bound)
    let max_entity_tags = 10.0;
    let entity_diversity = (all_entity_tags.len() as f64) / max_entity_tags * 0.25;

    let score = degree_score + scope_diversity + entity_diversity;
    Ok(score.clamp(0.0, 1.0))
}

/// Computes the support score for a node.
///
/// Support score measures the fraction of incoming edges that are supportive.
/// Supportive edge types are: Supports, ReinforcedBy, ConsolidatedFrom, ExtractedFrom, Entity, Reason.
///
/// # Arguments
/// - `storage`: The storage backend
/// - `id`: The node ID to analyze
///
/// # Returns
/// Support score clamped to [0, 1]. Returns 0.0 if the node has no incoming edges.
///
/// # Errors
/// Returns an error if the node does not exist or edge access fails.
pub fn support_score<S: StorageAdapter>(storage: &S, id: NodeId) -> Result<f64, Error> {
    let incoming_edges = storage.edges_to(id);

    if incoming_edges.is_empty() {
        return Ok(0.0);
    }

    let mut supportive_count = 0;
    for edge_id in incoming_edges {
        if let Ok(edge) = storage.get_edge(*edge_id) {
            if is_supportive_edge(&edge.edge_type) {
                supportive_count += 1;
            }
        }
    }

    let score = (supportive_count as f64) / (incoming_edges.len() as f64);
    Ok(score.clamp(0.0, 1.0))
}

/// Returns true if an edge type is supportive.
///
/// Supportive edge types: Supports, ReinforcedBy, ConsolidatedFrom, ExtractedFrom, Entity, Reason.
fn is_supportive_edge(edge_type: &EdgeType) -> bool {
    matches!(
        edge_type,
        EdgeType::Supports
            | EdgeType::ReinforcedBy
            | EdgeType::ConsolidatedFrom
            | EdgeType::ExtractedFrom
            | EdgeType::Entity
            | EdgeType::Reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Origin;
    use crate::graph::scope::ScopePath;
    use crate::graph::{Edge, EdgeId, KnowledgeType, Node, NodeId, Timestamp};
    use crate::storage::SqliteStorage;

    fn make_origin(_agent: &str, session: &str, scope: &str) -> Origin {
        Origin {
            peer_id: crate::graph::types::PeerId(0),
            source_kind: crate::peer::SourceKind::AgentObservation,
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
    fn orphan_node_has_zero_degree() {
        let mut storage = SqliteStorage::new().unwrap();
        let node = make_node(1, "agent-1", "session-1", "project-a");
        storage.set_node(node).expect("set node");

        assert!(is_orphan(&storage, NodeId(1)));
        assert_eq!(degree(&storage, NodeId(1)), 0);
        assert_eq!(degree_in(&storage, NodeId(1)), 0);
        assert_eq!(degree_out(&storage, NodeId(1)), 0);
    }

    #[test]
    fn degree_counts_incoming_and_outgoing() {
        let mut storage = SqliteStorage::new().unwrap();
        let node1 = make_node(1, "agent-1", "session-1", "project-a");
        let node2 = make_node(2, "agent-1", "session-1", "project-a");
        let node3 = make_node(3, "agent-1", "session-1", "project-a");

        storage.set_node(node1).expect("set node1");
        storage.set_node(node2).expect("set node2");
        storage.set_node(node3).expect("set node3");

        // Create edges: 1 -> 2, 3 -> 1
        let edge1 = Edge {
            id: EdgeId(1),
            source: NodeId(1),
            target: NodeId(2),
            edge_type: EdgeType::Semantic,
            weight: 0.8,
            conductance: 0.0,
            edge_source: crate::graph::edge::EdgeSource::Auto,
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
            edge_source: crate::graph::edge::EdgeSource::Auto,
            created_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            metadata: Default::default(),
        };

        storage.set_edge(edge1).expect("set edge1");
        storage.set_edge(edge2).expect("set edge2");

        // Node 1: 1 outgoing (to 2), 1 incoming (from 3)
        assert_eq!(degree_out(&storage, NodeId(1)), 1);
        assert_eq!(degree_in(&storage, NodeId(1)), 1);
        assert_eq!(degree(&storage, NodeId(1)), 2);

        // Node 2: 0 outgoing, 1 incoming (from 1)
        assert_eq!(degree_out(&storage, NodeId(2)), 0);
        assert_eq!(degree_in(&storage, NodeId(2)), 1);
        assert_eq!(degree(&storage, NodeId(2)), 1);

        // Node 3: 1 outgoing (to 1), 0 incoming
        assert_eq!(degree_out(&storage, NodeId(3)), 1);
        assert_eq!(degree_in(&storage, NodeId(3)), 0);
        assert_eq!(degree(&storage, NodeId(3)), 1);
    }

    #[test]
    fn bridge_score_with_cross_scope_neighbors() {
        let mut storage = SqliteStorage::new().unwrap();

        // Create nodes in different scopes
        let node1 = make_node(1, "agent-1", "session-1", "project-a");
        let node2 = make_node(2, "agent-1", "session-1", "project-a");
        let node3 = make_node(3, "agent-1", "session-1", "project-b");

        storage.set_node(node1).expect("set node1");
        storage.set_node(node2).expect("set node2");
        storage.set_node(node3).expect("set node3");

        // Create edges: 1 -> 2, 1 -> 3
        let edge1 = Edge {
            id: EdgeId(1),
            source: NodeId(1),
            target: NodeId(2),
            edge_type: EdgeType::Semantic,
            weight: 0.8,
            conductance: 0.0,
            edge_source: crate::graph::edge::EdgeSource::Auto,
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
            edge_source: crate::graph::edge::EdgeSource::Auto,
            created_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            metadata: Default::default(),
        };

        storage.set_edge(edge1).expect("set edge1");
        storage.set_edge(edge2).expect("set edge2");

        // Node 1 has degree 2, with 1 neighbor in different scope
        // degree_score = min(2/10, 1.0) * 0.5 = 0.1
        // scope_diversity = 1/2 * 0.25 = 0.125
        // entity_diversity = 0/10 * 0.25 = 0
        // total = 0.225
        let score = bridge_score(&storage, NodeId(1), 10).expect("bridge score");
        assert!(score > 0.2 && score < 0.3, "expected ~0.225, got {}", score);
    }

    #[test]
    fn support_score_filters_supportive_edges() {
        let mut storage = SqliteStorage::new().unwrap();

        let node1 = make_node(1, "agent-1", "session-1", "project-a");
        let node2 = make_node(2, "agent-1", "session-1", "project-a");
        let node3 = make_node(3, "agent-1", "session-1", "project-a");
        let node4 = make_node(4, "agent-1", "session-1", "project-a");

        storage.set_node(node1).expect("set node1");
        storage.set_node(node2).expect("set node2");
        storage.set_node(node3).expect("set node3");
        storage.set_node(node4).expect("set node4");

        // Create incoming edges to node 1:
        // - 2 -> 1 (Supports, supportive)
        // - 3 -> 1 (Contradicts, not supportive)
        // - 4 -> 1 (ReinforcedBy, supportive)
        let edge1 = Edge {
            id: EdgeId(1),
            source: NodeId(2),
            target: NodeId(1),
            edge_type: EdgeType::Supports,
            weight: 0.8,
            conductance: 0.0,
            edge_source: crate::graph::edge::EdgeSource::Auto,
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
            edge_source: crate::graph::edge::EdgeSource::Auto,
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
            edge_source: crate::graph::edge::EdgeSource::Auto,
            created_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            metadata: Default::default(),
        };

        storage.set_edge(edge1).expect("set edge1");
        storage.set_edge(edge2).expect("set edge2");
        storage.set_edge(edge3).expect("set edge3");

        // Node 1 has 3 incoming edges: 2 supportive, 1 not
        // support_score = 2/3 ≈ 0.667
        let score = support_score(&storage, NodeId(1)).expect("support score");
        assert!(
            (score - 2.0 / 3.0).abs() < 1e-10,
            "expected 2/3, got {}",
            score
        );
    }

    #[test]
    fn support_score_zero_for_no_incoming_edges() {
        let mut storage = SqliteStorage::new().unwrap();
        let node = make_node(1, "agent-1", "session-1", "project-a");
        storage.set_node(node).expect("set node");

        let score = support_score(&storage, NodeId(1)).expect("support score");
        assert_eq!(score, 0.0);
    }

    #[test]
    fn bridge_score_zero_degree_reference() {
        let mut storage = SqliteStorage::new().unwrap();
        let node = make_node(1, "agent-1", "session-1", "project-a");
        storage.set_node(node).expect("set node");

        // With d_ref = 0, degree_score should be 0
        let score = bridge_score(&storage, NodeId(1), 0).expect("bridge score");
        assert_eq!(score, 0.0);
    }
}
