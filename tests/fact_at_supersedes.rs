use anamnesis::api::{Engine, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::query::Query;

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
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
    }
}

fn created_id(result: IngestResult) -> NodeId {
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("expected Created"),
    }
}

#[test]
fn supersedes_invalidates_old_fact() {
    let mut engine = Engine::new();
    let old = created_id(engine.ingest(make_obs("old fact")).unwrap());
    let new = created_id(engine.ingest(make_obs("new fact")).unwrap());

    engine.link(new, old, EdgeType::Supersedes).unwrap();

    let old_node = engine.graph().get_node(old).unwrap();
    let new_node = engine.graph().get_node(new).unwrap();
    assert!(
        old_node.valid_until.is_some(),
        "old fact should be invalidated"
    );
    assert!(
        new_node.valid_from.is_some(),
        "new fact should become valid"
    );
}

#[test]
fn fact_at_returns_only_valid_facts() {
    let mut engine = Engine::new();
    let old = created_id(engine.ingest(make_obs("old fact")).unwrap());
    let new_fact = created_id(engine.ingest(make_obs("new fact")).unwrap());

    engine.link(new_fact, old, EdgeType::Supersedes).unwrap();

    let old_node = engine.graph().get_node(old).unwrap();
    assert!(old_node.valid_until.is_some());
    let new_node = engine.graph().get_node(new_fact).unwrap();
    assert!(new_node.valid_from.is_some());

    let far_future = Timestamp(u64::MAX / 2);
    let query = Query::Associative {
        seed: new_fact,
        budget: 100,
    };
    let package = engine.fact_at(&query, far_future).unwrap();

    let found_ids: Vec<NodeId> = package
        .knowledge
        .iter()
        .map(|fragment| fragment.node_id)
        .collect();
    assert!(
        found_ids.contains(&new_fact),
        "new fact should still be valid at far future"
    );
    assert!(
        !found_ids.contains(&old),
        "old fact should be filtered out at far future"
    );
}
