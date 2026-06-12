use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::query::{ContextPackage, Fragment, Query, QueryConfig};

const DAY_MS: u64 = 86_400_000;

fn origin(_agent_id: &str, session_id: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session_id.to_string(),
        scope: anamnesis::graph::ScopePath::new("agent-memory-project").expect("valid scope"),
        confidence: 0.95,
    }
}

fn observation(
    name: &str,
    node_type: KnowledgeType,
    embedding: Option<Vec<f64>>,
    entity_tags: &[&str],
    session_id: &str,
    timestamp: u64,
) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("Agent memory fragment: {name}")),
        content: format!("The agent recorded that {name}."),
        embedding,
        confidence: 0.95,
        node_type,
        entity_tags: entity_tags.iter().map(|tag| tag.to_string()).collect(),
        origin: origin("agent-1", session_id),
        timestamp: Timestamp(timestamp),
        valid_from: None,
        valid_until: None,
    }
}

fn test_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn created_id(result: IngestResult) -> NodeId {
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("expected a newly created node"),
    }
}

fn fragments(package: &ContextPackage) -> Vec<&Fragment> {
    package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .collect()
}

fn names(package: &ContextPackage) -> Vec<String> {
    fragments(package)
        .into_iter()
        .map(|fragment| fragment.name.clone())
        .collect()
}

fn assert_contains_name(package: &ContextPackage, expected: &str, message: &str) {
    let actual = names(package);
    assert!(
        actual.iter().any(|name| name == expected),
        "{message}; expected '{expected}' in {actual:?}"
    );
}

fn assert_not_contains_name(package: &ContextPackage, unexpected: &str, message: &str) {
    let actual = names(package);
    assert!(
        actual.iter().all(|name| name != unexpected),
        "{message}; did not expect '{unexpected}' in {actual:?}"
    );
}

#[test]
fn agent_session_simulation_retrieves_relevant_conventions() {
    let mut engine = test_engine();

    engine
        .ingest(observation(
            "auth module uses factory pattern",
            KnowledgeType::Convention,
            Some(vec![1.0, 0.0, 0.0]),
            &["auth", "factory-pattern"],
            "session-1",
            0,
        ))
        .unwrap();
    engine
        .ingest(observation(
            "database uses repository pattern",
            KnowledgeType::Convention,
            Some(vec![0.0, 1.0, 0.0]),
            &["database", "repository-pattern"],
            "session-1",
            1,
        ))
        .unwrap();
    engine
        .ingest(observation(
            "prefer composition over inheritance",
            KnowledgeType::Convention,
            Some(vec![0.0, 0.0, 1.0]),
            &["design"],
            "session-1",
            2,
        ))
        .unwrap();

    engine
        .ingest(observation(
            "found race condition in auth middleware",
            KnowledgeType::Episodic,
            Some(vec![0.8, 0.1, 0.1]),
            &["auth", "middleware"],
            "session-2",
            3,
        ))
        .unwrap();
    engine
        .ingest(observation(
            "auth handler refactored to async",
            KnowledgeType::Semantic,
            Some(vec![0.7, 0.2, 0.1]),
            &["auth", "async"],
            "session-2",
            4,
        ))
        .unwrap();

    let mut auth_config = QueryConfig::default();
    auth_config.context = Some("auth".to_string());
    let auth_conventions = engine
        .query(
            &Query::TypeFiltered {
                node_type: KnowledgeType::Convention,
                limit: 1,
            },
            &auth_config,
        )
        .unwrap();

    assert_contains_name(
        &auth_conventions,
        "auth module uses factory pattern",
        "auth-focused convention retrieval should surface the auth convention",
    );
    assert_not_contains_name(
        &auth_conventions,
        "database uses repository pattern",
        "auth-focused convention retrieval should not include unrelated database convention",
    );

    let all_active = engine
        .query(
            &Query::List {
                min_salience: 0.5,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();

    assert_eq!(
        all_active.total_fragments(),
        5,
        "all five session nodes should be returned while initial salience is 1.0"
    );
}

#[test]
fn forgetting_removes_decayed_nodes_until_reinforced() {
    let mut engine = test_engine();
    let target = created_id(
        engine
            .ingest(observation(
                "auth cache invalidation incident",
                KnowledgeType::Semantic,
                None,
                &["auth"],
                "session-1",
                0,
            ))
            .unwrap(),
    );

    for i in 1..5 {
        engine
            .ingest(observation(
                &format!("background memory {i}"),
                KnowledgeType::Semantic,
                None,
                &["background"],
                "session-1",
                i,
            ))
            .unwrap();
    }

    // Seed the target's evidence prior P_i to zero so the decay→reinforce cycle is
    // observable rather than pinned at the saturation rail by the flat surprise
    // ceiling. Salience is logistic(B_i + P_i) (ADR-0008); a touch refreshes the
    // cache so the pre-tick reading reflects the new prior.
    engine
        .graph_mut()
        .storage_mut()
        .set_evidence_prior(target, 0.0)
        .unwrap();
    engine.touch(target, Timestamp(0)).unwrap();
    let s_target_before = engine.graph().storage().get_salience(target).unwrap();

    let after_thirty_days = Timestamp(30 * DAY_MS);
    engine.tick(after_thirty_days).unwrap();

    // Power-law dissipation strictly lowers the target's salience projection over
    // 30 days (the reservoir decayed; nothing was deleted).
    let s_target_decayed = engine.graph().storage().get_salience(target).unwrap();
    assert!(
        s_target_decayed < s_target_before,
        "30 days of forgetting should lower salience: {s_target_decayed} !< {s_target_before}"
    );
    // The site is NOT deleted — still addressable, just less salient (ADR-0008).
    assert!(engine.graph().get_node(target).is_ok());
    let active_before = engine
        .query(
            &Query::List {
                min_salience: 0.8,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();
    assert!(
        !active_before.knowledge.iter().any(|f| f.node_id == target),
        "decayed target should have dropped out of the active 0.8 set"
    );

    // Touching the target applies decay-before-reinforce, raising its retained
    // action (and thus salience) well above its decayed level.
    for _ in 0..4 {
        engine.touch(target, after_thirty_days).unwrap();
    }
    let s_target_reinforced = engine.graph().storage().get_salience(target).unwrap();
    assert!(
        s_target_reinforced > s_target_decayed,
        "reinforcement should raise the target above its decayed level: \
         {s_target_reinforced} !> {s_target_decayed}"
    );
    assert_contains_name(
        &engine
            .query(
                &Query::List {
                    min_salience: 0.0,
                    limit: 10,
                },
                &QueryConfig::default(),
            )
            .unwrap(),
        "auth cache invalidation incident",
        "reinforced target memory should remain addressable in the active set",
    );
}

#[test]
fn ingest_deduplicates_identical_embeddings_and_creates_distinct_memory() {
    let mut engine = Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_threshold(0.92)
            .with_dedup_enabled(true),
    );

    let first = engine
        .ingest(observation(
            "auth uses factory pattern",
            KnowledgeType::Semantic,
            Some(vec![1.0, 0.0, 0.0]),
            &["auth"],
            "session-1",
            0,
        ))
        .unwrap();
    let first_id = match first {
        IngestResult::Created(ids) => ids[0],
        other => panic!("first ingest should create a node, got {other:?}"),
    };

    let duplicate = engine
        .ingest(observation(
            "auth uses factory pattern again",
            KnowledgeType::Semantic,
            Some(vec![1.0, 0.0, 0.0]),
            &["auth"],
            "session-2",
            1,
        ))
        .unwrap();
    match duplicate {
        IngestResult::Reinforced {
            existing_id,
            similarity,
        } => {
            assert_eq!(
                existing_id, first_id,
                "duplicate ingest should reinforce the original auth node"
            );
            assert!(
                similarity > 0.92,
                "identical embeddings should exceed dedup threshold, got {similarity}"
            );
        }
        other => panic!("duplicate ingest should reinforce, got {other:?}"),
    }
    assert_eq!(
        engine.graph().node_count(),
        1,
        "dedup reinforcement must not allocate a second node"
    );

    let distinct = engine
        .ingest(observation(
            "database uses repository pattern",
            KnowledgeType::Semantic,
            Some(vec![0.0, 1.0, 0.0]),
            &["database"],
            "session-2",
            2,
        ))
        .unwrap();
    assert!(
        matches!(distinct, IngestResult::Created(_)),
        "different embedding should create a new node, got {distinct:?}"
    );
    assert_eq!(
        engine.graph().node_count(),
        2,
        "distinct embeddings should leave two nodes in memory"
    );
}

#[test]
fn goal_weighted_rerank_prioritizes_contextual_memory() {
    let mut auth_engine = test_engine();
    auth_engine
        .ingest(observation(
            "auth security module",
            KnowledgeType::Semantic,
            Some(vec![1.0, 0.0]),
            &["auth", "security"],
            "session-1",
            0,
        ))
        .unwrap();
    auth_engine
        .ingest(observation(
            "database migration tool",
            KnowledgeType::Semantic,
            Some(vec![0.0, 1.0]),
            &["database", "migration"],
            "session-1",
            1,
        ))
        .unwrap();

    let mut auth_config = QueryConfig::default();
    auth_config.context = Some("auth security".to_string());
    let auth_ranked = auth_engine
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 10,
            },
            &auth_config,
        )
        .unwrap();
    assert_eq!(
        auth_ranked.knowledge[0].name, "auth security module",
        "auth security context should rank the auth security node first"
    );

    let mut database_engine = test_engine();
    database_engine
        .ingest(observation(
            "database migration tool",
            KnowledgeType::Semantic,
            Some(vec![0.0, 1.0]),
            &["database", "migration"],
            "session-1",
            0,
        ))
        .unwrap();
    database_engine
        .ingest(observation(
            "auth security module",
            KnowledgeType::Semantic,
            Some(vec![1.0, 0.0]),
            &["auth", "security"],
            "session-1",
            1,
        ))
        .unwrap();

    let mut database_config = QueryConfig::default();
    database_config.context = Some("database migration".to_string());
    let database_ranked = database_engine
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 10,
            },
            &database_config,
        )
        .unwrap();
    assert_eq!(
        database_ranked.knowledge[0].name, "database migration tool",
        "database migration context should rank the database migration node first"
    );
}

#[test]
fn temporal_query_returns_only_nodes_since_timestamp() {
    let mut engine = test_engine();
    engine
        .ingest(observation(
            "old knowledge",
            KnowledgeType::Semantic,
            None,
            &["archive"],
            "session-1",
            100,
        ))
        .unwrap();
    engine
        .ingest(observation(
            "recent discovery",
            KnowledgeType::Semantic,
            None,
            &["recent"],
            "session-2",
            1000,
        ))
        .unwrap();

    let recent = engine
        .query(
            &Query::Temporal {
                since: Timestamp(500),
                node_types: None,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();
    assert_contains_name(
        &recent,
        "recent discovery",
        "temporal query since 500 should include recent discovery",
    );
    assert_not_contains_name(
        &recent,
        "old knowledge",
        "temporal query since 500 should exclude old knowledge",
    );

    let all = engine
        .query(
            &Query::Temporal {
                since: Timestamp(0),
                node_types: None,
                limit: 10,
            },
            &QueryConfig::default(),
        )
        .unwrap();
    assert_contains_name(
        &all,
        "old knowledge",
        "temporal query since 0 should include old knowledge",
    );
    assert_contains_name(
        &all,
        "recent discovery",
        "temporal query since 0 should include recent discovery",
    );
}

#[test]
fn neighborhood_query_expands_by_requested_depth() {
    let mut engine = test_engine();
    let a = created_id(
        engine
            .ingest(observation(
                "node A auth entrypoint",
                KnowledgeType::Semantic,
                None,
                &["auth"],
                "session-1",
                0,
            ))
            .unwrap(),
    );
    let b = created_id(
        engine
            .ingest(observation(
                "node B auth handler",
                KnowledgeType::Semantic,
                None,
                &["auth"],
                "session-1",
                1,
            ))
            .unwrap(),
    );
    let c = created_id(
        engine
            .ingest(observation(
                "node C database side effect",
                KnowledgeType::Semantic,
                None,
                &["database"],
                "session-1",
                2,
            ))
            .unwrap(),
    );
    engine.link(a, b, EdgeType::Semantic).unwrap();
    engine.link(b, c, EdgeType::Causal).unwrap();

    let depth_one = engine
        .query(
            &Query::Neighborhood {
                entity: a,
                depth: 1,
            },
            &QueryConfig::default(),
        )
        .unwrap();
    assert_contains_name(
        &depth_one,
        "node A auth entrypoint",
        "depth-1 neighborhood should include the seed node",
    );
    assert_contains_name(
        &depth_one,
        "node B auth handler",
        "depth-1 neighborhood should include directly linked node B",
    );
    assert_not_contains_name(
        &depth_one,
        "node C database side effect",
        "depth-1 neighborhood should not include two-hop node C",
    );

    let depth_two = engine
        .query(
            &Query::Neighborhood {
                entity: a,
                depth: 2,
            },
            &QueryConfig::default(),
        )
        .unwrap();
    assert_contains_name(
        &depth_two,
        "node A auth entrypoint",
        "depth-2 neighborhood should include the seed node",
    );
    assert_contains_name(
        &depth_two,
        "node B auth handler",
        "depth-2 neighborhood should include node B",
    );
    assert_contains_name(
        &depth_two,
        "node C database side effect",
        "depth-2 neighborhood should include two-hop node C",
    );
}

#[test]
fn hybrid_text_search_finds_memory_by_partial_text() {
    let mut engine = test_engine();
    engine
        .ingest(observation(
            "authentication middleware",
            KnowledgeType::Semantic,
            None,
            &["auth"],
            "session-1",
            0,
        ))
        .unwrap();
    engine
        .ingest(observation(
            "authorization service",
            KnowledgeType::Semantic,
            None,
            &["auth"],
            "session-1",
            1,
        ))
        .unwrap();
    engine
        .ingest(observation(
            "database connection pool",
            KnowledgeType::Semantic,
            None,
            &["database"],
            "session-1",
            2,
        ))
        .unwrap();

    let auth_results = engine.graph().storage().text_search("auth", 10);
    let auth_names: Vec<String> = auth_results
        .iter()
        .map(|(id, _)| engine.graph().storage().get_node(*id).unwrap().name.clone())
        .collect();
    assert!(
        auth_names.contains(&"authentication middleware".to_string()),
        "text_search('auth') should find authentication middleware, got {auth_names:?}"
    );
    assert!(
        auth_names.contains(&"authorization service".to_string()),
        "text_search('auth') should find authorization service, got {auth_names:?}"
    );

    let database_results = engine.graph().storage().text_search("database", 10);
    let database_names: Vec<String> = database_results
        .iter()
        .map(|(id, _)| engine.graph().storage().get_node(*id).unwrap().name.clone())
        .collect();
    assert_eq!(
        database_names,
        vec!["database connection pool".to_string()],
        "text_search('database') should only return the database memory"
    );

    let missing = engine.graph().storage().text_search("nonexistent", 10);
    assert!(
        missing.is_empty(),
        "text_search('nonexistent') should return no matches, got {missing:?}"
    );
}

#[test]
fn end_to_end_agent_memory_pipeline_surfaces_identity_and_relevant_context() {
    let mut engine = test_engine();
    let _identity = created_id(
        engine
            .ingest(observation(
                "I am a code architect",
                KnowledgeType::IdentityCore,
                Some(vec![0.9, 0.1, 0.0]),
                &["identity"],
                "session-1",
                0,
            ))
            .unwrap(),
    );
    let convention = created_id(
        engine
            .ingest(observation(
                "prefer factory pattern",
                KnowledgeType::Convention,
                Some(vec![0.9, 0.1, 0.0]),
                &["factory", "pattern"],
                "session-1",
                1,
            ))
            .unwrap(),
    );
    let auth_refactoring = created_id(
        engine
            .ingest(observation(
                "auth module needs refactoring",
                KnowledgeType::Semantic,
                Some(vec![0.85, 0.15, 0.05]),
                &["auth", "refactoring"],
                "session-1",
                2,
            ))
            .unwrap(),
    );
    engine
        .link(convention, auth_refactoring, EdgeType::Reason)
        .unwrap();

    engine.tick(Timestamp(30 * DAY_MS)).unwrap();

    let edges_before = engine.graph().edge_count();
    engine
        .ingest(observation(
            "refactored auth module today",
            KnowledgeType::Episodic,
            Some(vec![0.88, 0.12, 0.02]),
            &["auth", "refactoring"],
            "session-2",
            30 * DAY_MS + 1,
        ))
        .unwrap();
    assert!(
        engine.graph().edge_count() > edges_before,
        "similar auth-refactoring episode should auto-link into the existing graph"
    );

    let mut associative_config = QueryConfig::default();
    associative_config.agent_id = Some("agent-1".to_string());
    associative_config.scope =
        anamnesis::graph::ScopePath::new("agent-memory-project").expect("valid scope");
    associative_config.query_embedding = Some(vec![0.86, 0.14, 0.04]);
    let associative = engine
        .query(
            &Query::Associative {
                seed: auth_refactoring,
                budget: 50,
            },
            &associative_config,
        )
        .unwrap();
    assert!(
        !associative.knowledge.is_empty(),
        "associative query should return knowledge connected to auth refactoring"
    );
    assert_contains_name(
        &associative,
        "I am a code architect",
        "associative query with agent_id should include the agent identity prior",
    );

    let mut auth_config = QueryConfig::default();
    auth_config.context = Some("auth refactoring".to_string());
    let auth_ranked = engine
        .query(
            &Query::List {
                min_salience: 0.0,
                limit: 10,
            },
            &auth_config,
        )
        .unwrap();
    assert!(
        auth_ranked.knowledge[0].name.contains("auth")
            || auth_ranked.knowledge[0].name.contains("refactoring"),
        "auth refactoring context should rank an auth-related node first, got {:?}",
        auth_ranked.knowledge[0].name
    );

    let factory_results = engine.graph().storage().text_search("factory", 10);
    let factory_names: Vec<String> = factory_results
        .iter()
        .map(|(id, _)| engine.graph().storage().get_node(*id).unwrap().name.clone())
        .collect();
    assert!(
        factory_names.contains(&"prefer factory pattern".to_string()),
        "final text_search('factory') should retrieve the convention node, got {factory_names:?}"
    );
}
