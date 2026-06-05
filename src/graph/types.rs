//! Core type primitives for the Anamnesis graph engine.

/// Unique identifier for a graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

/// Unique identifier for a graph edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub u64);

/// Unique identifier for a peer (human or agent) in the registry.
///
/// Newtypes over `u64` — same pattern as `NodeId` and `EdgeId`.
/// Type-safe: a `PeerId` cannot be used where a `NodeId` is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub u64);

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
    /// Unverified claim or proposed explanation awaiting evidence.
    Hypothesis,
    /// Supporting or refuting observation gathered during investigation.
    Evidence,
    /// Debugging session or investigation trace that should remain inert.
    DebugSession,

    // Memory (Dust — low mass, fast decay)
    /// Raw conversation turn or session text.
    Episodic,
    /// Time-bound occurrence or event.
    Event,

    /// Consumer-defined type.
    Custom(String),
}

/// Explicit memory tier override for a node.
///
/// When set to anything other than `Auto`, the tier overrides the natural
/// salience-based tier assignment. `Core` nodes are protected from decay.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum MemoryTier {
    /// No override — tier is determined by salience range (default).
    #[default]
    Auto,
    /// Pinned to core memory — protected from decay.
    Core,
    /// Pinned to recall tier — moderate decay.
    Recall,
    /// Pinned to archival tier — fast decay.
    Archival,
}

/// Edge type — determines the within-row propagation factor during spreading
/// activation. The factor itself is the calibrated `edge_type_factor` prior
/// ([`crate::mechanics::priors::edge_type_factor`]); the type only names the
/// relation.
///
/// Supportive edges propagate activation. `Contradicts` is excluded from propagation
/// and instead surfaces query-local frustration stress between its active endpoints
/// (frustration.md / ADR-0006) — it is never inhibitory damping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EdgeType {
    // Supportive (propagation)
    /// Conceptual relationship.
    Semantic,
    /// Cause-effect relationship.
    Causal,
    /// Temporal sequence.
    Temporal,
    /// Decision rationale.
    Reason,
    /// Repeated confirmation.
    ReinforcedBy,
    /// Derived from multiple fragments.
    ConsolidatedFrom,
    /// Derived knowledge to source episode.
    ExtractedFrom,
    /// Shared entity link across agents.
    Entity,
    /// Replaces outdated knowledge (directional: strong toward new, weak toward old).
    Supersedes,
    /// Considered and discarded option.
    RejectedAlternative,
    /// Positive evidential support.
    Supports,
    /// Refuting evidence. Weak supportive propagation, not inhibitory.
    Refutes,
    /// Hierarchical or containment relationship.
    BelongsTo,

    // Constraint (excluded from propagation; surfaces frustration stress)
    /// Conflicting assertions. Excluded from propagation; surfaces query-local
    /// frustration stress when both endpoints are active (ADR-0006), never deleted.
    Contradicts,

    /// Consumer-defined edge type.
    Custom(String),
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
            KnowledgeType::Hypothesis,
            KnowledgeType::Evidence,
            KnowledgeType::DebugSession,
            KnowledgeType::Episodic,
            KnowledgeType::Event,
            KnowledgeType::Custom("my-type".to_string()),
        ];
        assert_eq!(types.len(), 15);
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
            EdgeType::Supports,
            EdgeType::Refutes,
            EdgeType::BelongsTo,
            EdgeType::Contradicts,
            EdgeType::Custom("my-edge".to_string()),
        ];
        assert_eq!(types.len(), 15);
    }
}
