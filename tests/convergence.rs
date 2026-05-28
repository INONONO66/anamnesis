//! Tests for top-k convergence termination in spreading activation.

use anamnesis::IngestResult;
use anamnesis::api::{Engine, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::{ConvergenceConfig, Query, QueryConfig};

#[test]
fn convergence_enabled_stops_early() {
    let mut engine = Engine::new();

    let origin = Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "test-session".into(),
        scope: ScopePath::universal(),
        confidence: 0.9,
    };

    let mut node_ids = Vec::new();
    for i in 0..100 {
        let result = engine
            .ingest(Observation {
                name: format!("node-{}", i),
                summary: None,
                content: format!("content for node {}", i),
                embedding: Some(vec![0.5; 768]),
                confidence: 0.8,
                node_type: KnowledgeType::Semantic,
                entity_tags: vec![],
                origin: origin.clone(),
                timestamp: Timestamp::now(),
            })
            .expect("ingest failed");

        let id = match result {
            anamnesis::api::IngestResult::Created(ids) => ids[0],
            anamnesis::api::IngestResult::Reinforced { existing_id, .. } => existing_id,
            IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
        };
        node_ids.push(id);
    }

    for i in 0..node_ids.len() - 1 {
        engine
            .link(node_ids[i], node_ids[i + 1], EdgeType::Semantic, 0.8)
            .expect("link failed");
    }

    let seed = node_ids[0];
    let mut config_with_convergence = QueryConfig::default();
    config_with_convergence.budget = 500;
    config_with_convergence.max_hops = 20;
    config_with_convergence.convergence = Some(ConvergenceConfig {
        stable_rounds: 2,
        compare_top_k: 10,
        min_delta: 0.01,
    });

    let result = engine
        .query(
            &Query::Associative { seed, budget: 500 },
            &config_with_convergence,
        )
        .expect("query failed");

    assert!(
        !result.knowledge.is_empty(),
        "should have retrieved some nodes"
    );
}

#[test]
fn convergence_disabled_default_behavior() {
    let mut engine = Engine::new();

    let origin = Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "test-session".into(),
        scope: ScopePath::universal(),
        confidence: 0.9,
    };

    let mut node_ids = Vec::new();
    for i in 0..50 {
        let result = engine
            .ingest(Observation {
                name: format!("node-{}", i),
                summary: None,
                content: format!("content for node {}", i),
                embedding: Some(vec![0.5; 768]),
                confidence: 0.8,
                node_type: KnowledgeType::Semantic,
                entity_tags: vec![],
                origin: origin.clone(),
                timestamp: Timestamp::now(),
            })
            .expect("ingest failed");

        let id = match result {
            anamnesis::api::IngestResult::Created(ids) => ids[0],
            anamnesis::api::IngestResult::Reinforced { existing_id, .. } => existing_id,
            IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
        };
        node_ids.push(id);
    }

    for i in 0..node_ids.len() - 1 {
        engine
            .link(node_ids[i], node_ids[i + 1], EdgeType::Semantic, 0.8)
            .expect("link failed");
    }

    let seed = node_ids[0];
    let mut config_no_convergence = QueryConfig::default();
    config_no_convergence.budget = 500;
    config_no_convergence.max_hops = 20;
    config_no_convergence.convergence = None;

    let result = engine
        .query(
            &Query::Associative { seed, budget: 500 },
            &config_no_convergence,
        )
        .expect("query failed");

    assert!(
        !result.knowledge.is_empty(),
        "should have retrieved some nodes"
    );
}

#[test]
fn convergence_config_default() {
    let config = ConvergenceConfig::default();
    assert_eq!(config.stable_rounds, 3);
    assert_eq!(config.compare_top_k, 10);
    assert_eq!(config.min_delta, 0.01);
}

#[test]
fn query_config_default_convergence_none() {
    let config = QueryConfig::default();
    assert!(config.convergence.is_none());
}
