use anamnesis::Engine;
use anamnesis::api::{DecayModel, EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};

fn make_obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: None,
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
    }
}

#[test]
fn power_law_mode_updates_access_history_on_touch() {
    let cfg = EngineConfig {
        decay_model: DecayModel::PowerLaw,
        ..EngineConfig::default()
    };
    let mut e = Engine::with_config(cfg);
    let IngestResult::Created(ids) = e.ingest(make_obs("a")).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];

    let history_before = e.graph().get_node(id).unwrap().access_history.len();
    e.touch(id, Timestamp(2000)).unwrap();
    let history_after = e.graph().get_node(id).unwrap().access_history.len();

    assert!(
        history_after > history_before,
        "access_history should grow on touch in PowerLaw mode"
    );
}

#[test]
fn exponential_mode_unchanged() {
    let mut e = Engine::new();
    let IngestResult::Created(ids) = e.ingest(make_obs("b")).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];
    let future = Timestamp(1000 + 30 * 86_400_000);

    e.touch(id, future).unwrap();
    let node = e.graph().get_node(id).unwrap();

    assert!(node.salience < 1.0, "salience should have decayed");
    assert!(node.salience > 0.0, "salience should not be zero");
}
