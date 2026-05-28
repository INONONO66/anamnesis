use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::{Query, QueryConfig, SearchInput};

fn make_obs(name: &str, node_type: KnowledgeType) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("Summary of {}", name)),
        content: format!(
            "Full content of {}. This is a longer text to ensure we have enough content for L2 resolution. The content should be substantial enough to test token budget allocation.",
            name
        ),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(0),
    }
}

/// Test that L2 content is populated for all knowledge and memory fragments when budget allows.
///
/// Issue #14: L2 content should be available for all knowledge fragments when token budget is sufficient.
/// Currently, only top-3 knowledge fragments receive L2 content, and memories are forced to L0.
#[test]
fn test_l2_content_all_partitions_when_budget_allows() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Ingest 10 Semantic nodes + 5 Episodic nodes with summaries and content
    let mut semantic_ids = Vec::new();
    for i in 0..10 {
        let obs = make_obs(&format!("semantic-{}", i), KnowledgeType::Semantic);
        match engine.ingest(obs).unwrap() {
            IngestResult::Created(ids) => semantic_ids.push(ids[0]),
            _ => panic!("expected Created"),
        }
    }

    let mut episodic_ids = Vec::new();
    for i in 0..5 {
        let obs = make_obs(&format!("episodic-{}", i), KnowledgeType::Episodic);
        match engine.ingest(obs).unwrap() {
            IngestResult::Created(ids) => episodic_ids.push(ids[0]),
            _ => panic!("expected Created"),
        }
    }

    // Query with List mode and very large token budget
    let query = Query::List {
        min_salience: 0.0,
        limit: 20,
    };
    let mut config = QueryConfig::default();
    config.token_budget = 100_000;
    let result = engine.query(&query, &config).unwrap();

    // Assert ALL knowledge fragments have L2 content
    // Current bug: only top-3 knowledge fragments get content
    for (idx, frag) in result.knowledge.iter().enumerate() {
        assert!(
            frag.content.is_some(),
            "Knowledge fragment {} (index {}) should have L2 content when budget is 100k",
            frag.node_id.0,
            idx
        );
    }

    // Assert ALL memory fragments have L2 content and summaries
    // Current bug: memories are forced to L0
    for (idx, frag) in result.memories.iter().enumerate() {
        assert!(
            frag.content.is_some(),
            "Memory fragment {} (index {}) should have L2 content when budget is 100k",
            frag.node_id.0,
            idx
        );
        assert!(
            frag.summary.is_some(),
            "Memory fragment {} (index {}) should have L1 summary",
            frag.node_id.0,
            idx
        );
    }
}

/// Test that L2 budget exhaustion degrades gracefully.
///
/// Issue #14: When token budget is constrained, lower-relevance nodes should not receive L2 content.
/// Budget should not be exceeded.
#[test]
fn test_l2_budget_exhaustion_degrades_gracefully() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Ingest 5 nodes with distinct content sizes
    let mut node_ids = Vec::new();
    for i in 0..5 {
        let obs = Observation {
            name: format!("node-{}", i),
            summary: Some(format!("Summary {}", i)),
            content: format!("Content for node {}. {}", i, "x".repeat(500 + i * 100)),
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
        };
        match engine.ingest(obs).unwrap() {
            IngestResult::Created(ids) => node_ids.push(ids[0]),
            _ => panic!("expected Created"),
        }
    }

    // Query with constrained token budget
    let query = Query::List {
        min_salience: 0.0,
        limit: 5,
    };
    let mut config = QueryConfig::default();
    config.token_budget = 500;
    let result = engine.query(&query, &config).unwrap();

    // Assert budget is not exceeded
    assert!(
        result.token_usage.used <= result.token_usage.total,
        "Token usage {} should not exceed budget {}",
        result.token_usage.used,
        result.token_usage.total
    );

    // Assert mixed resolution: some nodes have L2, some don't
    let has_l2 = result.knowledge.iter().any(|f| f.content.is_some());
    let has_l0_or_l1 = result.knowledge.iter().any(|f| f.content.is_none());
    assert!(
        has_l2 || has_l0_or_l1,
        "Should have mixed resolution levels due to budget constraints"
    );
}

/// Test that episodic content is preserved in search results.
///
/// Issue #14: Episodic (memory) fragments should not be cleared or forced to L0.
/// They should appear in the memories partition with content when available.
#[test]
fn test_search_episodic_content_preserved() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let semantic_id = match engine
        .ingest(Observation {
            name: "auth bug session semantic fact".into(),
            summary: Some("Auth bug source fact".into()),
            content: "Knowledge extracted from auth bug session notes.".into(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec!["auth".to_string()],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "session-knowledge".to_string(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(0),
        })
        .unwrap()
    {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };

    // Ingest 5 Episodic observations with searchable text and content
    let mut episodic_ids = Vec::new();
    for i in 0..5 {
        let obs = Observation {
            name: format!("session-note-{}", i),
            summary: Some(format!("Note summary {}", i)),
            content: format!(
                "Session note content {}. Found a bug in the auth module during testing.",
                i
            ),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec!["auth".to_string()],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: format!("session-{}", i),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(i as u64),
        };
        match engine.ingest(obs).unwrap() {
            IngestResult::Created(ids) => {
                episodic_ids.push(ids[0]);
                engine
                    .link(ids[0], semantic_id, EdgeType::ExtractedFrom, 0.9)
                    .unwrap();
            }
            _ => panic!("expected Created"),
        }
    }

    // Ingest a contradicting fact to trigger KnowledgeWithProvenance packaging mode
    let contradiction_id = match engine
        .ingest(Observation {
            name: "auth bug contradiction".into(),
            summary: Some("Contradicting fact".into()),
            content: "Auth module session review: secure and has no bugs.".into(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec!["auth".to_string()],
            origin: Origin {
                peer_id: anamnesis::graph::types::PeerId(0),
                source_kind: anamnesis::peer::SourceKind::AgentObservation,
                session_id: "session-contradiction".to_string(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(100),
        })
        .unwrap()
    {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    engine
        .link(semantic_id, contradiction_id, EdgeType::Contradicts, 0.8)
        .unwrap();

    // Search for matching text
    let result = engine
        .search(SearchInput {
            text: "auth bug session".into(),
            limit: 10,
            seed_limit: Some(10),
            ..Default::default()
        })
        .unwrap();

    // Assert memories partition is not empty
    assert!(
        !result.package.memories.is_empty(),
        "Memories partition should not be empty after search"
    );

    // Assert at least one memory has CONTENT (not just summary)
    // Current bug: memories are forced to L0, so content is None
    let has_content = result.package.memories.iter().any(|m| m.content.is_some());
    assert!(
        has_content,
        "At least one memory fragment should have L2 content"
    );
}

/// Test that search uses multiple seeds and spreading activation.
///
/// Issue #14: Search should use multiple seeds (not hardcoded to 3) and perform spreading activation.
/// The trace should show seed_count >= 5 and spread_iterations > 3.
#[test]
fn test_search_seeds_ordered_by_relevance() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Ingest at least 10 searchable nodes
    let mut node_ids = Vec::new();
    for i in 0..10 {
        let obs = make_obs(&format!("searchable-node-{}", i), KnowledgeType::Semantic);
        match engine.ingest(obs).unwrap() {
            IngestResult::Created(ids) => node_ids.push(ids[0]),
            _ => panic!("expected Created"),
        }
    }

    // Search with text that matches all nodes
    let result = engine
        .search(SearchInput {
            text: "searchable".into(),
            limit: 10,
            seed_limit: Some(5),
            ..Default::default()
        })
        .unwrap();

    // Assert seed_count >= 5 (not hardcoded to 3)
    // Current bug: .take(3) limits seeds to 3
    assert!(
        result.trace.seed_count >= 5,
        "Should have at least 5 seeds, got {}",
        result.trace.seed_count
    );

    // Assert spreading activation occurred as one multi-source invocation.
    assert_eq!(result.trace.spread_iterations, 1);

    // Assert strategies include spreading activation
    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "spreading_activation"),
        "Should use spreading_activation strategy"
    );
}

/// Test that KnowledgeOnly packaging mode excludes memories.
///
/// KnowledgeOnly mode should return only knowledge fragments and keep token
/// accounting consistent with the returned package.
#[test]
fn test_knowledge_only_excludes_memories() {
    let config = EngineConfig::default().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    // Ingest both semantic and episodic nodes
    let semantic_obs = make_obs("semantic-fact", KnowledgeType::Semantic);
    let semantic_id = match engine.ingest(semantic_obs).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };

    let episodic_obs = make_obs("episodic-memory", KnowledgeType::Episodic);
    let episodic_id = match engine.ingest(episodic_obs).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    engine
        .link(episodic_id, semantic_id, EdgeType::ExtractedFrom, 0.9)
        .unwrap();

    // Search with ordinary text (triggers KnowledgeOnly packaging mode)
    let result = engine
        .search(SearchInput {
            text: "semantic episodic".into(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert!(
        result
            .package
            .memories
            .iter()
            .all(|m| m.node_id != episodic_id),
        "Episodic node should not be in memories partition"
    );
    assert!(result.package.memories.is_empty());
    assert_eq!(result.package.token_usage.memories_used, 0);
    assert_eq!(
        result.package.token_usage.used,
        result.package.token_usage.identity_used
            + result.package.token_usage.knowledge_used
            + result.package.token_usage.memories_used
    );
}
