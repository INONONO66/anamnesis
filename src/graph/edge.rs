//! Edge type for the Anamnesis graph.

use crate::graph::types::{EdgeId, EdgeType, NodeId, Timestamp};
use std::collections::HashMap;

/// A directed relationship between two nodes in the cognitive graph.
///
/// Edge weight represents relationship strength [0, 1].
/// Edge type determines propagation multiplier (kappa) during spreading activation.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Unique identifier.
    pub id: EdgeId,
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Relationship type — determines kappa multiplier during spreading activation.
    pub edge_type: EdgeType,
    /// Relationship strength [0, 1].
    pub weight: f64,
    /// When this edge was created.
    pub created_at: Timestamp,
    /// When this relationship becomes valid in domain time.
    pub valid_from: Option<Timestamp>,
    /// When this relationship stops being valid in domain time.
    pub valid_until: Option<Timestamp>,
    /// Consumer-defined metadata.
    pub metadata: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{EdgeId, EdgeType, NodeId, Timestamp};

    #[test]
    fn edge_all_fields() {
        let edge = Edge {
            id: EdgeId(1),
            source: NodeId(10),
            target: NodeId(20),
            edge_type: EdgeType::Reason,
            weight: 0.8,
            created_at: Timestamp(1000),
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        };
        assert_eq!(edge.source, NodeId(10));
        assert_eq!(edge.target, NodeId(20));
        assert_eq!(edge.weight, 0.8);
    }

    #[test]
    fn contradicts_edge() {
        let edge = Edge {
            id: EdgeId(2),
            source: NodeId(1),
            target: NodeId(2),
            edge_type: EdgeType::Contradicts,
            weight: 0.9,
            created_at: Timestamp(500),
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        };
        assert_eq!(edge.edge_type, EdgeType::Contradicts);
    }
}
