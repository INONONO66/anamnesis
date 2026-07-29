use anamnesis::api::{Engine, EngineConfig, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::{SearchDiagnostics, SearchInput};

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
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
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

#[test]
fn diagnostic_trace_limit_is_bounded_and_behaviorally_inert() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);
    let _ = engine.ingest(make_obs("auth factory pattern")).unwrap();
    let input = SearchInput {
        text: "auth".into(),
        limit: 10,
        ..Default::default()
    };

    let baseline = engine.search(input.clone()).unwrap();
    let diagnostic = engine
        .search_with_diagnostics(
            input.clone(),
            &SearchDiagnostics::with_readout_trace_limit(512),
        )
        .unwrap();
    assert_eq!(baseline.package, diagnostic.package);
    assert_eq!(baseline.trace.readout, diagnostic.trace.readout);

    let invalid =
        engine.search_with_diagnostics(input, &SearchDiagnostics::with_readout_trace_limit(0));
    assert!(invalid.is_err());
}

#[test]
fn public_query_variants_expose_the_exact_additive_search_surface() {
    let variants =
        anamnesis::query::search_query_variants("How many times has John injured his ankle?");
    assert_eq!(
        variants.first().map(String::as_str),
        Some("How many times has John injured his ankle?")
    );
    assert!(
        variants
            .iter()
            .any(|variant| variant == "John injured his ankle")
    );
}
