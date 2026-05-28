use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;
use anamnesis::{Engine, EngineConfig, Error};

fn setup_engine() -> Engine {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);
    let _ = engine
        .ingest(Observation {
            name: "valid query node".into(),
            summary: None,
            content: "valid query factory pattern auth handler".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "session-1".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(0),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
    engine
}

fn embedding() -> Vec<f64> {
    vec![0.5, 0.5, 0.5, 0.5]
}

fn used_text(strategies: &[String]) -> bool {
    strategies.iter().any(|s| s == "text_search")
}

fn used_vector(strategies: &[String]) -> bool {
    strategies.iter().any(|s| s == "vector_similarity")
}

#[test]
fn valid_text_with_embedding_uses_text_and_vector() {
    let engine = setup_engine();
    let result = engine
        .search(SearchInput {
            text: "valid query".into(),
            query_embedding: Some(embedding()),
            ..Default::default()
        })
        .expect("search should succeed");

    assert!(used_text(&result.trace.strategies_used));
    assert!(used_vector(&result.trace.strategies_used));
}

#[test]
fn valid_text_no_embedding_uses_text_only() {
    let engine = setup_engine();
    let result = engine
        .search(SearchInput {
            text: "valid query".into(),
            query_embedding: None,
            ..Default::default()
        })
        .expect("search should succeed");

    assert!(used_text(&result.trace.strategies_used));
    assert!(!used_vector(&result.trace.strategies_used));
}

#[test]
fn whitespace_text_with_embedding_uses_vector_only() {
    let engine = setup_engine();
    let result = engine
        .search(SearchInput {
            text: "   ".into(),
            query_embedding: Some(embedding()),
            ..Default::default()
        })
        .expect("search should succeed");

    assert!(!used_text(&result.trace.strategies_used));
    assert!(used_vector(&result.trace.strategies_used));
}

#[test]
fn whitespace_text_no_embedding_returns_invalid_input() {
    let engine = setup_engine();
    let result = engine.search(SearchInput {
        text: "   ".into(),
        query_embedding: None,
        ..Default::default()
    });

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[test]
fn empty_text_no_embedding_returns_invalid_input() {
    let engine = setup_engine();
    let result = engine.search(SearchInput {
        text: "".into(),
        query_embedding: None,
        ..Default::default()
    });

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[test]
fn empty_text_with_embedding_uses_vector_only() {
    let engine = setup_engine();
    let result = engine
        .search(SearchInput {
            text: "".into(),
            query_embedding: Some(embedding()),
            ..Default::default()
        })
        .expect("search should succeed");

    assert!(!used_text(&result.trace.strategies_used));
    assert!(used_vector(&result.trace.strategies_used));
}
