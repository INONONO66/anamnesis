//! Edge-validity helper shared by the read-only retrieval pipeline.
//!
//! The legacy priority-queue BFS spreading model (`spread_activation*`), the
//! eq-10 initial-activation kernel, salience gating, and fan-out normalization
//! have been removed (Phase 3): retrieval is now a single additive directed
//! Random-Walk-with-Restart over conductance (see [`crate::query::rwr`]).

use crate::graph::{Edge, Timestamp};

/// Returns whether an edge is valid at a domain timestamp.
///
/// Edges without validity bounds are always valid.
pub fn edge_valid_at(edge: &Edge, as_of: Timestamp) -> bool {
    crate::graph::valid_at(edge.valid_from, edge.valid_until, as_of)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::edge::EdgeSource;
    use crate::graph::{Edge, EdgeId, EdgeType, NodeId};
    use std::collections::HashMap;

    fn edge(valid_from: Option<Timestamp>, valid_until: Option<Timestamp>) -> Edge {
        Edge {
            id: EdgeId(0),
            source: NodeId(0),
            target: NodeId(1),
            edge_type: EdgeType::Semantic,
            weight: 0.5,
            conductance: 0.0,
            edge_source: EdgeSource::Auto,
            created_at: Timestamp(0),
            accessed_at: Timestamp(0),
            leaked_at: Timestamp(0),
            valid_from,
            valid_until,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn unbounded_edge_always_valid() {
        assert!(edge_valid_at(&edge(None, None), Timestamp(100)));
    }

    #[test]
    fn half_open_interval_respected() {
        let e = edge(Some(Timestamp(10)), Some(Timestamp(20)));
        assert!(!edge_valid_at(&e, Timestamp(9)));
        assert!(edge_valid_at(&e, Timestamp(10)));
        assert!(edge_valid_at(&e, Timestamp(19)));
        assert!(!edge_valid_at(&e, Timestamp(20)));
    }
}
