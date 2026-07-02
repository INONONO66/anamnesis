use anamnesis::api::Observation;
use anamnesis::engine::{CrystallizeRequest, CrystallizeResult, IngestResult, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::{Engine, Error};

fn origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::engine::SourceKind::AgentObservation,
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

/// Inserts a source and sets its retained-action reservoir directly.
///
/// `set_retained_action` writes the authoritative `A_i` and re-projects salience,
/// so the reservoir and its projection stay coherent — crystallization synthesizes
/// from the reservoir (ADR-0002).
fn insert_source(engine: &mut Engine, name: &str, retained_action: f64) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name, Timestamp(1000))).unwrap()
    else {
        panic!("expected source creation");
    };
    let id = ids[0];
    engine
        .graph_mut()
        .storage_mut()
        .set_retained_action(id, retained_action)
        .unwrap();
    id
}

/// `project_salience(A) = logistic(A)`.
fn project_salience(a: f64) -> f64 {
    1.0 / (1.0 + (-a).exp())
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
fn crystallize_synthesizes_additively_without_mutating_sources() {
    let mut engine = Engine::new();
    // Sources with explicit retained-action reservoirs (log need-odds).
    let action_a = 1.0;
    let action_b = 3.0;
    let source_a = insert_source(&mut engine, "source-a", action_a);
    let source_b = insert_source(&mut engine, "source-b", action_b);

    // Capture full source state to prove crystallize never mutates a source.
    let before_a = engine.graph().get_node(source_a).unwrap().clone();
    let before_b = engine.graph().get_node(source_b).unwrap().clone();

    let result = engine
        .crystallize(crystallize_request(
            vec![source_a, source_b],
            Some(vec![1.0, 0.0]),
        ))
        .unwrap();

    // A_s_new = relevance-weighted average of source retained_action, plus the
    // bounded confidence offset (2*conf - 1) * REWARD_LOG_ODDS_SCALE.
    // relevances = [0.25, 0.75], confidence = 0.8, REWARD_LOG_ODDS_SCALE = 4.0.
    let weighted_avg = (0.25 * action_a + 0.75 * action_b) / (0.25 + 0.75);
    let expected_action = weighted_avg + (2.0 * 0.8 - 1.0) * 4.0;
    let expected_salience = project_salience(expected_action);

    assert!((result.initial_salience - expected_salience).abs() < 1e-9);
    assert_eq!(result.dedup_score, 0.0);
    assert_eq!(result.consolidation_edges.len(), 2);
    // Crystallize is non-destructive: no source is reinforced.
    assert_eq!(result.nodes_reinforced, 0);
    assert_eq!(engine.graph().node_count(), 3);

    let crystal = engine.graph().get_node(result.node_id).unwrap();
    assert_eq!(crystal.name, "auth synthesis");
    assert_eq!(crystal.node_type, KnowledgeType::Semantic);
    assert!((crystal.salience - expected_salience).abs() < 1e-9);
    assert!((crystal.retained_action - expected_action).abs() < 1e-9);

    let edge_a = engine
        .graph()
        .get_edge(result.consolidation_edges[0])
        .unwrap();
    assert_eq!(edge_a.source, result.node_id);
    assert_eq!(edge_a.target, source_a);
    assert_eq!(edge_a.edge_type, EdgeType::ConsolidatedFrom);

    let edge_b = engine
        .graph()
        .get_edge(result.consolidation_edges[1])
        .unwrap();
    assert_eq!(edge_b.source, result.node_id);
    assert_eq!(edge_b.target, source_b);
    assert_eq!(edge_b.edge_type, EdgeType::ConsolidatedFrom);

    // Sources are untouched: reservoirs, salience, access count, timestamps.
    let after_a = engine.graph().get_node(source_a).unwrap();
    let after_b = engine.graph().get_node(source_b).unwrap();
    assert_eq!(after_a.retained_action, before_a.retained_action);
    assert_eq!(after_a.salience, before_a.salience);
    assert_eq!(after_a.access_count, before_a.access_count);
    assert_eq!(after_a.accessed_at, before_a.accessed_at);
    assert_eq!(after_b.retained_action, before_b.retained_action);
    assert_eq!(after_b.salience, before_b.salience);
    assert_eq!(after_b.access_count, before_b.access_count);
    assert_eq!(after_b.accessed_at, before_b.accessed_at);
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
