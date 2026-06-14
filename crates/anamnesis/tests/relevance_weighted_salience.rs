//! Crystallization synthesizes its retained-action reservoir as the additive,
//! relevance-weighted average of the SOURCE reservoirs (overview.md `crystallize`
//! contract, interactions.md `Crystallized`), then projects salience from it
//! (ADR-0002). It never mutates a source.
//!
//! ```text
//! A_s_new = (sum_i relevance_i * A_source_i) / sum_i relevance_i
//!         + (2*confidence - 1) * REWARD_LOG_ODDS_SCALE
//! salience = project_salience(A_s_new) = logistic(A_s_new)
//! ```
//!
//! When all relevances are zero (weight_sum <= EPSILON) the weighting falls back to
//! the uniform mean of the source reservoirs.

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{CrystallizeRequest, IngestResult, NodeId, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};

/// Calibrated prior `REWARD_LOG_ODDS_SCALE` (mechanics::priors).
const REWARD_LOG_ODDS_SCALE: f64 = 4.0;

fn origin() -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "test-session".to_string(),
        scope: anamnesis::graph::ScopePath::new("test-project").expect("valid scope"),
        confidence: 0.9,
    }
}

/// Inserts a source and sets its retained-action reservoir `A_i` directly.
///
/// `set_retained_action` writes the authoritative reservoir and re-projects
/// salience, so crystallization synthesizes from a coherent reservoir.
fn make_source(engine: &mut Engine, name: &str, retained_action: f64) -> NodeId {
    let result = engine
        .ingest(Observation {
            name: name.to_string(),
            summary: None,
            content: format!("Content for {name}"),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec![],
            origin: origin(),
            timestamp: Timestamp(1000),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
    let IngestResult::Created(ids) = result else {
        panic!("expected source creation");
    };
    let id = ids[0];
    engine
        .graph_mut()
        .storage_mut()
        .set_retained_action(id, retained_action)
        .unwrap();
    id
}

/// `project_salience(A) = logistic(A)`.
fn project_salience(a: f64) -> f64 {
    1.0 / (1.0 + (-a).exp())
}

/// Expected synthesis salience for the additive-synthesis rule.
fn expected_salience(actions: &[f64], relevances: &[f64], confidence: f64) -> f64 {
    let weight_sum: f64 = relevances.iter().sum();
    let avg = if weight_sum > f64::EPSILON {
        actions
            .iter()
            .zip(relevances)
            .map(|(a, r)| a * r)
            .sum::<f64>()
            / weight_sum
    } else {
        actions.iter().sum::<f64>() / actions.len() as f64
    };
    project_salience(avg + (2.0 * confidence - 1.0) * REWARD_LOG_ODDS_SCALE)
}

#[test]
fn opposing_relevances_produce_different_saliences() {
    let mut engine = Engine::new();

    let low_a = make_source(&mut engine, "low-a", 0.5);
    let high_a = make_source(&mut engine, "high-a", 3.0);
    let low_b = make_source(&mut engine, "low-b", 0.5);
    let high_b = make_source(&mut engine, "high-b", 3.0);

    let crystal_a = engine
        .crystallize(CrystallizeRequest {
            name: "crystal-a".to_string(),
            summary: None,
            content: "Synthesis A".to_string(),
            embedding: Some(vec![1.0, 0.0, 0.0]),
            source_ids: vec![low_a, high_a],
            source_relevances: Some(vec![0.9, 0.1]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.8,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    let crystal_b = engine
        .crystallize(CrystallizeRequest {
            name: "crystal-b".to_string(),
            summary: None,
            content: "Synthesis B".to_string(),
            embedding: Some(vec![0.0, 1.0, 0.0]),
            source_ids: vec![low_b, high_b],
            source_relevances: Some(vec![0.1, 0.9]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.8,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    let expected_a = expected_salience(&[0.5, 3.0], &[0.9, 0.1], 0.8);
    let expected_b = expected_salience(&[0.5, 3.0], &[0.1, 0.9], 0.8);
    assert!(
        (crystal_a.initial_salience - expected_a).abs() < 1e-9,
        "crystal_a salience: expected {expected_a}, got {}",
        crystal_a.initial_salience,
    );
    assert!(
        (crystal_b.initial_salience - expected_b).abs() < 1e-9,
        "crystal_b salience: expected {expected_b}, got {}",
        crystal_b.initial_salience,
    );

    // Weighting toward the higher-action source raises the synthesis salience.
    assert!(
        crystal_a.initial_salience < crystal_b.initial_salience,
        "relevance weighting should shift salience: A={} vs B={}",
        crystal_a.initial_salience,
        crystal_b.initial_salience,
    );
}

#[test]
fn exact_three_source_weighted_formula() {
    let mut engine = Engine::new();

    let src_1 = make_source(&mut engine, "src-1", 1.0);
    let src_2 = make_source(&mut engine, "src-2", 2.0);
    let src_3 = make_source(&mut engine, "src-3", 3.0);

    let result = engine
        .crystallize(CrystallizeRequest {
            name: "three-source crystal".to_string(),
            summary: None,
            content: "Synthesis from three sources".to_string(),
            embedding: Some(vec![0.5, 0.5, 0.0]),
            source_ids: vec![src_1, src_2, src_3],
            source_relevances: Some(vec![0.5, 0.3, 0.2]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.7,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    let expected = expected_salience(&[1.0, 2.0, 3.0], &[0.5, 0.3, 0.2], 0.7);
    assert!(
        (result.initial_salience - expected).abs() < 1e-9,
        "expected {expected}, got {}",
        result.initial_salience,
    );
}

#[test]
fn none_relevances_produce_uniform_weighting() {
    let mut engine = Engine::new();

    let src_1 = make_source(&mut engine, "src-1", 0.5);
    let src_2 = make_source(&mut engine, "src-2", 2.5);

    let result = engine
        .crystallize(CrystallizeRequest {
            name: "uniform crystal".to_string(),
            summary: None,
            content: "Synthesis with uniform weights".to_string(),
            embedding: None,
            source_ids: vec![src_1, src_2],
            source_relevances: None,
            node_type: KnowledgeType::Semantic,
            confidence: 0.8,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    // None => uniform relevance weights of 1.0 each.
    let expected = expected_salience(&[0.5, 2.5], &[1.0, 1.0], 0.8);
    assert!(
        (result.initial_salience - expected).abs() < 1e-9,
        "expected {expected}, got {}",
        result.initial_salience,
    );
}

#[test]
fn zero_relevances_fall_back_to_uniform_mean() {
    let mut engine = Engine::new();

    let src_1 = make_source(&mut engine, "src-1", 1.0);
    let src_2 = make_source(&mut engine, "src-2", 2.0);

    let result = engine
        .crystallize(CrystallizeRequest {
            name: "zero-weight crystal".to_string(),
            summary: None,
            content: "Synthesis with zero relevance weights".to_string(),
            embedding: None,
            source_ids: vec![src_1, src_2],
            source_relevances: Some(vec![0.0, 0.0]),
            node_type: KnowledgeType::Semantic,
            confidence: 0.9,
            origin: origin(),
            entity_tags: vec![],
            timestamp: Timestamp(2000),
        })
        .unwrap();

    // weight_sum <= EPSILON => fall back to the uniform mean of the reservoirs.
    let expected = expected_salience(&[1.0, 2.0], &[0.0, 0.0], 0.9);
    assert!(
        (result.initial_salience - expected).abs() < 1e-9,
        "expected {expected}, got {}",
        result.initial_salience,
    );
}
