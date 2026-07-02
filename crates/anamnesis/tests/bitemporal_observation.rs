//! Tests for Observation.valid_from/valid_until + ingest() (T14).

use anamnesis::Engine;
use anamnesis::api::{IngestResult, Observation};
use anamnesis::engine::EngineConfig;
use anamnesis::engine::SourceKind;
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};

fn obs_with_validity(
    name: &str,
    valid_from: Option<Timestamp>,
    valid_until: Option<Timestamp>,
) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::AgentObservation,
            session_id: "s1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp::now(),
        valid_from,
        valid_until,
    }
}

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

#[test]
fn valid_from_passed_to_node() {
    let mut e = engine();
    let ts = Timestamp(1_000_000);
    let IngestResult::Created(ids) = e
        .ingest(obs_with_validity("node-a", Some(ts), None))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let node = e.graph().get_node(ids[0]).unwrap();
    assert_eq!(node.valid_from, Some(ts));
    assert_eq!(node.valid_until, None);
}

#[test]
fn valid_until_passed_to_node() {
    let mut e = engine();
    let ts = Timestamp(2_000_000);
    let IngestResult::Created(ids) = e
        .ingest(obs_with_validity("node-b", None, Some(ts)))
        .unwrap()
    else {
        panic!("expected Created");
    };
    let node = e.graph().get_node(ids[0]).unwrap();
    assert_eq!(node.valid_from, None);
    assert_eq!(node.valid_until, Some(ts));
}

#[test]
fn none_valid_from_preserves_existing_behavior() {
    let mut e = engine();
    let IngestResult::Created(ids) = e.ingest(obs_with_validity("node-c", None, None)).unwrap()
    else {
        panic!("expected Created");
    };
    let node = e.graph().get_node(ids[0]).unwrap();
    assert_eq!(node.valid_from, None);
    assert_eq!(node.valid_until, None);
}

// NOTE: valid_from/valid_until temporal *filtering* at query time was exercised
// through the removed `Engine::fact_at` convenience API (0.10.0 shrink). The
// bitemporal *storage* invariants above (fields round-trip on ingest) are the
// surviving contract.
