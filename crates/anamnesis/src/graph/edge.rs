//! Edge type for the Anamnesis graph.

use crate::graph::types::{EdgeId, EdgeType, NodeId, Timestamp};
use std::collections::HashMap;

/// The origin of an edge — how it was created.
///
/// Consumers set this explicitly; the engine sets it automatically for
/// attraction auto-links (`Auto`) and `link()` calls (`Manual`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeSource {
    /// Created automatically by the engine (attraction).
    Auto,
    /// Created explicitly by the consumer via `Engine::link()`.
    Manual,
    /// Derived by the engine from structural analysis (e.g. crystallize).
    Inferred,
}

/// A directed relationship between two nodes in the cognitive graph.
///
/// Edge weight represents relationship strength [0, 1].
/// Edge type determines the within-row `edge_type_factor` during spreading activation.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Unique identifier.
    pub id: EdgeId,
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Relationship type — determines the `edge_type_factor` during spreading activation.
    pub edge_type: EdgeType,
    /// Relationship strength [0, 1].
    pub weight: f64,
    /// Conductance `C_ij` — authoritative log-likelihood-ratio reservoir; `weight` is its
    /// bounded projection (`weight = project_weight(conductance)`, ADR-0002).
    pub conductance: f64,
    /// How this edge was created — set automatically by the engine.
    pub edge_source: EdgeSource,
    /// When this edge was created.
    pub created_at: Timestamp,
    /// When this edge was last accessed (committed). Used for idle-edge leakage.
    pub accessed_at: Timestamp,
    /// When this relationship becomes valid in domain time.
    pub valid_from: Option<Timestamp>,
    /// When this relationship stops being valid in domain time.
    pub valid_until: Option<Timestamp>,
    /// Consumer-defined metadata.
    pub metadata: HashMap<String, String>,
}

impl Edge {
    /// Construct an edge from its authoritative conductance reservoir, deriving the
    /// bounded `weight` projection so the ADR-0002 invariant `weight =
    /// project_weight(conductance)` holds by construction.
    ///
    /// Conductance is never authored as a public control knob (conductance.md): the
    /// caller supplies the seeded log-LR reservoir (e.g. a cold-start coupling) and
    /// the projection is computed here, never independently. Every engine edge-creation
    /// site routes through this so `weight` can never diverge from `conductance`.
    #[allow(clippy::too_many_arguments)]
    pub fn seeded(
        id: EdgeId,
        source: NodeId,
        target: NodeId,
        edge_type: EdgeType,
        conductance: f64,
        edge_source: EdgeSource,
        created_at: Timestamp,
        accessed_at: Timestamp,
        metadata: HashMap<String, String>,
    ) -> Self {
        Edge {
            id,
            source,
            target,
            edge_type,
            weight: crate::mechanics::priors::project_weight(conductance),
            conductance,
            edge_source,
            created_at,
            accessed_at,
            valid_from: None,
            valid_until: None,
            metadata,
        }
    }
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
            conductance: 0.0,
            edge_source: EdgeSource::Manual,
            created_at: Timestamp(1000),
            accessed_at: Timestamp(1000),
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
            conductance: 0.0,
            edge_source: EdgeSource::Auto,
            created_at: Timestamp(500),
            accessed_at: Timestamp(500),
            valid_from: None,
            valid_until: None,
            metadata: HashMap::new(),
        };
        assert_eq!(edge.edge_type, EdgeType::Contradicts);
    }
}
