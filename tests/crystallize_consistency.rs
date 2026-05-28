use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::{CrystallizeRequest, Engine, IngestResult};

fn origin(_agent: &str, session: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session.to_string(),
        scope: ScopePath::new("project-1").expect("valid scope"),
        confidence: 0.9,
    }
}

fn insert_source(engine: &mut Engine, name: &str, agent: &str, session: &str) -> NodeId {
    let IngestResult::Created(ids) = engine
        .ingest(Observation {
            name: name.to_string(),
            summary: None,
            content: format!("Content for {name}"),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec!["test".to_string()],
            origin: origin(agent, session),
            timestamp: Timestamp(1000),
        })
        .unwrap()
    else {
        panic!("expected source creation");
    };
    ids[0]
}

fn crystal_request(sources: Vec<NodeId>, origin: Origin) -> CrystallizeRequest {
    CrystallizeRequest {
        name: "synthesis".to_string(),
        summary: None,
        content: "Synthesized content".to_string(),
        embedding: None,
        source_ids: sources.clone(),
        source_relevances: Some(vec![1.0; sources.len()]),
        node_type: KnowledgeType::Semantic,
        confidence: 0.8,
        origin,
        entity_tags: vec![],
        timestamp: Timestamp(2000),
    }
}

#[test]
fn consistent_sources_with_supportive_edges() {
    let mut engine = Engine::new();
    let a = insert_source(&mut engine, "src-a", "agent-1", "sess-1");
    let b = insert_source(&mut engine, "src-b", "agent-2", "sess-2");
    let c = insert_source(&mut engine, "src-c", "agent-3", "sess-3");

    engine.link(a, b, EdgeType::Supports, 0.8).unwrap();
    engine.link(b, c, EdgeType::Reason, 0.7).unwrap();

    let result = engine
        .crystallize(crystal_request(vec![a, b, c], origin("agent-1", "sess-1")))
        .unwrap();

    assert!(
        result.consistency_score > 0.5,
        "expected consistency_score > 0.5, got {}",
        result.consistency_score
    );
    assert!(
        (result.contradiction_rate - 0.0).abs() < f64::EPSILON,
        "expected contradiction_rate == 0.0, got {}",
        result.contradiction_rate
    );
    assert!(
        result.support_density > 0.0,
        "expected support_density > 0.0, got {}",
        result.support_density
    );
    assert!(!result.circular_evidence_warning);
    assert!(!result.single_source_warning);
}

#[test]
fn contradicting_sources() {
    let mut engine = Engine::new();
    let a = insert_source(&mut engine, "src-a", "agent-1", "sess-1");
    let b = insert_source(&mut engine, "src-b", "agent-2", "sess-2");

    engine.link(a, b, EdgeType::Contradicts, 0.9).unwrap();

    let result = engine
        .crystallize(crystal_request(vec![a, b], origin("agent-1", "sess-1")))
        .unwrap();

    assert!(
        result.contradiction_rate > 0.0,
        "expected contradiction_rate > 0.0, got {}",
        result.contradiction_rate
    );
    assert!(
        result.consistency_score < 0.5,
        "expected consistency_score < 0.5, got {}",
        result.consistency_score
    );
}

#[test]
fn circular_evidence_detected() {
    let mut engine = Engine::new();
    let a = insert_source(&mut engine, "src-a", "agent-1", "sess-1");
    let b = insert_source(&mut engine, "src-b", "agent-2", "sess-2");
    let c = insert_source(&mut engine, "src-c", "agent-3", "sess-3");

    let first = engine
        .crystallize(crystal_request(vec![a, b], origin("agent-1", "sess-1")))
        .unwrap();

    assert!(!first.circular_evidence_warning);

    let second = engine
        .crystallize(crystal_request(vec![a, c], origin("agent-1", "sess-1")))
        .unwrap();

    assert!(
        second.circular_evidence_warning,
        "source a is already a ConsolidatedFrom target of the first crystal"
    );
}

#[test]
fn single_source_warning_same_agent_session() {
    let mut engine = Engine::new();
    let a = insert_source(&mut engine, "src-a", "agent-1", "sess-1");
    let b = insert_source(&mut engine, "src-b", "agent-1", "sess-1");
    let c = insert_source(&mut engine, "src-c", "agent-1", "sess-1");

    let result = engine
        .crystallize(crystal_request(vec![a, b, c], origin("agent-1", "sess-1")))
        .unwrap();

    assert!(
        result.single_source_warning,
        "all sources from same (agent_id, session_id)"
    );
}

#[test]
fn no_single_source_warning_different_agents() {
    let mut engine = Engine::new();
    let a = insert_source(&mut engine, "src-a", "agent-1", "sess-1");
    let b = insert_source(&mut engine, "src-b", "agent-2", "sess-2");

    let result = engine
        .crystallize(crystal_request(vec![a, b], origin("agent-1", "sess-1")))
        .unwrap();

    assert!(!result.single_source_warning);
}

#[test]
fn no_edges_between_sources_yields_zero_metrics() {
    let mut engine = Engine::new();
    let a = insert_source(&mut engine, "src-a", "agent-1", "sess-1");
    let b = insert_source(&mut engine, "src-b", "agent-2", "sess-2");

    let result = engine
        .crystallize(crystal_request(vec![a, b], origin("agent-1", "sess-1")))
        .unwrap();

    assert!(
        (result.consistency_score - 0.0).abs() < f64::EPSILON,
        "no edges → consistency_score should be 0.0, got {}",
        result.consistency_score
    );
    assert!(
        (result.contradiction_rate - 0.0).abs() < f64::EPSILON,
        "no edges → contradiction_rate should be 0.0, got {}",
        result.contradiction_rate
    );
    assert!(
        (result.support_density - 0.0).abs() < f64::EPSILON,
        "no edges → support_density should be 0.0, got {}",
        result.support_density
    );
}
