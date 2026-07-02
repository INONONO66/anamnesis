//! Power-law base-level forgetting (ADR-0008).
//!
//! Persistent node strength is `A_i = B_i + P_i`. The base level
//! `B_i = ln(Σ_j (now − at_j)^(−d_j))` (each trace's own activation-dependent decay
//! `d_j`, Pavlik & Anderson 2005) is recomputed on demand from the access-trace
//! history (never stored); `P_i` is a decay-exempt evidence prior.
//! `salience = logistic(B_i + P_i)`. A committed access (`touch`) appends a trace,
//! raising `B_i`; `tick` recomputes salience as `B_i(now)` falls with elapsed time —
//! it does not shift a stored reservoir. `retained_action` is the cached composite.

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
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
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
fn tick_recomputes_lower_salience_as_base_level_ages() {
    let mut e = engine();
    let IngestResult::Created(ids) = e.ingest(make_obs("b")).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];

    let a0 = e.graph().get_node(id).unwrap().retained_action;
    let s0 = e.graph().get_node(id).unwrap().salience;

    // 30 days elapsed: no stored reservoir is shifted — B_i(now) falls because the
    // fixed creation trace is aged to a later `now`, so the recomputed composite
    // A_i = B_i + P_i (and its salience projection) drops.
    e.tick(Timestamp(1000 + 30 * DAY_MS)).unwrap();
    let node = e.graph().get_node(id).unwrap();

    assert!(
        node.retained_action < a0,
        "recomputed A_i fell because B_i(now) decreased: {}",
        node.retained_action
    );
    assert!(
        node.salience < s0,
        "salience projection should fall: {}",
        node.salience
    );
    // The projection is always strictly in (0, 1).
    assert!(node.salience > 0.0 && node.salience < 1.0);
}

#[test]
fn touch_appends_trace_recompute_is_decay_first() {
    let mut e = engine();
    let IngestResult::Created(ids) = e.ingest(make_obs("c")).unwrap() else {
        panic!("expected Created");
    };
    let id = ids[0];

    // Touch far in the future: the access appends a trace at `now` and salience is
    // logistic(B_i(now) + P_i). Decay-first is intrinsic — B_i ages the creation
    // trace to `now` inside the same sum that adds the fresh trace.
    e.touch(id, Timestamp(1000 + 30 * DAY_MS)).unwrap();
    let node = e.graph().get_node(id).unwrap();

    assert!(node.salience > 0.0 && node.salience < 1.0);
    assert_eq!(node.access_count, 1);
    assert_eq!(node.access_history.len(), 2, "creation + touch traces");
}
