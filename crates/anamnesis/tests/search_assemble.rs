use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, ScopeRelation, Timestamp};
use anamnesis::query::SearchInput;

fn origin(_agent: &str, session: &str, scope: Option<&str>) -> Origin {
    let scope = scope
        .map(|s| ScopePath::new(s).expect("valid scope"))
        .unwrap_or_else(ScopePath::universal);
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::engine::SourceKind::AgentObservation,
        session_id: session.to_string(),
        scope,
        confidence: 0.9,
    }
}

fn observation(
    name: &str,
    node_type: KnowledgeType,
    scope: Option<&str>,
    timestamp: u64,
) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("summary for {name}")),
        content: format!("full content for {name} with enough detail for assembly tests"),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: vec![],
        origin: origin("agent-1", "session-1", scope),
        timestamp: Timestamp(timestamp),
        valid_from: None,
        valid_until: None,
    }
}

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::default()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn ingest(engine: &mut Engine, observation: Observation) -> NodeId {
    match engine.ingest(observation).expect("ingest should succeed") {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("dedup is disabled"),
    }
}

#[test]
fn token_usage_preserved() {
    let mut engine = engine();
    ingest(
        &mut engine,
        observation("token-usage-search fact", KnowledgeType::Semantic, None, 0),
    );

    let result = engine
        .search(SearchInput {
            text: "token-usage-search".into(),
            limit: 10,
            ..Default::default()
        })
        .expect("search should succeed");

    assert!(result.package.token_usage.total > 0);
    assert!(result.package.token_usage.used > 0);
    assert!(result.package.token_usage.knowledge_used > 0);
}

#[test]
fn identity_tension_preserved() {
    let mut engine = engine();
    let identity = ingest(
        &mut engine,
        observation(
            "identity-tension anchor",
            KnowledgeType::IdentityCore,
            None,
            0,
        ),
    );
    let fact = ingest(
        &mut engine,
        observation(
            "identity-tension contrary fact",
            KnowledgeType::Semantic,
            None,
            1,
        ),
    );
    engine
        .link(identity, fact, EdgeType::Contradicts)
        .expect("link should succeed");

    let result = engine
        .search(SearchInput {
            text: "identity-tension".into(),
            agent_id: Some("0".into()), // PeerId(0) matches nodes created with PeerId(0)
            limit: 10,
            seed_limit: Some(2),
            ..Default::default()
        })
        .expect("search should succeed");

    assert!(result.package.agent_tension > 0.0);
    assert!(!result.package.tensions.is_empty());
}

#[test]
fn balanced_default_preserves_activated_memories_and_token_accounting() {
    // Balanced packaging (the default for plain queries) is a no-op on the
    // assembled package: episodic nodes that won activation appear in memories,
    // and token accounting stays consistent (readout-scoring.md "Bucket Handling").
    let mut engine = engine();
    let source = ingest(
        &mut engine,
        observation("linked-source episode", KnowledgeType::Episodic, None, 0),
    );
    let knowledge = ingest(
        &mut engine,
        observation("linked-source knowledge", KnowledgeType::Semantic, None, 1),
    );
    engine
        .link(source, knowledge, EdgeType::ExtractedFrom)
        .expect("link should succeed");

    let result = engine
        .search(SearchInput {
            text: "linked-source".into(),
            limit: 10,
            seed_limit: Some(2),
            ..Default::default()
        })
        .expect("search should succeed");

    assert_eq!(
        result.trace.packaging_mode,
        Some(anamnesis::query::PackagingMode::Balanced)
    );
    assert!(
        result
            .package
            .knowledge
            .iter()
            .any(|f| f.node_id == knowledge)
    );
    // Balanced preserves the assembled memories bucket unchanged.
    assert!(result.package.memories.iter().any(|f| f.node_id == source));
    // Token accounting must always be internally consistent.
    assert_eq!(
        result.package.token_usage.used,
        result.package.token_usage.identity_used
            + result.package.token_usage.knowledge_used
            + result.package.token_usage.memories_used
    );
}

#[test]
fn balanced_default_preserves_all_activated_episodic_nodes() {
    // Under Balanced packaging, all episodic nodes that win activation appear in
    // the memories bucket regardless of whether they have ExtractedFrom links.
    let mut engine = engine();
    let source = ingest(
        &mut engine,
        observation(
            "drop-check linked episode",
            KnowledgeType::Episodic,
            None,
            0,
        ),
    );
    let unrelated = ingest(
        &mut engine,
        observation(
            "drop-check unrelated episode",
            KnowledgeType::Episodic,
            None,
            1,
        ),
    );
    let knowledge = ingest(
        &mut engine,
        observation("drop-check knowledge", KnowledgeType::Semantic, None, 2),
    );
    engine
        .link(source, knowledge, EdgeType::ExtractedFrom)
        .expect("link should succeed");

    let result = engine
        .search(SearchInput {
            text: "drop-check".into(),
            limit: 10,
            seed_limit: Some(3),
            ..Default::default()
        })
        .expect("search should succeed");

    assert_eq!(
        result.trace.packaging_mode,
        Some(anamnesis::query::PackagingMode::Balanced)
    );
    // Both episodic nodes matched the query and won activation; both survive
    // in the memories bucket under Balanced packaging.
    assert!(result.package.memories.iter().any(|f| f.node_id == source));
    assert!(
        result
            .package
            .memories
            .iter()
            .any(|f| f.node_id == unrelated)
    );
}

#[test]
fn source_fragment_carries_source_scope() {
    let mut engine = engine();
    let source = ingest(
        &mut engine,
        observation(
            "origin episode",
            KnowledgeType::Episodic,
            Some("source-project"),
            0,
        ),
    );
    let knowledge = ingest(
        &mut engine,
        observation(
            "scope-source knowledge",
            KnowledgeType::Semantic,
            Some("knowledge-project"),
            1,
        ),
    );
    let contrary = ingest(
        &mut engine,
        observation(
            "scope-source contrary",
            KnowledgeType::Semantic,
            Some("knowledge-project"),
            2,
        ),
    );
    engine
        .link(source, knowledge, EdgeType::ExtractedFrom)
        .expect("link should succeed");
    engine
        .link(knowledge, contrary, EdgeType::Contradicts)
        .expect("link should succeed");

    let result = engine
        .search(SearchInput {
            text: "scope-source".into(),
            scope: anamnesis::graph::ScopePath::new("source-project").expect("valid scope"),
            limit: 10,
            seed_limit: Some(2),
            ..Default::default()
        })
        .expect("search should succeed");

    let fragment = result
        .package
        .memories
        .iter()
        .find(|fragment| fragment.node_id == source)
        .expect("source memory should be packaged");
    assert_eq!(fragment.origin.scope.as_str(), "source-project");
    assert_eq!(fragment.scope, ScopeRelation::Equal);
}

#[test]
fn empty_seeds_return_empty_package() {
    let mut engine = engine();
    ingest(
        &mut engine,
        observation("present searchable fact", KnowledgeType::Semantic, None, 0),
    );

    let result = engine
        .search(SearchInput {
            text: "absent-search-term".into(),
            limit: 10,
            ..Default::default()
        })
        .expect("search should succeed");

    assert_eq!(result.trace.seed_count, 0);
    assert_eq!(result.package.total_fragments(), 0);
    assert!(result.package.tensions.is_empty());
}
