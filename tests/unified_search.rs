use anamnesis::api::{Engine, EngineConfig, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;

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
fn search_with_text_returns_results() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);
    let _ = engine.ingest(make_obs("auth factory pattern")).unwrap();

    let result = engine
        .search(SearchInput {
            text: "auth".into(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
    assert!(!result.trace.strategies_used.is_empty());
}

#[test]
fn search_empty_text_and_no_embedding_returns_error() {
    let engine = Engine::new();
    let result = engine.search(SearchInput {
        text: "".into(),
        query_embedding: None,
        ..Default::default()
    });

    assert!(result.is_err());
}
