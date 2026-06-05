use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, ScopeRelation, Timestamp};
use anamnesis::query::SearchInput;
use anamnesis::{NodeId, StorageAdapter};

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

/// The retrieval flow is now a single additive directed RWR; this alias keeps the
/// read-only invariant tests that previously toggled the spreading model.
fn rwr_engine() -> Engine {
    engine()
}

fn origin(_agent: &str, scope: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session".to_string(),
        scope: ScopePath::new(scope).expect("valid scope"),
        confidence: 0.9,
    }
}

fn observation(name: &str, node_type: KnowledgeType, scope: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("summary {name}")),
        content: format!("full content for {name}"),
        embedding: Some(vec![1.0, 0.0, 0.0]),
        confidence: 0.95,
        node_type,
        entity_tags: vec![name.split_whitespace().next().unwrap_or(name).to_string()],
        origin: origin("agent-1", scope),
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

fn ingest(engine: &mut Engine, name: &str, node_type: KnowledgeType, scope: &str) -> NodeId {
    match engine.ingest(observation(name, node_type, scope)).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
        IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    }
}

/// Protects RRF ordering from regressing to NodeId monotone sorting.
///
/// Engineered so the higher-NodeId node has an exact text match (Pass 1, score 1.0)
/// while the lower-NodeId node only matches via TF-IDF (Pass 2, score 0.5). RRF must
/// place the higher-NodeId node first, inverting NodeId-ascending order. If fusion
/// ever regresses to NodeId monotone sorting, this assertion fails.
#[test]
fn fused_order_differs_from_node_id_sort() {
    let mut engine = engine();
    // Distinct embeddings so both observations allocate as separate sites at the
    // surprise-gated ceiling (ADR-0009); identical embeddings would route the second
    // one in at near-zero charge and collapse its salience, masking the fusion order.
    let mut obs_first = observation("alpha weak", KnowledgeType::Semantic, "dev/rust");
    obs_first.embedding = Some(vec![1.0, 0.0, 0.0]);
    let first = match engine.ingest(obs_first).unwrap() {
        IngestResult::Created(ids) => ids[0],
        other => panic!("expected Created, got {other:?}"),
    };
    let mut obs_second = observation("alpha strong", KnowledgeType::Semantic, "dev/rust");
    obs_second.embedding = Some(vec![0.0, 1.0, 0.0]);
    let second = match engine.ingest(obs_second).unwrap() {
        IngestResult::Created(ids) => ids[0],
        other => panic!("expected Created, got {other:?}"),
    };
    // second (NodeId 1) becomes an exact-name match; first (NodeId 0) keeps a partial
    // word match only. Text rank 0 must go to second despite its higher NodeId.
    engine.graph_mut().get_node_mut(second).unwrap().name = "alpha".to_string();

    let result = engine
        .search(SearchInput {
            text: "alpha".to_string(),
            limit: 2,
            seed_limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<NodeId> = result.package.knowledge.iter().map(|f| f.node_id).collect();

    assert!(ids.contains(&first));
    assert!(ids.contains(&second));
    assert_eq!(ids, vec![second, first]);
    let mut node_id_sorted = ids.clone();
    node_id_sorted.sort_by_key(|n| n.0);
    assert_ne!(
        ids, node_id_sorted,
        "fused order must differ from NodeId-ascending"
    );
}

/// Protects token accounting from being reset during search assembly.
#[test]
fn token_usage_preserved() {
    let mut engine = engine();
    ingest(
        &mut engine,
        "token alpha",
        KnowledgeType::Semantic,
        "dev/rust",
    );

    let result = engine
        .search(SearchInput {
            text: "token".to_string(),
            limit: 1,
            ..Default::default()
        })
        .unwrap();

    assert!(result.package.token_usage.used > 0);
}

/// Protects identity contradiction tension from being lost in SearchResult assembly.
#[test]
fn identity_tension_preserved() {
    let mut engine = engine();
    let identity = ingest(
        &mut engine,
        "agent prefers safe rust",
        KnowledgeType::IdentityCore,
        "dev/rust",
    );
    let conflicting = ingest(
        &mut engine,
        "unsafe rust shortcut",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    engine
        .link(identity, conflicting, EdgeType::Contradicts, 0.9)
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "rust".to_string(),
            agent_id: Some("0".to_string()), // PeerId(0) matches nodes created with PeerId(0)
            limit: 10,
            seed_limit: Some(2),
            ..Default::default()
        })
        .unwrap();

    assert!(result.package.agent_tension > 0.0);
}

/// Protects RWR recall from ignoring identity prior and edge kappa weighting.
#[test]
fn rwr_consults_identity_and_kappa() {
    let mut engine = rwr_engine();
    let identity = ingest(
        &mut engine,
        "identity rust safety",
        KnowledgeType::IdentityCore,
        "dev/rust",
    );
    let seed = ingest(
        &mut engine,
        "rust seed",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    let supported = ingest(
        &mut engine,
        "rust supported",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    let refuted = ingest(
        &mut engine,
        "rust refuted",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    engine
        .link(seed, supported, EdgeType::Supersedes, 1.0)
        .unwrap();
    engine.link(seed, refuted, EdgeType::Refutes, 1.0).unwrap();
    engine
        .link(identity, supported, EdgeType::Semantic, 1.0)
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "rust".to_string(),
            agent_id: Some("agent-1".to_string()),
            limit: 10,
            seed_limit: Some(3),
            ..Default::default()
        })
        .unwrap();
    let relevance = |id| {
        result
            .package
            .knowledge
            .iter()
            .find(|fragment| fragment.node_id == id)
            .map(|fragment| fragment.relevance)
            .unwrap_or(0.0)
    };

    assert!(relevance(supported) > relevance(refuted));
}

/// Protects SearchInput.seed_limit from being ignored by recall selection.
#[test]
fn seed_limit_configurable_changes_recall() {
    let mut engine = engine();
    let a = ingest(
        &mut engine,
        "limit alpha",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    let b = ingest(
        &mut engine,
        "limit beta",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    let c = ingest(
        &mut engine,
        "limit gamma",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    engine.link(a, b, EdgeType::Semantic, 1.0).unwrap();
    engine.link(b, c, EdgeType::Semantic, 1.0).unwrap();

    let one = engine
        .search(SearchInput {
            text: "limit".into(),
            seed_limit: Some(1),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    let many = engine
        .search(SearchInput {
            text: "limit".into(),
            seed_limit: Some(10),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert_ne!(one.trace.seed_count, many.trace.seed_count);
}

/// Protects validation that whitespace text without vector input is invalid.
#[test]
fn whitespace_text_rejected_without_embedding() {
    let engine = engine();
    assert!(
        engine
            .search(SearchInput {
                text: "   ".to_string(),
                ..Default::default()
            })
            .is_err()
    );
}

/// Protects pure-vector search with whitespace text from being rejected.
#[test]
fn whitespace_text_accepted_with_embedding() {
    let mut engine = engine();
    ingest(
        &mut engine,
        "vector alpha",
        KnowledgeType::Semantic,
        "dev/rust",
    );

    let result = engine
        .search(SearchInput {
            text: "   ".to_string(),
            query_embedding: Some(vec![1.0, 0.0, 0.0]),
            limit: 1,
            ..Default::default()
        })
        .unwrap();

    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "vector_similarity")
    );
}

/// Protects Engine::link endpoint validation and finite weight handling matrix.
#[test]
fn engine_link_validation_full_matrix() {
    let mut engine = engine();
    let a = ingest(&mut engine, "link a", KnowledgeType::Semantic, "dev/rust");
    let b = ingest(&mut engine, "link b", KnowledgeType::Semantic, "dev/rust");

    assert!(
        engine
            .link(NodeId(999), b, EdgeType::Semantic, 0.5)
            .is_err()
    );
    assert!(
        engine
            .link(a, NodeId(999), EdgeType::Semantic, 0.5)
            .is_err()
    );
    assert!(engine.link(a, b, EdgeType::Semantic, f64::NAN).is_err());
    let low = engine.link(a, b, EdgeType::Semantic, -1.0).unwrap();
    assert_eq!(engine.graph().storage().get_edge(low).unwrap().weight, 0.0);
    let high = engine.link(a, b, EdgeType::Semantic, 2.0).unwrap();
    assert_eq!(engine.graph().storage().get_edge(high).unwrap().weight, 1.0);
}

/// Protects accessed_at and decay_checkpoint synchronization invariants.
#[test]
fn hot_field_setter_sync_invariant() {
    let mut engine = engine();
    let id = ingest(
        &mut engine,
        "hot field",
        KnowledgeType::Semantic,
        "dev/rust",
    );

    engine.touch(id, Timestamp(2000)).unwrap();
    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        storage.get_decay_checkpoint(id).unwrap()
    );

    engine.tick(Timestamp(3000)).unwrap();
    let storage = engine.graph().storage();
    assert!(storage.get_decay_checkpoint(id).unwrap() >= storage.get_accessed_at(id).unwrap());
}

/// Protects deterministic RRF tie-breaking by NodeId ascending.
#[test]
fn rrf_tie_break_node_id_ascending() {
    let mut engine = engine();
    let first = ingest(
        &mut engine,
        "tie alpha",
        KnowledgeType::Semantic,
        "dev/rust",
    );
    let second = ingest(&mut engine, "tie beta", KnowledgeType::Semantic, "dev/rust");

    let result = engine
        .search(SearchInput {
            text: "tie".to_string(),
            limit: 2,
            seed_limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<NodeId> = result.package.knowledge.iter().map(|f| f.node_id).collect();

    assert_eq!(ids, vec![first, second]);
}

/// Protects ExtractedFrom source fragments carrying the source node's scope relation.
#[test]
fn source_fragment_carries_source_scope() {
    let mut engine = engine();
    let source = ingest(
        &mut engine,
        "scope source",
        KnowledgeType::Episodic,
        "project/main",
    );
    let knowledge = ingest(
        &mut engine,
        "scope knowledge",
        KnowledgeType::Semantic,
        "project/main",
    );
    let tension = ingest(
        &mut engine,
        "scope tension",
        KnowledgeType::Semantic,
        "project/main",
    );
    engine
        .link(knowledge, source, EdgeType::ExtractedFrom, 1.0)
        .unwrap();
    // Create a Contradicts edge to trigger KnowledgeWithProvenance packaging mode
    engine
        .link(knowledge, tension, EdgeType::Contradicts, 0.8)
        .unwrap();

    let result = engine
        .search(SearchInput {
            text: "scope knowledge".to_string(),
            scope: ScopePath::new("project/main").expect("valid scope"),
            limit: 10,
            seed_limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    let source_fragment = result
        .package
        .memories
        .iter()
        .find(|f| f.node_id == source)
        .unwrap();

    assert_eq!(source_fragment.origin.scope.as_str(), "project/main");
    assert_eq!(source_fragment.scope, ScopeRelation::Exact);
}

/// Protects empty-candidate search from panicking or fabricating fragments.
#[test]
fn empty_seeds_no_panic() {
    let engine = engine();
    let result = engine
        .search(SearchInput {
            text: "missing".to_string(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(result.trace.seed_count, 0);
    assert_eq!(result.package.total_fragments(), 0);
}
