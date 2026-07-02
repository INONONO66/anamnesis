//! Tests for Engine::link endpoint validation and cold-start conductance seeding.
//!
//! `link` no longer accepts a caller-supplied weight: conductance is never set
//! directly (conductance.md). It seeds the conductance reservoir from the
//! cold-start coupling and derives the bounded `weight` projection (ADR-0002).

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::IngestResult;
use anamnesis::error::Error;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};

fn setup_engine_with_nodes() -> (Engine, NodeId, NodeId) {
    let mut engine = Engine::new();

    let origin = Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::engine::SourceKind::AgentObservation,
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
        valid_from: None,
        valid_until: None,
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
        valid_from: None,
        valid_until: None,
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

    let result = engine.link(missing_source, node2, EdgeType::Semantic);

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

    let result = engine.link(node1, missing_target, EdgeType::Semantic);

    assert!(result.is_err());
    match result {
        Err(Error::NodeNotFound(id)) => {
            assert_eq!(id, missing_target);
        }
        _ => panic!("expected NodeNotFound error"),
    }
}

#[test]
fn link_seeds_finite_conductance_and_bounded_weight() {
    let (mut engine, node1, node2) = setup_engine_with_nodes();

    let edge_id = engine
        .link(node1, node2, EdgeType::Semantic)
        .expect("link should succeed");

    let edge = engine.graph().get_edge(edge_id).expect("edge should exist");
    // Conductance is the cold-start log-LR prior — always finite.
    assert!(
        edge.conductance.is_finite(),
        "seeded conductance must be finite, got {}",
        edge.conductance
    );
    // The public weight is the bounded projection of the reservoir (ADR-0002).
    assert!(
        (0.0..=1.0).contains(&edge.weight),
        "weight projection must stay in [0, 1], got {}",
        edge.weight
    );
    // weight == project_weight(conductance) == logistic(conductance).
    let expected = 1.0 / (1.0 + (-edge.conductance).exp());
    assert!(
        (edge.weight - expected).abs() < 1e-9,
        "weight must be the logistic projection of conductance"
    );
}

#[test]
fn link_seed_grows_with_endpoint_coupling() {
    // Two endpoints sharing entity tags and embedding should seed a higher
    // conductance than a bare pair — the cold-start coupling reflects features.
    let mut engine = Engine::new();
    let origin = Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::engine::SourceKind::AgentObservation,
        session_id: "test-session".into(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.9,
    };
    let mk = |name: &str, tags: Vec<String>, emb: Option<Vec<f64>>| Observation {
        name: name.into(),
        summary: None,
        content: format!("{name} content"),
        embedding: emb,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags,
        origin: origin.clone(),
        timestamp: Timestamp::now(),
        valid_from: None,
        valid_until: None,
    };
    let first = |r: IngestResult| match r {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    };

    let bare_a = first(engine.ingest(mk("bare-a", vec![], None)).unwrap());
    let bare_b = first(engine.ingest(mk("bare-b", vec![], None)).unwrap());
    let rich_a = first(
        engine
            .ingest(mk(
                "rich-a",
                vec!["shared".into()],
                Some(vec![1.0, 0.0, 0.0]),
            ))
            .unwrap(),
    );
    let rich_b = first(
        engine
            .ingest(mk(
                "rich-b",
                vec!["shared".into()],
                Some(vec![1.0, 0.0, 0.0]),
            ))
            .unwrap(),
    );

    let bare_edge = engine.link(bare_a, bare_b, EdgeType::Semantic).unwrap();
    let rich_edge = engine.link(rich_a, rich_b, EdgeType::Semantic).unwrap();
    let bare_c = engine.graph().get_edge(bare_edge).unwrap().conductance;
    let rich_c = engine.graph().get_edge(rich_edge).unwrap().conductance;

    assert!(
        rich_c > bare_c,
        "shared-feature endpoints must seed stronger conductance: rich {rich_c} <= bare {bare_c}"
    );
}
