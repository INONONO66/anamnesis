//! Tests for Observation.valid_from/valid_until + ingest() (T14).

use anamnesis::Engine;
use anamnesis::api::{IngestResult, Observation};
use anamnesis::engine::EngineConfig;
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::peer::SourceKind;
use anamnesis::query::Query;

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

#[test]
fn fact_at_filters_by_valid_from_valid_until() {
    use anamnesis::query::QueryConfig;
    let mut e = engine();
    let t1 = Timestamp(1_000_000);
    let t2 = Timestamp(2_000_000);
    let t3 = Timestamp(3_000_000);

    // Node valid from t1 to t2
    let IngestResult::Created(ids) = e
        .ingest(obs_with_validity("time-bounded", Some(t1), Some(t2)))
        .unwrap()
    else {
        panic!("expected Created");
    };

    // fact_at(t1) -> should find the node
    let q = Query::List {
        min_salience: 0.0,
        limit: 10,
    };
    let config = QueryConfig::default();
    let pkg_at_t1 = e.fact_at(&q, t1).unwrap();
    let found_at_t1 = pkg_at_t1
        .knowledge
        .iter()
        .chain(pkg_at_t1.memories.iter())
        .any(|f| f.node_id == ids[0]);
    assert!(found_at_t1, "node should be found at t1");

    // fact_at(t3) -> should NOT find the node (expired)
    let pkg_at_t3 = e.fact_at(&q, t3).unwrap();
    let found_at_t3 = pkg_at_t3
        .knowledge
        .iter()
        .chain(pkg_at_t3.memories.iter())
        .any(|f| f.node_id == ids[0]);
    assert!(
        !found_at_t3,
        "node should not be found at t3 (after valid_until)"
    );

    let _ = config;
}
