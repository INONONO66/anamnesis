//! Tests for the IngestResult enum.
//!
//! Verifies that ingest() returns the correct IngestResult variant
//! and that the enum can be pattern-matched correctly.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::IngestResult;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};

fn make_obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {}", name),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

#[test]
fn ingest_returns_created_for_new_observation() {
    let mut e = Engine::new();
    let r = e.ingest(make_obs("a")).unwrap();
    assert!(matches!(r, IngestResult::Created(ref ids) if ids.len() == 1));
}

#[test]
fn ingest_result_created_variant_contains_node_id() {
    let mut e = Engine::new();
    let result = e.ingest(make_obs("test")).unwrap();

    let IngestResult::Created(ids) = result else {
        panic!("expected Created variant");
    };

    assert_eq!(ids.len(), 1);
}

#[test]
fn ingest_result_created_can_be_destructured() {
    let mut e = Engine::new();
    let IngestResult::Created(ids) = e.ingest(make_obs("destructure")).unwrap() else {
        panic!("expected Created");
    };

    assert_eq!(ids.len(), 1);
    let node_id = ids[0];
    let node = e.graph().get_node(node_id).unwrap();
    assert_eq!(node.name, "destructure");
}

#[test]
fn multiple_ingests_return_different_node_ids() {
    let mut e = Engine::new();

    let IngestResult::Created(ids1) = e.ingest(make_obs("first")).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = e.ingest(make_obs("second")).unwrap() else {
        panic!("expected Created");
    };

    assert_ne!(
        ids1[0], ids2[0],
        "different observations should get different node IDs"
    );
}

#[test]
fn ingest_result_created_ids_are_valid() {
    let mut e = Engine::new();
    let IngestResult::Created(ids) = e.ingest(make_obs("valid")).unwrap() else {
        panic!("expected Created");
    };

    let node_id = ids[0];
    let node = e.graph().get_node(node_id).expect("node should exist");
    assert_eq!(node.name, "valid");
    // Salience is the projection of the surprise-gated retained-action reservoir
    // (ADR-0009), not a flat 1.0. A no-embedding observation is maximally surprising
    // and enters just below the prior ceiling.
    assert!(
        node.salience > 0.999 && node.salience < 1.0,
        "salience should be a near-ceiling projection, got {}",
        node.salience
    );
}
