use anamnesis::api::{Engine, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Timestamp};

fn make_obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: name.to_string(),
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
        timestamp: Timestamp(0),
    }
}

#[test]
fn node_default_tier_is_auto() {
    let mut e = Engine::new();
    let id = match e.ingest(make_obs("a")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    let node = e.graph().get_node(id).unwrap();
    assert!(matches!(node.tier, MemoryTier::Auto));
}
