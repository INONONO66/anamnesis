//! Node and Origin types for the Anamnesis graph.

use crate::graph::scope::ScopePath;
use crate::graph::types::{
    AccessTrace, KnowledgeType, MemoryTier, NodeId, PeerId, SourceKind, Timestamp,
};
use std::collections::{HashMap, VecDeque};

/// Provenance and scope of a knowledge fragment.
///
/// Tracks which peer produced this node, from which session,
/// and the hierarchical scope path it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    /// The peer (human or agent) that produced this knowledge fragment.
    pub peer_id: PeerId,
    /// The kind of source that produced this fragment.
    pub source_kind: SourceKind,
    /// The session in which this fragment was created.
    pub session_id: String,
    /// Hierarchical scope path. `ScopePath::universal()` = applies across all scopes.
    pub scope: ScopePath,
    /// Creation-time certainty [0, 1].
    pub confidence: f64,
}

impl Origin {
    /// Convenience constructor for tests.
    ///
    /// Uses `SourceKind::AgentObservation` and `confidence = 0.9`.
    pub fn test_default(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            source_kind: SourceKind::AgentObservation,
            session_id: "test-session".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        }
    }
}

/// A knowledge fragment in the cognitive graph.
///
/// Nodes carry multi-resolution content (L0/L1/L2), memory-strength state
/// (`retained_action` and its `salience` projection), provenance (origin), and
/// classification (node_type, entity_tags).
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Unique identifier.
    pub id: NodeId,
    /// Knowledge type — *policy* input to the decay and coupling priors (it scales
    /// the single decay prior `d` via `decay_multiplier_for_type` and supplies the
    /// edge-type-affinity coupling feature); it is not an independent dynamics knob.
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

    // Memory-strength state
    /// Salience score [0, 1] — the bounded logistic projection of the composite
    /// retained action, `salience = logistic(B_i + P_i)` (ADR-0008). It is a CACHED
    /// read-only view: only `commit`/`touch`/`tick` *refresh* it (recomputing
    /// `B_i(now)` and adding `P_i`); read-only query/search must not recompute it.
    pub salience: f64,
    /// Retained action `A_i = B_i + P_i` — the composite log need-odds strength.
    /// `B_i` is the multi-trace ACT-R base level computed on demand from
    /// `access_history` (NOT stored); `P_i` is the stored `evidence_prior`. This
    /// field is a CACHED snapshot of the composite, refreshed only on
    /// commit/touch/tick at that event's `now`; read paths return it unchanged
    /// (ADR-0008).
    pub retained_action: f64,
    /// Evidence prior `P_i` — a persistent, decay-EXEMPT log-odds offset holding
    /// encoding surprise (`P_i ← k·eps` at allocation), feedback / social
    /// reinforcement (`dP_i = eta·(lambda − predicted)`), and peer trust. It does
    /// NOT undergo base-level decay (ADR-0008 / ADR-0009).
    pub evidence_prior: f64,
    /// Number of times this node has been accessed via touch().
    pub access_count: u32,
    /// Bounded 32-trace access-history window (a creation trace plus each committed
    /// access). Each [`AccessTrace`] carries its own activation-dependent decay
    /// `d_j` (Pavlik & Anderson 2005), so it is the load-bearing input to the base
    /// level `B_i = ln(Σ_j (now − at_j)^(−d_j))`; oldest traces drop when full.
    pub access_history: VecDeque<AccessTrace>,
    /// Explicit memory tier override for salience-based tiering.
    pub tier: MemoryTier,

    // Provenance
    /// Who created this node, from which session, and with what confidence.
    pub origin: Origin,

    // Classification
    /// Entity tags for automatic cross-node linking. Nodes sharing tags get Entity edges.
    pub entity_tags: Vec<String>,
    /// Consumer-defined metadata key-value pairs.
    pub metadata: HashMap<String, String>,
}

impl Node {
    /// Record an access trace. Maintains a ring buffer capped at 32 entries.
    ///
    /// The trace carries its own pre-computed per-trace decay `d_j`; the caller is
    /// responsible for computing it from the existing history at the access moment
    /// ([`crate::mechanics::forgetting::compute_trace_decay`]) before pushing.
    pub fn record_access(&mut self, trace: AccessTrace) {
        self.access_history.push_back(trace);
        if self.access_history.len() > 32 {
            self.access_history.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{KnowledgeType, NodeId, Timestamp};

    fn make_origin() -> Origin {
        Origin {
            peer_id: crate::graph::types::PeerId(0),
            source_kind: crate::graph::types::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: ScopePath::new("anamnesis").expect("valid scope"),
            confidence: 0.9,
        }
    }

    #[test]
    fn origin_universal() {
        let o = Origin {
            peer_id: crate::graph::types::PeerId(0),
            source_kind: crate::graph::types::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.8,
        };
        assert!(o.scope.is_universal());
    }

    #[test]
    fn origin_project_scoped() {
        let o = make_origin();
        assert_eq!(o.scope.as_str(), "anamnesis");
    }

    #[test]
    fn node_all_fields() {
        let node = Node {
            id: NodeId(1),
            node_type: KnowledgeType::Semantic,
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
            retained_action: 0.0,
            evidence_prior: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
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
            retained_action: 0.0,
            evidence_prior: 0.0,
            access_count: 0,
            access_history: VecDeque::new(),
            tier: MemoryTier::Auto,
            origin: Origin {
                peer_id: crate::graph::types::PeerId(0),
                source_kind: crate::graph::types::SourceKind::AgentObservation,
                session_id: "session-2".to_string(),
                scope: ScopePath::universal(),
                confidence: 0.7,
            },
            entity_tags: vec![],
            metadata: HashMap::new(),
        };
        assert!(node.summary.is_none());
        assert!(node.embedding.is_none());
    }
}
