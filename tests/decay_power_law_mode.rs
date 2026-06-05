//! Reservoir-space power-law dissipation (Phase 2 dynamics substrate).
//!
//! Forgetting is power-law base-level dissipation of the retained-action reservoir
//! `A_i` (ADR-0008); `salience = project_salience(A_i)` is a derived projection.
//! Power-law is the only decay model — `touch()`/`tick()` operate on `A_i`.

use anamnesis::Engine;
use anamnesis::api::{EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};

const DAY_MS: u64 = 86_400_000;

fn make_obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Episodic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

#[test]
fn touch_records_access_history() {
    let mut e = engine();
    let IngestResult::Created(ids) = e.ingest(make_obs("a")).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];

    let before = e.graph().get_node(id).unwrap().access_history.len();
    e.touch(id, Timestamp(2000)).unwrap();
    let after = e.graph().get_node(id).unwrap().access_history.len();

    assert!(after > before, "access_history should grow on touch");
}

#[test]
fn tick_decays_reservoir_and_reprojects_salience() {
    let mut e = engine();
    let IngestResult::Created(ids) = e.ingest(make_obs("b")).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];

    let a0 = e.graph().get_node(id).unwrap().retained_action;
    let s0 = e.graph().get_node(id).unwrap().salience;

    // 30 days elapsed: power-law dissipation lowers A_i and thus salience.
    e.tick(Timestamp(1000 + 30 * DAY_MS)).unwrap();
    let node = e.graph().get_node(id).unwrap();

    assert!(node.retained_action < a0, "A_i should decay: {}", node.retained_action);
    assert!(node.salience < s0, "salience projection should fall: {}", node.salience);
    // No [0,1] floor on the reservoir, but the projection is always in (0,1).
    assert!(node.salience > 0.0 && node.salience < 1.0);
}

#[test]
fn touch_decays_before_reinforcing() {
    let mut e = engine();
    let IngestResult::Created(ids) = e.ingest(make_obs("c")).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];

    // Touch far in the future: decay (30 days) is applied before access gain.
    e.touch(id, Timestamp(1000 + 30 * DAY_MS)).unwrap();
    let node = e.graph().get_node(id).unwrap();

    assert!(node.salience > 0.0 && node.salience < 1.0);
    assert_eq!(node.access_count, 1);
}
