//! Core type primitives for the Anamnesis graph engine.

/// Unique identifier for a graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

/// Unique identifier for a graph edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub u64);

/// Unix timestamp in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// Returns the current system time as milliseconds since Unix epoch.
    pub fn now() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Timestamp(millis)
    }
}

/// Knowledge type taxonomy — determines decay rate, mass prior, and physics behavior.
///
/// Three classes of matter:
/// - Identity (Star): high mass, low/no decay
/// - Knowledge (Planet): medium mass, moderate decay  
/// - Memory (Dust): low mass, fast decay
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KnowledgeType {
    // Identity (Star — high mass, low/no decay)
    /// L0: Immutable core trait. No decay. ("I am a code architect")
    IdentityCore,
    /// L1: Experience-formed trait. Very slow decay. ("prefers factory pattern")
    IdentityLearned,
    /// L2: Current state. Normal decay. ("refactoring auth module")
    IdentityState,

    // Knowledge (Planet — medium mass, moderate decay)
    /// Extracted fact from conversation or document.
    Semantic,
    /// How-to or execution pattern.
    Procedural,
    /// Named concept, module, person, or service.
    Entity,
    /// Project rule or convention.
    Convention,
    /// Decision with rationale.
    Decision,
    /// Pitfall or warning.
    Gotcha,

    // Memory (Dust — low mass, fast decay)
    /// Raw conversation turn or session text.
    Episodic,
    /// Time-bound occurrence or event.
    Event,

    /// Consumer-defined type.
    Custom(String),
}

/// Edge type — determines propagation multiplier (kappa) during spreading activation.
///
/// Supportive edges propagate activation. Contradicts is inhibitory (applies repulsion).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EdgeType {
    // Supportive (propagation)
    /// Conceptual relationship. kappa = 1.00
    Semantic,
    /// Cause-effect relationship. kappa = 1.00
    Causal,
    /// Temporal sequence. kappa = 0.85
    Temporal,
    /// Decision rationale. kappa = 1.15
    Reason,
    /// Repeated confirmation. kappa = 1.10
    ReinforcedBy,
    /// Derived from multiple fragments. kappa = 1.00
    ConsolidatedFrom,
    /// Derived knowledge to source episode. kappa = 1.00
    ExtractedFrom,
    /// Shared entity link across agents. kappa = 0.95
    Entity,
    /// Replaces outdated knowledge. kappa = 1.20 toward new, 0.40 toward old.
    Supersedes,
    /// Considered and discarded option. kappa = 0.60
    RejectedAlternative,

    // Inhibitory
    /// Conflicting assertions. Excluded from propagation; applies repulsion instead.
    Contradicts,

    /// Consumer-defined edge type.
    Custom(String),
}

impl EdgeType {
    /// Returns the propagation multiplier (kappa) for this edge type during spreading activation.
    ///
    /// `is_forward`: true when traversing source→target, false when traversing target→source.
    /// Only `Supersedes` has different kappa values by direction.
    pub fn kappa(&self, is_forward: bool) -> f64 {
        match self {
            EdgeType::Supersedes => {
                if is_forward {
                    1.20
                } else {
                    0.40
                }
            }
            EdgeType::Reason => 1.15,
            EdgeType::ReinforcedBy => 1.10,
            EdgeType::Semantic => 1.00,
            EdgeType::Causal => 1.00,
            EdgeType::ConsolidatedFrom => 1.00,
            EdgeType::ExtractedFrom => 1.00,
            EdgeType::Entity => 0.95,
            EdgeType::Temporal => 0.85,
            EdgeType::RejectedAlternative => 0.60,
            EdgeType::Contradicts => 0.00,
            EdgeType::Custom(_) => 1.00,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_copy() {
        let id = NodeId(42);
        let id2 = id; // Copy
        assert_eq!(id, id2);
    }

    #[test]
    fn edge_id_is_copy() {
        let id = EdgeId(7);
        let id2 = id;
        assert_eq!(id, id2);
    }

    #[test]
    fn timestamp_ordering() {
        let t1 = Timestamp(100);
        let t2 = Timestamp(200);
        assert!(t1 < t2);
    }

    #[test]
    fn all_knowledge_types_constructable() {
        let types = vec![
            KnowledgeType::IdentityCore,
            KnowledgeType::IdentityLearned,
            KnowledgeType::IdentityState,
            KnowledgeType::Semantic,
            KnowledgeType::Procedural,
            KnowledgeType::Entity,
            KnowledgeType::Convention,
            KnowledgeType::Decision,
            KnowledgeType::Gotcha,
            KnowledgeType::Episodic,
            KnowledgeType::Event,
            KnowledgeType::Custom("my-type".to_string()),
        ];
        assert_eq!(types.len(), 12);
    }

    #[test]
    fn kappa_values_match_architecture() {
        assert_eq!(EdgeType::Reason.kappa(true), 1.15);
        assert_eq!(EdgeType::Supersedes.kappa(true), 1.20);
        assert_eq!(EdgeType::Supersedes.kappa(false), 0.40);
        assert_eq!(EdgeType::Contradicts.kappa(true), 0.00);
        assert_eq!(EdgeType::Semantic.kappa(true), 1.00);
        assert_eq!(EdgeType::Temporal.kappa(true), 0.85);
        assert_eq!(EdgeType::RejectedAlternative.kappa(true), 0.60);
    }

    #[test]
    fn all_edge_types_constructable() {
        let types = vec![
            EdgeType::Semantic,
            EdgeType::Causal,
            EdgeType::Temporal,
            EdgeType::Reason,
            EdgeType::ReinforcedBy,
            EdgeType::ConsolidatedFrom,
            EdgeType::ExtractedFrom,
            EdgeType::Entity,
            EdgeType::Supersedes,
            EdgeType::RejectedAlternative,
            EdgeType::Contradicts,
            EdgeType::Custom("my-edge".to_string()),
        ];
        assert_eq!(types.len(), 12);
    }
}
