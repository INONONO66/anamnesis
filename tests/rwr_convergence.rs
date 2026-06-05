//! Additive directed RWR convergence and conservation invariants (Phase 3).
//!
//! The retrieval flow is `a_next = alpha*seed + (1-alpha)*transpose(P)*a` over
//! row-stochastic conductance transitions (ADR-0005). Seed mass is L1-normalized
//! and `P` is row-stochastic, so total response mass is conserved and the operator
//! converges geometrically to a unique fixed point.

use std::collections::HashMap;

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::additive_rwr;
use anamnesis::{Engine, EngineConfig, IngestResult, NodeId};

fn make_observation(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("RWR test node: {name}"),
        embedding: None,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 1.0,
        },
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
    }
}

fn make_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn ingest_node(engine: &mut Engine, name: &str) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(make_observation(name)).unwrap() else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

#[test]
fn rwr_conserves_mass_and_converges() {
    let mut engine = make_engine();
    let seed = ingest_node(&mut engine, "seed");
    let mid = ingest_node(&mut engine, "mid");
    let leaf = ingest_node(&mut engine, "leaf");

    engine.link(seed, mid, EdgeType::Semantic, 1.0).unwrap();
    engine.link(mid, leaf, EdgeType::Semantic, 1.0).unwrap();

    let response = additive_rwr(
        &HashMap::from([(seed, 1.0)]),
        engine.graph().storage(),
        Timestamp(0),
    );

    let total: f64 = response.activation.values().sum();
    assert!(
        (total - 1.0).abs() < 1e-8,
        "probability leaked: {total} (iters={})",
        response.iterations
    );
    assert!(response.activation.values().all(|score| score.is_finite()));
    assert!(!response.truncated, "RWR must converge within the bound");
    assert!(response.residual < 1e-9, "residual {} too large", response.residual);
}

#[test]
fn rwr_seed_has_highest_activation() {
    let mut engine = make_engine();
    let seed = ingest_node(&mut engine, "seed");
    let left = ingest_node(&mut engine, "left");
    let right = ingest_node(&mut engine, "right");

    engine.link(seed, left, EdgeType::Semantic, 1.0).unwrap();
    engine.link(seed, right, EdgeType::Semantic, 1.0).unwrap();

    let response = additive_rwr(
        &HashMap::from([(seed, 1.0)]),
        engine.graph().storage(),
        Timestamp(0),
    );
    let seed_score = response.activation.get(&seed).copied().unwrap_or(0.0);

    assert!(seed_score > response.activation.get(&left).copied().unwrap_or(0.0));
    assert!(seed_score > response.activation.get(&right).copied().unwrap_or(0.0));
    assert!(response.activation.get(&left).copied().unwrap_or(0.0) > 0.0);
    assert!(response.activation.get(&right).copied().unwrap_or(0.0) > 0.0);
}

#[test]
fn rwr_is_idempotent() {
    let mut engine = make_engine();
    let seed = ingest_node(&mut engine, "seed");
    let a = ingest_node(&mut engine, "a");
    let b = ingest_node(&mut engine, "b");
    engine.link(seed, a, EdgeType::Reason, 1.0).unwrap();
    engine.link(a, b, EdgeType::Temporal, 1.0).unwrap();

    let storage = engine.graph().storage();
    let first = additive_rwr(&HashMap::from([(seed, 1.0)]), storage, Timestamp(0));
    let second = additive_rwr(&HashMap::from([(seed, 1.0)]), storage, Timestamp(0));
    assert_eq!(first.activation, second.activation);
}
