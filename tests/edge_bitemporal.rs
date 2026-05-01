use anamnesis::api::{Engine, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};

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
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(0),
    }
}

#[test]
fn edge_default_has_no_valid_range() {
    let mut e = Engine::new();
    let a = match e.ingest(make_obs("a")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    let b = match e.ingest(make_obs("b")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    let eid = e.link(a, b, EdgeType::Semantic, 1.0).unwrap();
    let edge = e.graph().get_edge(eid).unwrap();
    assert!(edge.valid_from.is_none());
    assert!(edge.valid_until.is_none());
}
