//! Integration tests for seed selection via SearchInput.seed_limit.

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::{Engine, EngineConfig};

#[test]
fn default_seed_limit_three() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_enabled(false)
            .with_novelty_threshold(0.0),
    );

    for i in 0..5 {
        let _ = engine.ingest(Observation {
            name: format!("node-{}", i),
            summary: None,
            content: format!("content for node {}", i),
            embedding: Some(vec![0.5; 4]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test-session".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        });
    }

    let result = engine.search(anamnesis::query::SearchInput {
        text: "node".into(),
        agent_id: None,
        peer_filter: None,
        scope: anamnesis::graph::ScopePath::universal(),
        now: Timestamp::now(),
        query_embedding: None,
        limit: 10,
        context: None,
        entity_tags: vec![],
        seed_limit: None,
    });

    assert!(result.is_ok());
    let search_result = result.unwrap();
    assert_eq!(
        search_result.trace.seed_count, 3,
        "default seed_limit should be 3"
    );
}

#[test]
fn custom_seed_limit_five() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_enabled(false)
            .with_novelty_threshold(0.0),
    );

    for i in 0..7 {
        let _ = engine.ingest(Observation {
            name: format!("node-{}", i),
            summary: None,
            content: format!("content for node {}", i),
            embedding: Some(vec![0.5; 4]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test-session".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        });
    }

    let result = engine.search(anamnesis::query::SearchInput {
        text: "node".into(),
        agent_id: None,
        peer_filter: None,
        scope: anamnesis::graph::ScopePath::universal(),
        now: Timestamp::now(),
        query_embedding: None,
        limit: 10,
        context: None,
        entity_tags: vec![],
        seed_limit: Some(5),
    });

    assert!(result.is_ok());
    let search_result = result.unwrap();
    assert_eq!(
        search_result.trace.seed_count, 5,
        "custom seed_limit should be 5"
    );
}

#[test]
fn seed_limit_zero_no_panic() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_enabled(false)
            .with_novelty_threshold(0.0),
    );

    for i in 0..3 {
        let _ = engine.ingest(Observation {
            name: format!("node-{}", i),
            summary: None,
            content: format!("content for node {}", i),
            embedding: Some(vec![0.5; 4]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test-session".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        });
    }

    let result = engine.search(anamnesis::query::SearchInput {
        text: "node".into(),
        agent_id: None,
        peer_filter: None,
        scope: anamnesis::graph::ScopePath::universal(),
        now: Timestamp::now(),
        query_embedding: None,
        limit: 10,
        context: None,
        entity_tags: vec![],
        seed_limit: Some(0),
    });

    assert!(result.is_ok(), "seed_limit=0 should not panic");
    let search_result = result.unwrap();
    assert_eq!(
        search_result.trace.seed_count, 0,
        "seed_limit=0 should return 0 seeds"
    );
}

#[test]
fn seed_limit_larger_than_fused_returns_all() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_enabled(false)
            .with_novelty_threshold(0.0),
    );

    for i in 0..3 {
        let _ = engine.ingest(Observation {
            name: format!("node-{}", i),
            summary: None,
            content: format!("content for node {}", i),
            embedding: Some(vec![0.5; 4]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test-session".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        });
    }

    let result = engine.search(anamnesis::query::SearchInput {
        text: "node".into(),
        agent_id: None,
        peer_filter: None,
        scope: anamnesis::graph::ScopePath::universal(),
        now: Timestamp::now(),
        query_embedding: None,
        limit: 10,
        context: None,
        entity_tags: vec![],
        seed_limit: Some(100),
    });

    assert!(result.is_ok());
    let search_result = result.unwrap();
    assert_eq!(
        search_result.trace.seed_count, 3,
        "seed_limit larger than fused should return all 3 seeds"
    );
}
