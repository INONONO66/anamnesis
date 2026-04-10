//! Node and Origin types for the Anamnesis graph.

use crate::graph::types::{KnowledgeType, NodeId, Timestamp};
use std::collections::HashMap;

/// Provenance and scope of a knowledge fragment.
///
/// Tracks which agent produced this node, from which session,
/// and whether it belongs to a specific project or is universal.
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    /// The agent that produced this knowledge fragment.
    pub agent_id: String,
    /// The session in which this fragment was created.
    pub session_id: String,
    /// Project scope. None = universal knowledge (applies everywhere).
    /// Some("project-id") = scoped to a specific project.
    pub project_id: Option<String>,
    /// Creation-time certainty [0, 1].
    pub confidence: f64,
}

/// A knowledge fragment in the cognitive graph.
///
/// Nodes carry multi-resolution content (L0/L1/L2), physics state (salience),
/// provenance (origin), and classification (node_type, entity_tags).
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Unique identifier.
    pub id: NodeId,
    /// Knowledge type — determines decay rate, mass prior, and physics behavior.
    pub node_type: KnowledgeType,

    // Multi-resolution content
    /// L0: One-liner label (~100 tokens). Always present. Used for fast scanning.
    pub name: String,
    /// L1: Consumer-provided summary (~500 tokens). Optional acceleration layer.
    pub summary: Option<String>,
    /// L2: Full original content. Source of truth. Always preserved.
    pub content: String,
    /// Embedding vector provided by the consumer. Used for similarity-based operations.
    pub embedding: Option<Vec<f64>>,

    // Temporal fields
    /// When this node was created (immutable after creation).
    pub created_at: Timestamp,
    /// When this node was last modified.
    pub updated_at: Timestamp,
    /// When this node was last accessed via touch(). Used for lazy decay.
    pub accessed_at: Timestamp,
    /// When the fact represented by this node became valid. None = always valid.
    pub valid_from: Option<Timestamp>,
    /// When the fact represented by this node became invalid. None = still valid.
    pub valid_until: Option<Timestamp>,

    // Physics state
    /// Salience score [0, 1]. The universal currency — all mechanics read/write this.
    pub salience: f64,
    /// Number of times this node has been accessed via touch().
    pub access_count: u32,

    // Provenance
    /// Who created this node, from which session, and with what confidence.
    pub origin: Origin,

    // Classification
    /// Entity tags for automatic cross-node linking. Nodes sharing tags get Entity edges.
    pub entity_tags: Vec<String>,
    /// Consumer-defined metadata key-value pairs.
    pub metadata: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{KnowledgeType, NodeId, Timestamp};

    fn make_origin() -> Origin {
        Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: Some("anamnesis".to_string()),
            confidence: 0.9,
        }
    }

    #[test]
    fn origin_universal() {
        let o = Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: None, // universal
            confidence: 0.8,
        };
        assert!(o.project_id.is_none());
    }

    #[test]
    fn origin_project_scoped() {
        let o = make_origin();
        assert_eq!(o.project_id.as_deref(), Some("anamnesis"));
    }

    #[test]
    fn node_all_fields() {
        let node = Node {
            id: NodeId(1),
            node_type: KnowledgeType::Decision,
            name: "physics = edge weight dynamics".to_string(),
            summary: Some(
                "Force-directed simulation rejected in favor of edge weight dynamics".to_string(),
            ),
            content: "Full discussion content...".to_string(),
            embedding: Some(vec![0.1, 0.2, 0.3]),
            created_at: Timestamp(1000),
            updated_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            salience: 0.85,
            access_count: 0,
            origin: make_origin(),
            entity_tags: vec!["physics".to_string(), "anamnesis".to_string()],
            metadata: HashMap::new(),
        };
        assert_eq!(node.id, NodeId(1));
        assert_eq!(node.salience, 0.85);
        assert_eq!(node.entity_tags.len(), 2);
        assert!(node.embedding.is_some());
    }

    #[test]
    fn node_without_optional_fields() {
        let node = Node {
            id: NodeId(2),
            node_type: KnowledgeType::Episodic,
            name: "session note".to_string(),
            summary: None,
            content: "Raw conversation turn".to_string(),
            embedding: None,
            created_at: Timestamp(2000),
            updated_at: Timestamp(2000),
            accessed_at: Timestamp(2000),
            valid_from: None,
            valid_until: None,
            salience: 1.0,
            access_count: 0,
            origin: Origin {
                agent_id: "agent-1".to_string(),
                session_id: "session-2".to_string(),
                project_id: None,
                confidence: 0.7,
            },
            entity_tags: vec![],
            metadata: HashMap::new(),
        };
        assert!(node.summary.is_none());
        assert!(node.embedding.is_none());
    }
}
