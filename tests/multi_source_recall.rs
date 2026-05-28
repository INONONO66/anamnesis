use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;
use anamnesis::{Engine, EngineConfig, IngestResult, NodeId, SpreadingModel};

fn origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: anamnesis::graph::ScopePath::new("test-project").expect("valid scope"),
        confidence: 1.0,
    }
}

fn observation(name: &str) -> Observation {
    observation_with_type(name, KnowledgeType::Semantic)
}

fn observation_with_type(name: &str, node_type: KnowledgeType) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("summary {name}")),
        content: name.to_string(),
        embedding: None,
        confidence: 1.0,
        node_type,
        entity_tags: Vec::new(),
        origin: origin(),
        timestamp: Timestamp(0),
    }
}

fn engine_config() -> EngineConfig {
    EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false)
}

fn ingest(engine: &mut Engine, name: &str) -> NodeId {
    match engine
        .ingest(observation(name))
        .expect("ingest should succeed")
    {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("dedup is disabled"),
        IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    }
}

fn knowledge_relevance(result: &anamnesis::query::SearchResult, node_id: NodeId) -> f64 {
    result
        .package
        .knowledge
        .iter()
        .find(|fragment| fragment.node_id == node_id)
        .map(|fragment| fragment.relevance)
        .expect("knowledge fragment should be present")
}

#[test]
fn recall_with_5_seeds_invokes_spread_once() {
    let mut engine = Engine::with_config(engine_config());
    for index in 0..5 {
        ingest(&mut engine, &format!("recall-topic-{index}"));
    }

    let result = engine
        .search(SearchInput {
            text: "recall-topic".to_string(),
            limit: 10,
            seed_limit: Some(5),
            ..Default::default()
        })
        .expect("search should succeed");

    assert_eq!(result.trace.seed_count, 5);
    assert_eq!(result.trace.spread_iterations, 1);
    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|strategy| strategy == "spreading_activation")
    );
}

#[test]
fn recall_passes_fused_scores_as_initial_activation() {
    let mut engine = Engine::with_config(engine_config());
    let first = ingest(&mut engine, "alpha first");
    let second = ingest(&mut engine, "alpha second");

    let result = engine
        .search(SearchInput {
            text: "alpha".to_string(),
            limit: 10,
            seed_limit: Some(2),
            scope: anamnesis::graph::ScopePath::new("test-project").expect("valid scope"),
            ..Default::default()
        })
        .expect("search should succeed");

    let first_relevance = knowledge_relevance(&result, first);
    let second_relevance = knowledge_relevance(&result, second);
    let expected_delta = 0.50 * ((1.0 / 61.0) - (1.0 / 62.0));
    let observed_delta = first_relevance - second_relevance;

    assert!(
        (observed_delta - expected_delta).abs() < 1e-8,
        "expected relevance delta from raw fused activations, got {observed_delta}"
    );
}

#[test]
fn recall_uses_rwr_when_configured() {
    let mut config = engine_config();
    config.spreading_model = SpreadingModel::RandomWalkRestart;
    let mut engine = Engine::with_config(config);
    let seed = ingest(&mut engine, "rwr seed");
    let neighbor = ingest(&mut engine, "rwr neighbor");
    engine
        .link(seed, neighbor, EdgeType::Semantic, 1.0)
        .expect("link should succeed");

    let result = engine
        .search(SearchInput {
            text: "rwr seed".to_string(),
            limit: 10,
            seed_limit: Some(1),
            ..Default::default()
        })
        .expect("search should succeed");

    assert_eq!(result.trace.spread_iterations, 1);
    assert!(
        result
            .package
            .knowledge
            .iter()
            .any(|fragment| fragment.node_id == neighbor)
    );
}

#[test]
fn priority_queue_bfs_uses_identity_prior_without_dropping_seed() {
    let mut engine = Engine::with_config(engine_config());
    let seed = ingest(&mut engine, "priority-prior-seed fact");
    let identity = match engine
        .ingest(observation_with_type(
            "persona anchor only",
            KnowledgeType::IdentityCore,
        ))
        .expect("ingest should succeed")
    {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("dedup is disabled"),
        IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    };

    let result = engine
        .search(SearchInput {
            text: "priority-prior-seed".to_string(),
            agent_id: Some("agent-1".to_string()),
            limit: 10,
            seed_limit: Some(1),
            ..Default::default()
        })
        .expect("search should succeed");

    assert!(
        result
            .package
            .knowledge
            .iter()
            .any(|fragment| fragment.node_id == seed),
        "text seed should remain activated"
    );
    assert!(
        result
            .package
            .identity
            .iter()
            .any(|fragment| fragment.node_id == identity),
        "identity prior node should be activated by PriorityQueue BFS"
    );
}

#[test]
fn recall_activated_count_matches_result_size() {
    let mut engine = Engine::with_config(engine_config());
    for index in 0..3 {
        ingest(&mut engine, &format!("size-topic-{index}"));
    }

    let result = engine
        .search(SearchInput {
            text: "size-topic".to_string(),
            limit: 10,
            seed_limit: Some(3),
            ..Default::default()
        })
        .expect("search should succeed");

    assert_eq!(result.trace.seed_count, 3);
    assert_eq!(result.trace.spread_iterations, 1);
    assert_eq!(result.package.total_fragments(), 3);
}
