//! Tests for Engine::link endpoint validation and weight clamping.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::error::Error;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};

fn setup_engine_with_nodes() -> (Engine, NodeId, NodeId) {
    let mut engine = Engine::new();

    let origin = Origin {
        agent_id: "test-agent".into(),
        session_id: "test-session".into(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.9,
    };

    let obs1 = Observation {
        name: "node1".into(),
        summary: None,
        content: "first node".into(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: origin.clone(),
        timestamp: Timestamp::now(),
    };

    let obs2 = Observation {
        name: "node2".into(),
        summary: None,
        content: "second node".into(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: origin.clone(),
        timestamp: Timestamp::now(),
    };

    let result1 = engine.ingest(obs1).expect("ingest node1");
    let result2 = engine.ingest(obs2).expect("ingest node2");

    let node1 = match result1 {
        anamnesis::api::IngestResult::Created(ids) => ids[0],
        anamnesis::api::IngestResult::Reinforced { existing_id, .. } => existing_id,
    };

    let node2 = match result2 {
        anamnesis::api::IngestResult::Created(ids) => ids[0],
        anamnesis::api::IngestResult::Reinforced { existing_id, .. } => existing_id,
    };

    (engine, node1, node2)
}

#[test]
fn link_missing_source_returns_err() {
    let (mut engine, _, node2) = setup_engine_with_nodes();
    let missing_source = NodeId(9999);

    let result = engine.link(missing_source, node2, EdgeType::Semantic, 0.5);

    assert!(result.is_err());
    match result {
        Err(Error::NodeNotFound(id)) => {
            assert_eq!(id, missing_source);
        }
        _ => panic!("expected NodeNotFound error"),
    }
}

#[test]
fn link_missing_target_returns_err() {
    let (mut engine, node1, _) = setup_engine_with_nodes();
    let missing_target = NodeId(9999);

    let result = engine.link(node1, missing_target, EdgeType::Semantic, 0.5);

    assert!(result.is_err());
    match result {
        Err(Error::NodeNotFound(id)) => {
            assert_eq!(id, missing_target);
        }
        _ => panic!("expected NodeNotFound error"),
    }
}

#[test]
fn link_weight_nan_returns_err() {
    let (mut engine, node1, node2) = setup_engine_with_nodes();
    let nan_weight = f64::NAN;

    let result = engine.link(node1, node2, EdgeType::Semantic, nan_weight);

    assert!(result.is_err());
    match result {
        Err(Error::InvalidInput(msg)) => {
            assert!(msg.contains("finite"));
        }
        _ => panic!("expected InvalidInput error with 'finite' message"),
    }
}

#[test]
fn link_weight_negative_clamped_to_zero() {
    let (mut engine, node1, node2) = setup_engine_with_nodes();
    let negative_weight = -0.5;

    let edge_id = engine
        .link(node1, node2, EdgeType::Semantic, negative_weight)
        .expect("link should succeed with clamping");

    let edge = engine.graph().get_edge(edge_id).expect("edge should exist");
    assert_eq!(edge.weight, 0.0, "negative weight should be clamped to 0.0");
}

#[test]
fn link_weight_above_one_clamped_to_one() {
    let (mut engine, node1, node2) = setup_engine_with_nodes();
    let above_one_weight = 1.5;

    let edge_id = engine
        .link(node1, node2, EdgeType::Semantic, above_one_weight)
        .expect("link should succeed with clamping");

    let edge = engine.graph().get_edge(edge_id).expect("edge should exist");
    assert_eq!(
        edge.weight, 1.0,
        "weight above 1.0 should be clamped to 1.0"
    );
}
