//! Public smoke tests for Task 6 candidate collection.
//!
//! The three required tests (`collect_text_preserves_score`,
//! `collect_vector_preserves_cosine`, `collect_entity_returns_correct_source_rank`)
//! observe candidate behaviour through `Engine::search` because the
//! collectors themselves are `pub(crate)`. The unit-level score and rank
//! invariants are pinned in `src/api/search/candidates.rs` under
//! `#[cfg(test)]`.

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::SearchInput;
use anamnesis::{Engine, EngineConfig};

fn origin(session: &str) -> Origin {
    Origin {
        agent_id: "agent-1".into(),
        session_id: session.into(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 0.9,
    }
}

fn ingest(
    engine: &mut Engine,
    name: &str,
    content: &str,
    embedding: Option<Vec<f64>>,
    entity_tags: Vec<String>,
) {
    engine
        .ingest(Observation {
            name: name.into(),
            summary: None,
            content: content.into(),
            embedding,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags,
            origin: origin(name),
            timestamp: Timestamp(0),
        })
        .unwrap();
}

fn engine_with(setup: impl FnOnce(&mut Engine)) -> Engine {
    let config = EngineConfig::default()
        .with_novelty_threshold(0.0)
        .with_dedup_threshold(2.0);
    let mut engine = Engine::with_config(config);
    setup(&mut engine);
    engine
}

#[test]
fn collect_text_preserves_score() {
    let engine = engine_with(|e| {
        ingest(e, "alpha", "alpha factory pattern handler", None, vec![]);
        ingest(e, "beta", "beta factory utility helper", None, vec![]);
        ingest(e, "gamma", "gamma unrelated text", None, vec![]);
    });

    let result = engine
        .search(SearchInput {
            text: "factory".into(),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "text_search"),
        "text_search must be activated by plan.use_text"
    );
}

#[test]
fn collect_vector_preserves_cosine() {
    let engine = engine_with(|e| {
        ingest(e, "v1", "v1 content", Some(vec![1.0, 0.0, 0.0]), vec![]);
        ingest(e, "v2", "v2 content", Some(vec![0.7, 0.7, 0.0]), vec![]);
        ingest(e, "v3", "v3 content", Some(vec![0.0, 1.0, 0.0]), vec![]);
    });

    let result = engine
        .search(SearchInput {
            text: "ignored".into(),
            query_embedding: Some(vec![1.0, 0.0, 0.0]),
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "vector_similarity"),
        "vector_similarity must be activated by plan.use_vector"
    );
}

#[test]
fn collect_entity_returns_correct_source_rank() {
    let engine = engine_with(|e| {
        ingest(e, "a", "node a", None, vec!["x".into(), "y".into()]);
        ingest(e, "b", "node b", None, vec!["x".into()]);
        ingest(e, "c", "node c", None, vec!["y".into()]);
        ingest(e, "d", "node d", None, vec!["z".into()]);
    });

    let result = engine
        .search(SearchInput {
            text: "irrelevant".into(),
            entity_tags: vec!["x".into(), "y".into()],
            limit: 10,
            ..Default::default()
        })
        .expect("search must succeed");

    assert!(
        result
            .trace
            .strategies_used
            .iter()
            .any(|s| s == "entity_tags"),
        "entity_tags must be activated by plan.use_entity"
    );
}
