use std::collections::HashMap;

use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::engine::NodeId;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeId, EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::{ContextPackage, Query, QueryConfig, additive_rwr};

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn observation(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: name.to_string(),
        embedding: None,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: ScopePath::universal(),
            confidence: 1.0,
        },
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
    }
}

fn ingest(engine: &mut Engine, name: &str) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name)).expect("ingest succeeds")
    else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

fn set_edge_validity(
    engine: &mut Engine,
    edge_id: EdgeId,
    valid_from: Option<Timestamp>,
    valid_until: Option<Timestamp>,
) {
    let edge = engine
        .graph_mut()
        .get_edge_mut(edge_id)
        .expect("edge exists");
    edge.valid_from = valid_from;
    edge.valid_until = valid_until;
}

fn associative_query(engine: &Engine, seed: NodeId, now: Timestamp) -> ContextPackage {
    let mut config = QueryConfig::default();
    config.now = Some(now);
    engine
        .query(&Query::Associative { seed, budget: 20 }, &config)
        .expect("query succeeds")
}

fn package_contains(package: &ContextPackage, node_id: NodeId) -> bool {
    package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .any(|fragment| fragment.node_id == node_id)
}

#[test]
fn expired_edge_skipped_during_recall() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let expired = ingest(&mut engine, "expired target");
    let edge = engine.link(seed, expired, EdgeType::Semantic).unwrap();
    set_edge_validity(&mut engine, edge, None, Some(Timestamp(5)));

    let package = associative_query(&engine, seed, Timestamp(10));

    assert!(package_contains(&package, seed));
    assert!(!package_contains(&package, expired));
}

#[test]
fn valid_edge_is_traversable() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let target = ingest(&mut engine, "valid target");
    let edge = engine.link(seed, target, EdgeType::Semantic).unwrap();
    set_edge_validity(&mut engine, edge, Some(Timestamp(5)), Some(Timestamp(15)));

    let package = associative_query(&engine, seed, Timestamp(10));

    assert!(package_contains(&package, target));
}

#[test]
fn rwr_excludes_invalid_transitions() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let valid = ingest(&mut engine, "valid");
    let expired = ingest(&mut engine, "expired");

    engine.link(seed, valid, EdgeType::Semantic).unwrap();
    let expired_edge = engine.link(seed, expired, EdgeType::Semantic).unwrap();
    set_edge_validity(&mut engine, expired_edge, None, Some(Timestamp(5)));

    let response = additive_rwr(
        &HashMap::from([(seed, 1.0)]),
        engine.graph().storage(),
        Timestamp(10),
    );
    let scores = &response.activation;

    assert!(scores.get(&valid).copied().unwrap_or(0.0) > 0.0);
    assert_eq!(scores.get(&expired).copied().unwrap_or(0.0), 0.0);
}

#[test]
fn expired_contradicts_does_not_create_tension() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "claim a");
    let target = ingest(&mut engine, "claim b");

    engine.link(seed, target, EdgeType::Semantic).unwrap();
    let contradiction = engine.link(seed, target, EdgeType::Contradicts).unwrap();
    set_edge_validity(&mut engine, contradiction, None, Some(Timestamp(5)));

    let package = associative_query(&engine, seed, Timestamp(10));

    assert!(package_contains(&package, target));
    assert!(package.tensions.is_empty());
}

#[test]
fn edges_without_validity_bounds_are_always_valid() {
    let config = EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false);
    let mut engine = Engine::with_config(config);
    let seed = ingest(&mut engine, "seed");
    let target = ingest(&mut engine, "unbounded target");
    engine.link(seed, target, EdgeType::Semantic).unwrap();

    let package = associative_query(&engine, seed, Timestamp(999));

    assert!(package_contains(&package, target));
}

#[test]
fn edge_excluded_at_exact_valid_until_boundary() {
    // Validity is half-open [valid_from, valid_until): the upper bound is
    // EXCLUSIVE, so an edge is not valid at as_of == valid_until. This pins the
    // Phase 0 gate-unification fix (edge_valid_at previously used `<=` here).
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let target = ingest(&mut engine, "boundary target");
    let edge = engine.link(seed, target, EdgeType::Semantic).unwrap();
    set_edge_validity(&mut engine, edge, None, Some(Timestamp(10)));

    let package = associative_query(&engine, seed, Timestamp(10));

    assert!(package_contains(&package, seed));
    assert!(
        !package_contains(&package, target),
        "edge must be invalid at as_of == valid_until (half-open upper bound is exclusive)"
    );
}

#[test]
fn edge_valid_at_exact_valid_from_boundary() {
    // The lower bound is INCLUSIVE: an edge is valid at as_of == valid_from.
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let target = ingest(&mut engine, "from-boundary target");
    let edge = engine.link(seed, target, EdgeType::Semantic).unwrap();
    set_edge_validity(&mut engine, edge, Some(Timestamp(10)), Some(Timestamp(20)));

    let package = associative_query(&engine, seed, Timestamp(10));

    assert!(
        package_contains(&package, target),
        "edge must be valid at as_of == valid_from (half-open lower bound is inclusive)"
    );
}
