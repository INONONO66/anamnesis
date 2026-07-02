//! Deterministic synthetic graph construction via the public engine API.
//! Instrumentation discipline: read-only paradigms use `activation_from` (no
//! touch/commit); time advances via `tick` in ms; decaying cohorts use
//! Episodic node_type + default Auto tier (never Core).

use std::collections::HashMap;

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult, NodeId};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::additive_rwr;

pub const DAY_MS: u64 = 86_400_000;
pub const T0: u64 = 1000;

/// Engine where every ingest allocates a fresh distinct node.
pub fn scenario_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false)
            .with_confidence_threshold(0.0),
    )
}

pub fn observation(name: &str, node_type: KnowledgeType) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "fidelity".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(T0),
        valid_from: None,
        valid_until: None,
    }
}

/// Ingest one node and return its id (panics unless a fresh node is created).
pub fn ingest(engine: &mut Engine, name: &str, node_type: KnowledgeType) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name, node_type)).unwrap() else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

/// Ingest one node BORN at `when`: its creation trace is stamped at `when` (the
/// engine seeds the creation trace from `observation.timestamp`). Used by the
/// spacing paradigm so the first study event IS the creation trace (Pavlik-Anderson
/// framing), with no synthetic day-0 trace ahead of it.
pub fn ingest_at(
    engine: &mut Engine,
    name: &str,
    node_type: KnowledgeType,
    when: Timestamp,
) -> NodeId {
    let mut obs = observation(name, node_type);
    obs.timestamp = when;
    let IngestResult::Created(ids) = engine.ingest(obs).unwrap() else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

/// Settled query-local activation a_i for `target` when RWR is seeded at `seed`.
pub fn activation_from(engine: &Engine, seed: NodeId, target: NodeId) -> f64 {
    let resp = additive_rwr(
        &HashMap::from([(seed, 1.0)]),
        engine.graph().storage(),
        Timestamp(T0),
    );
    resp.activation.get(&target).copied().unwrap_or(0.0)
}

/// Activation for `target` when RWR is seeded at MULTIPLE cues (priming additivity).
pub fn activation_from_many(engine: &Engine, seeds: &[NodeId], target: NodeId) -> f64 {
    let seed_map: HashMap<NodeId, f64> = seeds.iter().map(|&s| (s, 1.0)).collect();
    let resp = additive_rwr(&seed_map, engine.graph().storage(), Timestamp(T0));
    resp.activation.get(&target).copied().unwrap_or(0.0)
}

/// Bounded salience projection of a node.
pub fn salience(engine: &Engine, id: NodeId) -> f64 {
    engine.graph().get_node(id).unwrap().salience
}

pub fn day(n: u64) -> Timestamp {
    Timestamp(T0 + n * DAY_MS)
}
