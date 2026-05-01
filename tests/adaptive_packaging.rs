use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::{PackagingMode, SearchInput};

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
fn high_tension_triggers_provenance_mode() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut e = Engine::with_config(config);

    let a = match e.ingest(make_obs("conflict topic A")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    let b = match e.ingest(make_obs("conflict topic B")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    let hub = match e.ingest(make_obs("conflict topic hub")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    e.link(hub, a, EdgeType::Semantic, 0.9).unwrap();
    e.link(hub, b, EdgeType::Semantic, 0.9).unwrap();
    e.link(a, b, EdgeType::Contradicts, 0.9).unwrap();

    let result = e
        .search(SearchInput {
            text: "conflict topic".into(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert!(!result.trace.strategies_used.is_empty());
    assert_eq!(
        result.trace.packaging_mode,
        Some(PackagingMode::KnowledgeWithProvenance)
    );
}

#[test]
fn default_returns_knowledge_only() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut e = Engine::with_config(config);
    let _ = e.ingest(make_obs("simple fact")).unwrap();

    let result = e
        .search(SearchInput {
            text: "simple".into(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
    assert_eq!(
        result.trace.packaging_mode,
        Some(PackagingMode::KnowledgeOnly)
    );
}
