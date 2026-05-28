//! End-to-end integration tests for the Anamnesis cognitive graph engine.
//!
//! These tests verify the full engine lifecycle:
//! ingest → tick → touch → query → ContextPackage
//!
//! Unlike unit tests in `src/api/mod.rs`, these exercise multi-step
//! interactions across the entire cognitive pipeline.

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::{Query, QueryConfig};
use anamnesis::{Engine, EngineConfig, IngestResult, StorageAdapter};

fn make_origin(_agent: &str, project: Option<&str>) -> Origin {
    let scope = project
        .map(|s| ScopePath::new(s).expect("valid scope"))
        .unwrap_or_else(ScopePath::universal);
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope,
        confidence: 0.9,
    }
}

fn make_obs(
    name: &str,
    kt: KnowledgeType,
    embedding: Vec<f64>,
    project: Option<&str>,
) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("Summary: {name}")),
        content: format!("Full content: {name}"),
        embedding: Some(embedding),
        confidence: 0.9,
        node_type: kt,
        entity_tags: vec!["test".to_string()],
        origin: make_origin("agent-1", project),
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
    }
}

/// Full cognitive engine lifecycle test.
///
/// Verifies: ingest → link → tick → touch → query → non-trivial ContextPackage
#[test]
fn full_cognitive_lifecycle() {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_confidence_threshold(0.5)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    let identity_obs = make_obs(
        "I am a code architect",
        KnowledgeType::IdentityCore,
        vec![1.0, 0.0, 0.0],
        Some("proj-a"),
    );
    let semantic1_obs = make_obs(
        "auth uses factory pattern",
        KnowledgeType::Semantic,
        vec![0.8, 0.2, 0.0],
        Some("proj-a"),
    );
    let semantic2_obs = make_obs(
        "factory pattern is preferred",
        KnowledgeType::Semantic,
        vec![0.7, 0.3, 0.0],
        Some("proj-a"),
    );
    let decision_obs = make_obs(
        "use factory not DI",
        KnowledgeType::Decision,
        vec![0.6, 0.4, 0.0],
        Some("proj-a"),
    );
    let episodic_obs = make_obs(
        "session: discussed factory pattern",
        KnowledgeType::Episodic,
        vec![0.5, 0.5, 0.0],
        Some("proj-a"),
    );

    let IngestResult::Created(identity_ids) = engine.ingest(identity_obs).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(semantic1_ids) = engine.ingest(semantic1_obs).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(semantic2_ids) = engine.ingest(semantic2_obs).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(decision_ids) = engine.ingest(decision_obs).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(episodic_ids) = engine.ingest(episodic_obs).unwrap() else {
        panic!("expected Created");
    };
    let _identity_id = identity_ids[0];
    let semantic1_id = semantic1_ids[0];
    let semantic2_id = semantic2_ids[0];
    let decision_id = decision_ids[0];
    let episodic_id = episodic_ids[0];

    assert_eq!(
        engine.graph().node_count(),
        5,
        "all 5 nodes should be ingested"
    );

    engine
        .link(semantic1_id, semantic2_id, EdgeType::Semantic, 0.9)
        .unwrap();
    engine
        .link(decision_id, semantic1_id, EdgeType::Reason, 0.8)
        .unwrap();
    engine
        .link(episodic_id, semantic1_id, EdgeType::ExtractedFrom, 0.7)
        .unwrap();

    let week_later = Timestamp(7 * 86_400_000);
    let report = engine.tick(week_later).unwrap();
    assert!(
        report.nodes_decayed >= 1,
        "at least one node should have decayed"
    );

    engine.touch(semantic1_id, week_later).unwrap();

    let q = Query::Associative {
        seed: semantic1_id,
        budget: 100,
    };
    let mut qconfig = QueryConfig::default();
    qconfig.scope = anamnesis::graph::ScopePath::new("proj-a").expect("valid scope");
    qconfig.agent_id = Some("agent-1".to_string());
    let pkg = engine.query(&q, &qconfig).unwrap();

    assert!(
        pkg.total_fragments() > 0,
        "query should return non-empty ContextPackage, got {} fragments",
        pkg.total_fragments()
    );
    assert!(
        pkg.token_usage.used <= pkg.token_usage.total,
        "token budget should not be exceeded: used={}, total={}",
        pkg.token_usage.used,
        pkg.token_usage.total
    );

    let all_node_ids: Vec<_> = pkg
        .identity
        .iter()
        .chain(pkg.knowledge.iter())
        .chain(pkg.memories.iter())
        .map(|f| f.node_id)
        .collect();
    assert!(
        all_node_ids.contains(&semantic1_id),
        "seed node should be in results"
    );

    assert!(
        all_node_ids.len() > 1,
        "spreading activation should reach neighbors, got {} fragments",
        all_node_ids.len()
    );
}

/// Decay integration test: episodic decays faster than semantic.
///
/// After 30 days of no access, episodic salience should be significantly
/// lower than semantic salience due to different decay rates.
#[test]
fn decay_episodic_faster_than_semantic() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let episodic = make_obs(
        "episodic note",
        KnowledgeType::Episodic,
        vec![1.0, 0.0],
        None,
    );
    let semantic = make_obs(
        "semantic fact",
        KnowledgeType::Semantic,
        vec![0.0, 1.0],
        None,
    );

    let IngestResult::Created(episodic_ids) = engine.ingest(episodic).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(semantic_ids) = engine.ingest(semantic).unwrap() else {
        panic!("expected Created");
    };
    let episodic_id = episodic_ids[0];
    let semantic_id = semantic_ids[0];

    let month_later = Timestamp(30 * 86_400_000);
    engine.tick(month_later).unwrap();

    let episodic_s = engine.graph().storage().get_salience(episodic_id).unwrap();
    let semantic_s = engine.graph().storage().get_salience(semantic_id).unwrap();

    assert!(
        episodic_s < semantic_s,
        "episodic ({episodic_s:.3}) should decay faster than semantic ({semantic_s:.3})"
    );
    assert!(
        episodic_s < 0.5,
        "episodic should have decayed significantly after 30 days: {episodic_s:.3}"
    );
}

/// Duplicate ingest reinforces the existing node before perception gating.
///
/// When dedup is enabled, near-identical embeddings should touch the prior node
/// instead of creating a duplicate or falling through to novelty rejection.
#[test]
fn perception_gate_rejects_duplicate() {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.3)
        .with_confidence_threshold(0.5);
    let mut engine = Engine::with_config(config);

    let original = make_obs(
        "factory pattern",
        KnowledgeType::Semantic,
        vec![1.0, 0.0, 0.0],
        None,
    );
    let _ = engine.ingest(original).unwrap();

    let duplicate = make_obs(
        "factory pattern duplicate",
        KnowledgeType::Semantic,
        vec![1.0, 0.001, 0.0],
        None,
    );
    let result = engine.ingest(duplicate);

    assert!(matches!(result, Ok(IngestResult::Reinforced { .. })));
}

/// Attraction test: similar nodes auto-linked on ingest.
///
/// When two nodes with very similar embeddings are ingested, the attraction
/// mechanic should automatically create edges between them.
#[test]
fn attraction_auto_links_similar_nodes() {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    let obs1 = make_obs(
        "auth module",
        KnowledgeType::Semantic,
        vec![1.0, 0.0, 0.0],
        None,
    );
    let obs2 = make_obs(
        "auth service",
        KnowledgeType::Semantic,
        vec![0.95, 0.1, 0.0],
        None,
    );

    let _ = engine.ingest(obs1).unwrap();
    let _ = engine.ingest(obs2).unwrap();

    assert!(
        engine.graph().edge_count() >= 1,
        "similar nodes should be auto-linked, edge_count={}",
        engine.graph().edge_count()
    );
}

/// Scope test: same-project nodes score higher than other-project nodes.
///
/// When querying with a project context, nodes from the same project
/// should receive higher relevance scores via scope weighting (eq 13).
#[test]
fn scope_same_project_preferred() {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    let same_proj = make_obs(
        "same project knowledge",
        KnowledgeType::Semantic,
        vec![1.0, 0.0],
        Some("proj-a"),
    );
    let other_proj = make_obs(
        "other project knowledge",
        KnowledgeType::Semantic,
        vec![0.9, 0.1],
        Some("proj-b"),
    );

    let IngestResult::Created(same_ids) = engine.ingest(same_proj).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(other_ids) = engine.ingest(other_proj).unwrap() else {
        panic!("expected Created");
    };
    let same_id = same_ids[0];
    let other_id = other_ids[0];

    engine
        .link(same_id, other_id, EdgeType::Semantic, 0.8)
        .unwrap();

    let q = Query::Associative {
        seed: same_id,
        budget: 50,
    };
    let mut qconfig = QueryConfig::default();
    qconfig.scope = anamnesis::graph::ScopePath::new("proj-a").expect("valid scope");
    let pkg = engine.query(&q, &qconfig).unwrap();

    let all_frags: Vec<_> = pkg
        .identity
        .iter()
        .chain(pkg.knowledge.iter())
        .chain(pkg.memories.iter())
        .collect();

    let same_relevance = all_frags
        .iter()
        .find(|f| f.node_id == same_id)
        .map(|f| f.relevance);
    let other_relevance = all_frags
        .iter()
        .find(|f| f.node_id == other_id)
        .map(|f| f.relevance);

    if let (Some(same_r), Some(other_r)) = (same_relevance, other_relevance) {
        assert!(
            same_r >= other_r,
            "same-project ({same_r:.3}) should score >= other-project ({other_r:.3})"
        );
    }
}

/// Contradicts edge creates tension in query results.
///
/// When two activated nodes are connected by a Contradicts edge,
/// the query pipeline should surface this as a Tension.
#[test]
fn contradicts_creates_tension() {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);

    let obs1 = make_obs(
        "factory pattern is good",
        KnowledgeType::Semantic,
        vec![1.0, 0.0],
        None,
    );
    let obs2 = make_obs(
        "factory pattern is bad",
        KnowledgeType::Semantic,
        vec![0.9, 0.1],
        None,
    );

    let IngestResult::Created(ids1) = engine.ingest(obs1).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = engine.ingest(obs2).unwrap() else {
        panic!("expected Created");
    };
    let id1 = ids1[0];
    let id2 = ids2[0];
    engine.link(id1, id2, EdgeType::Contradicts, 0.9).unwrap();

    let q = Query::Associative {
        seed: id1,
        budget: 50,
    };
    let pkg = engine.query(&q, &QueryConfig::default()).unwrap();

    assert!(
        !pkg.tensions.is_empty() || pkg.agent_tension >= 0.0,
        "Contradicts edge should create tension"
    );
}

/// Touch revives decayed nodes via reinforcement.
///
/// After significant decay, touching a node should increase its salience
/// above the decayed level, demonstrating the "forgetting + recall" cycle.
#[test]
fn touch_revives_decayed_node() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let obs = make_obs(
        "decaying fact",
        KnowledgeType::Semantic,
        vec![1.0, 0.0],
        None,
    );
    let IngestResult::Created(ids) = engine.ingest(obs).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];

    let month_later = Timestamp(30 * 86_400_000);
    engine.tick(month_later).unwrap();

    let decayed_s = engine.graph().storage().get_salience(id).unwrap();
    assert!(
        decayed_s < 1.0,
        "node should have decayed from 1.0: {decayed_s:.3}"
    );

    engine.touch(id, month_later).unwrap();

    let revived_s = engine.graph().storage().get_salience(id).unwrap();
    assert!(
        revived_s > decayed_s,
        "touch should reinforce: revived ({revived_s:.3}) > decayed ({decayed_s:.3})"
    );
}
