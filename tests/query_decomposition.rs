use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, KnowledgeType, SearchInput};
use anamnesis::graph::Timestamp;
use anamnesis::graph::node::Origin;

#[test]
fn english_who_is_pattern() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "CEO of Hashed".into(),
            summary: None,
            content: "CEO of Hashed".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "who is the CEO of Hashed?".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn english_what_is_pattern() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "factory pattern".into(),
            summary: None,
            content: "the factory pattern is a design pattern".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "what is the factory pattern?".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn english_of_pattern() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "CEO".into(),
            summary: None,
            content: "CEO of Hashed".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "CEO of Hashed".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn english_how_does_pattern() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "spreading activation".into(),
            summary: None,
            content: "spreading activation is a query mechanism".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "how does spreading activation work?".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn korean_eui_pattern() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "Hashed".into(),
            summary: None,
            content: "Hashed의 CEO".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "Hashed의 CEO는 누구야?".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn korean_nugu_pattern() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "Alice".into(),
            summary: None,
            content: "Alice is a person".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "Alice가 누구야?".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn korean_mwo_pattern() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "팩토리 패턴".into(),
            summary: None,
            content: "팩토리 패턴은 디자인 패턴이다".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "팩토리 패턴은 뭐야?".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn no_match_returns_original() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "foo bar baz".into(),
            summary: None,
            content: "foo bar baz".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "foo bar baz".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}

#[test]
fn case_insensitive_english() {
    let mut engine = Engine::with_config(
        EngineConfig::default()
            .with_dedup_threshold(2.0)
            .with_novelty_threshold(0.0),
    );

    engine
        .ingest(Observation {
            name: "Alice".into(),
            summary: None,
            content: "Alice is a person".into(),
            embedding: Some(vec![0.5, 0.5, 0.5, 0.5]),
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "test".into(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "Who Is Alice?".into(),
            query_embedding: None,
            entity_tags: vec![],
            agent_id: None,
            peer_filter: None,
            scope: anamnesis::graph::ScopePath::universal(),
            seed_limit: None,
            limit: 10,
            now: Timestamp::now(),
            context: None,
        })
        .unwrap();

    assert!(!result.package.knowledge.is_empty());
}
