use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult, Node};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{AccessTrace, KnowledgeType, MemoryTier, NodeId, Timestamp};
use std::collections::{HashMap, VecDeque};

/// A test access trace at `at` with an arbitrary (non-load-bearing) decay.
fn trace(at: u64) -> AccessTrace {
    AccessTrace {
        at: Timestamp(at),
        decay: 0.16,
    }
}

fn make_test_node() -> Node {
    Node {
        id: NodeId(0),
        node_type: KnowledgeType::Semantic,
        name: "test".to_string(),
        summary: None,
        content: "test content".to_string(),
        embedding: None,
        created_at: Timestamp(0),
        updated_at: Timestamp(0),
        accessed_at: Timestamp(0),
        valid_from: None,
        valid_until: None,
        salience: 1.0,
        retained_action: 0.0,
        evidence_prior: 0.0,
        access_count: 0,
        access_history: VecDeque::new(),
        tier: MemoryTier::Auto,
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 1.0,
        },
        entity_tags: vec![],
        metadata: HashMap::new(),
    }
}

#[test]
fn access_history_starts_empty() {
    // A hand-built Node literal carries no traces until one is recorded; the engine
    // seeds the creation trace at ingest (see `ingest_seeds_creation_trace`).
    let node = make_test_node();
    assert_eq!(node.access_history.len(), 0);
}

#[test]
fn ingest_seeds_creation_trace() {
    // The creation event IS a trace (ADR-0008): a freshly ingested node carries
    // exactly one access trace, stamped at created_at, so its base level B_i is
    // finite at birth (compute_base_level returns NEG_INFINITY on empty history).
    let mut engine = Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    );
    let obs = Observation {
        name: "seeded".to_string(),
        summary: None,
        content: "creation trace".to_string(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "s".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(4242),
        valid_from: None,
        valid_until: None,
    };
    let IngestResult::Created(ids) = engine.ingest(obs).unwrap() else {
        panic!("expected Created");
    };
    let node = engine.graph().get_node(ids[0]).unwrap();
    assert_eq!(node.access_history.len(), 1, "creation trace seeded");
    assert_eq!(node.access_history.front().unwrap().at, Timestamp(4242));
    assert!(node.retained_action.is_finite());
}

#[test]
fn access_history_caps_at_32() {
    let mut node = make_test_node();
    for i in 0..35u64 {
        node.record_access(trace(i));
    }
    assert_eq!(node.access_history.len(), 32);
    assert!(
        node.access_history.front().unwrap().at.0 >= 3,
        "oldest entry should be at index 3 or later, got {}",
        node.access_history.front().unwrap().at.0
    );
}

#[test]
fn access_history_preserves_order() {
    let mut node = make_test_node();
    node.record_access(trace(10));
    node.record_access(trace(20));
    node.record_access(trace(30));
    assert_eq!(node.access_history.front().unwrap().at.0, 10);
    assert_eq!(node.access_history.back().unwrap().at.0, 30);
}
