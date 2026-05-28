use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::{
    CrystallizeRequest, CrystallizeResult, Engine, Error, IngestResult, StorageAdapter,
};

fn origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: anamnesis::graph::ScopePath::new("project-1").expect("valid scope"),
        confidence: 0.9,
    }
}

fn observation(name: &str, timestamp: Timestamp) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Episodic,
        entity_tags: vec!["auth".to_string()],
        origin: origin(),
        timestamp,
        valid_from: None,
        valid_until: None,
    }
}

fn insert_source(engine: &mut Engine, name: &str, salience: f64) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name, Timestamp(1000))).unwrap()
    else {
        panic!("expected source creation");
    };
    let id = ids[0];
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(id, salience)
        .unwrap();
    id
}

fn crystallize_request(source_ids: Vec<NodeId>, embedding: Option<Vec<f64>>) -> CrystallizeRequest {
    CrystallizeRequest {
        name: "auth synthesis".to_string(),
        summary: Some("Synthesized auth knowledge".to_string()),
        content: "The auth module combines repeated fragments into one pattern.".to_string(),
        embedding,
        source_ids,
        source_relevances: Some(vec![0.25, 0.75]),
        node_type: KnowledgeType::Semantic,
        confidence: 0.8,
        origin: origin(),
        entity_tags: vec!["auth".to_string()],
        timestamp: Timestamp(2000),
    }
}

#[test]
fn crystallize_creates_crystal_edges_and_reinforces_sources() {
    let mut engine = Engine::new();
    let source_a = insert_source(&mut engine, "source-a", 0.2);
    let source_b = insert_source(&mut engine, "source-b", 0.8);

    let result = engine
        .crystallize(crystallize_request(
            vec![source_a, source_b],
            Some(vec![1.0, 0.0]),
        ))
        .unwrap();

    let expected_salience = 0.60 * 0.65 + 0.25 * 0.8 + 0.15 * 0.10;
    assert!((result.initial_salience - expected_salience).abs() < 1e-10);
    assert_eq!(result.dedup_score, 0.0);
    assert_eq!(result.consolidation_edges.len(), 2);
    assert_eq!(result.nodes_reinforced, 2);
    assert_eq!(engine.graph().node_count(), 3);

    let crystal = engine.graph().get_node(result.node_id).unwrap();
    assert_eq!(crystal.name, "auth synthesis");
    assert_eq!(crystal.node_type, KnowledgeType::Semantic);
    assert!((crystal.salience - expected_salience).abs() < 1e-10);

    let edge_a = engine
        .graph()
        .get_edge(result.consolidation_edges[0])
        .unwrap();
    assert_eq!(edge_a.source, result.node_id);
    assert_eq!(edge_a.target, source_a);
    assert_eq!(edge_a.edge_type, EdgeType::ConsolidatedFrom);
    assert!((edge_a.weight - 0.25).abs() < 1e-10);

    let edge_b = engine
        .graph()
        .get_edge(result.consolidation_edges[1])
        .unwrap();
    assert_eq!(edge_b.source, result.node_id);
    assert_eq!(edge_b.target, source_b);
    assert_eq!(edge_b.edge_type, EdgeType::ConsolidatedFrom);
    assert!((edge_b.weight - 0.75).abs() < 1e-10);

    assert_eq!(engine.graph().get_node(source_a).unwrap().access_count, 1);
    assert_eq!(engine.graph().get_node(source_b).unwrap().access_count, 1);
}

#[test]
fn crystallize_returns_max_dedup_score_for_allowed_similarity() {
    let mut engine = Engine::new();
    let source_a = insert_source(&mut engine, "source-a", 0.7);
    let source_b = insert_source(&mut engine, "source-b", 0.9);

    let first: CrystallizeResult = engine
        .crystallize(crystallize_request(
            vec![source_a, source_b],
            Some(vec![1.0, 0.0]),
        ))
        .unwrap();
    assert_eq!(first.dedup_score, 0.0);

    let second = engine
        .crystallize(crystallize_request(
            vec![source_a, source_b],
            Some(vec![0.3, 0.954]),
        ))
        .unwrap();

    assert!(
        second.dedup_score > 0.29 && second.dedup_score < 0.31,
        "dedup score should track max non-duplicate similarity, got {}",
        second.dedup_score
    );
    assert_eq!(engine.graph().node_count(), 4);
}

#[test]
fn crystallize_rejects_duplicate_crystal_embedding() {
    let mut engine = Engine::new();
    let source_a = insert_source(&mut engine, "source-a", 0.7);
    let source_b = insert_source(&mut engine, "source-b", 0.9);

    let first = engine.crystallize(crystallize_request(
        vec![source_a, source_b],
        Some(vec![1.0, 0.0]),
    ));
    assert!(first.is_ok());
    assert_eq!(engine.graph().node_count(), 3);

    let duplicate = engine.crystallize(crystallize_request(
        vec![source_a, source_b],
        Some(vec![1.0, 0.001]),
    ));

    assert!(
        matches!(duplicate, Err(Error::Rejected(reason)) if reason.contains("duplicate crystallization"))
    );
    assert_eq!(engine.graph().node_count(), 3);
}

#[test]
fn crystallize_validates_minimum_sources() {
    let mut engine = Engine::new();
    let source = insert_source(&mut engine, "source", 0.5);

    let result = engine.crystallize(CrystallizeRequest {
        source_ids: vec![source],
        source_relevances: None,
        ..crystallize_request(vec![source, source], None)
    });

    assert!(matches!(result, Err(Error::InvalidInput(message)) if message.contains("at least 2")));
    assert_eq!(engine.graph().node_count(), 1);
}

#[test]
fn crystallize_validates_relevance_length() {
    let mut engine = Engine::new();
    let source_a = insert_source(&mut engine, "source-a", 0.5);
    let source_b = insert_source(&mut engine, "source-b", 0.6);

    let result = engine.crystallize(CrystallizeRequest {
        source_relevances: Some(vec![1.0]),
        ..crystallize_request(vec![source_a, source_b], None)
    });

    assert!(
        matches!(result, Err(Error::InvalidInput(message)) if message.contains("source_relevances"))
    );
    assert_eq!(engine.graph().node_count(), 2);
}

#[test]
fn crystallize_rejects_missing_source_without_writes() {
    let mut engine = Engine::new();
    let source = insert_source(&mut engine, "source", 0.5);

    let result = engine.crystallize(CrystallizeRequest {
        source_relevances: None,
        ..crystallize_request(vec![source, NodeId(999)], None)
    });

    assert!(matches!(result, Err(Error::NodeNotFound(NodeId(999)))));
    assert_eq!(engine.graph().node_count(), 1);
    assert_eq!(engine.graph().edge_count(), 0);
}
