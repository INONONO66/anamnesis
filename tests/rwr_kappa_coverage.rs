use std::collections::HashMap;

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::random_walk_restart_from_distribution;
use anamnesis::{Engine, EngineConfig, IngestResult, NodeId};

fn observation(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("RWR kappa test node: {name}"),
        embedding: None,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 1.0,
        },
        timestamp: Timestamp(0),
    }
}

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn ingest(engine: &mut Engine, name: &str) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name)).unwrap() else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

fn one_step_scores(engine: &Engine, seed: NodeId) -> HashMap<NodeId, f64> {
    random_walk_restart_from_distribution(
        &HashMap::from([(seed, 1.0)]),
        None,
        0.0,
        1,
        engine.graph().storage(),
    )
}

fn assert_ratio(
    scores: &HashMap<NodeId, f64>,
    numerator: NodeId,
    denominator: NodeId,
    expected: f64,
) {
    let numerator_score = scores.get(&numerator).copied().unwrap_or(0.0);
    let denominator_score = scores.get(&denominator).copied().unwrap_or(0.0);
    let actual = numerator_score / denominator_score;
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected ratio {expected}, got {actual} ({numerator_score}/{denominator_score})"
    );
}

#[test]
fn rwr_supersedes_forward_kappa_120() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let control = ingest(&mut engine, "control");
    let newer = ingest(&mut engine, "newer");

    engine.link(seed, control, EdgeType::Semantic, 1.0).unwrap();
    engine.link(seed, newer, EdgeType::Supersedes, 1.0).unwrap();

    let scores = one_step_scores(&engine, seed);
    assert_ratio(&scores, newer, control, 1.20);
}

#[test]
fn rwr_supersedes_backward_kappa_040() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let control = ingest(&mut engine, "control");
    let newer = ingest(&mut engine, "newer");

    engine.link(seed, control, EdgeType::Semantic, 1.0).unwrap();
    engine.link(newer, seed, EdgeType::Supersedes, 1.0).unwrap();

    let scores = one_step_scores(&engine, seed);
    assert_ratio(&scores, newer, control, 0.40);
}

#[test]
fn rwr_refutes_kappa_030() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let control = ingest(&mut engine, "control");
    let refuting = ingest(&mut engine, "refuting");

    engine.link(seed, control, EdgeType::Semantic, 1.0).unwrap();
    engine.link(seed, refuting, EdgeType::Refutes, 1.0).unwrap();

    let scores = one_step_scores(&engine, seed);
    assert_ratio(&scores, refuting, control, 0.30);
}

#[test]
fn rwr_contradicts_excluded() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let control = ingest(&mut engine, "control");
    let contradiction = ingest(&mut engine, "contradiction");

    engine.link(seed, control, EdgeType::Semantic, 1.0).unwrap();
    engine
        .link(seed, contradiction, EdgeType::Contradicts, 1.0)
        .unwrap();

    let scores = one_step_scores(&engine, seed);
    assert!(scores.get(&control).copied().unwrap_or(0.0) > 0.0);
    assert_eq!(scores.get(&contradiction).copied().unwrap_or(0.0), 0.0);
}

#[test]
fn rwr_identity_conditioning() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let identity_biased = ingest(&mut engine, "identity-biased");

    let scores = random_walk_restart_from_distribution(
        &HashMap::from([(seed, 1.0)]),
        Some(&HashMap::from([(identity_biased, 1.0)])),
        0.15,
        0,
        engine.graph().storage(),
    );

    let seed_score = scores.get(&seed).copied().unwrap_or(0.0);
    let identity_score = scores.get(&identity_biased).copied().unwrap_or(0.0);
    let total: f64 = scores.values().sum();

    assert!(identity_score > 0.0);
    assert!(seed_score > identity_score);
    assert!((identity_score - (0.10 / 1.10)).abs() < 1e-12);
    assert!((total - 1.0).abs() < 1e-12);
}
