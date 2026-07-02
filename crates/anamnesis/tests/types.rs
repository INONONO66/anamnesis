//! Exhaustive type construction tests.
//!
//! Verifies all enum variants and struct fields can be constructed.

use anamnesis::Error;
use anamnesis::engine::{EdgeId, EdgeType, KnowledgeType, NodeId, Query, Timestamp};
use anamnesis::graph::node::Origin;
use anamnesis::query::{ContextPackage, TokenBudget};

#[test]
fn all_knowledge_types() {
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
fn all_edge_types() {
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

#[test]
fn all_query_variants() {
    let queries = [
        Query::Associative {
            seed: NodeId(1),
            budget: 100,
        },
        Query::TypeFiltered {
            node_type: KnowledgeType::Convention,
            limit: 10,
        },
        Query::Neighborhood {
            entity: NodeId(2),
            depth: 3,
        },
        Query::Temporal {
            since: Timestamp(1000),
            node_types: Some(vec![KnowledgeType::Decision]),
            limit: 20,
        },
        Query::List {
            min_salience: 0.5,
            limit: 50,
        },
    ];
    assert_eq!(queries.len(), 5);
}

#[test]
fn all_error_variants() {
    let errors: Vec<Error> = vec![
        Error::NodeNotFound(NodeId(1)),
        Error::EdgeNotFound(EdgeId(2)),
        Error::StorageError("disk full".to_string()),
        Error::Rejected("low novelty".to_string()),
        Error::InvalidConfig("bad value".to_string()),
        Error::BudgetExhausted,
    ];
    assert_eq!(errors.len(), 6);
    assert!(errors[0].to_string().contains("node not found"));
    assert!(errors[5].to_string().contains("budget exhausted"));
}

#[test]
fn origin_universal_and_scoped() {
    let universal = Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::engine::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.8,
    };
    assert!(universal.scope.is_universal());

    let scoped = Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::engine::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: anamnesis::graph::ScopePath::new("anamnesis").expect("valid scope"),
        confidence: 0.9,
    };
    assert_eq!(scoped.scope.as_str(), "anamnesis");
}

#[test]
fn context_package_empty() {
    let pkg = ContextPackage::empty();
    assert_eq!(pkg.total_fragments(), 0);
    assert!(pkg.identity.is_empty());
    assert!(pkg.knowledge.is_empty());
    assert!(pkg.memories.is_empty());
    assert!(pkg.tensions.is_empty());
    assert_eq!(pkg.agent_tension, 0.0);
}

#[test]
fn token_budget_arithmetic() {
    let mut budget = TokenBudget::new(4000);
    assert_eq!(budget.remaining(), 4000);
    budget.used = 1500;
    assert_eq!(budget.remaining(), 2500);
    budget.used = 5000;
    assert_eq!(budget.remaining(), 0);
}

#[test]
fn newtypes_are_copy() {
    let id = NodeId(42);
    let id2 = id;
    assert_eq!(id, id2);

    let eid = EdgeId(7);
    let eid2 = eid;
    assert_eq!(eid, eid2);

    let ts = Timestamp(100);
    let ts2 = ts;
    assert_eq!(ts, ts2);
}

#[test]
fn timestamp_ordering() {
    assert!(Timestamp(100) < Timestamp(200));
    assert!(Timestamp(0) < Timestamp(1));
}
