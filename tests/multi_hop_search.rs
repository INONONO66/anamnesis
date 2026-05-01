use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::query::SearchInput;

fn make_obs_tagged(name: &str, tags: Vec<&str>) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: name.to_string(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags.into_iter().map(|s| s.to_string()).collect(),
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
fn multi_hop_finds_linked_entities() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut e = Engine::with_config(config);

    let hashed = match e
        .ingest(make_obs_tagged("Hashed entity", vec!["Hashed"]))
        .unwrap()
    {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    let ceo = match e
        .ingest(make_obs_tagged("CEO of Hashed is X", vec!["Hashed", "CEO"]))
        .unwrap()
    {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    e.link(ceo, hashed, EdgeType::Semantic, 1.0).unwrap();

    let result = e
        .search(SearchInput {
            text: "Hashed CEO".into(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    let found_ids: Vec<NodeId> = result.package.knowledge.iter().map(|f| f.node_id).collect();
    assert!(
        found_ids.contains(&ceo) || found_ids.contains(&hashed),
        "Should find at least one of the linked entities"
    );
}
