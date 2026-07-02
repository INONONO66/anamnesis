//! Access-trace and tick behavior under the `A_i = B_i + P_i` model (ADR-0008).
//!
//! `decay_checkpoint` is OBSOLETE: there is no scalar-decay "as-of" baseline. The
//! base level `B_i` is recomputed from the access-trace history aged to `now`, so a
//! committed access (touch) appends a now-stamped trace and updates `accessed_at`,
//! while `tick` only recomputes salience (it appends no trace and does not touch
//! `accessed_at`). These tests pin that behavior; the obsolete checkpoint column is
//! retained only for snapshot back-compat (see `snapshot_round_trip.rs`).

use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{EngineConfig, IngestResult, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, NodeId, Timestamp};

const DAY_MS: u64 = 86_400_000;

fn observation_at(name: &str, ts: u64) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: Some(vec![0.5, 0.4, 0.1]),
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["topic".to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: "sess-1".to_string(),
            scope: anamnesis::graph::ScopePath::new("proj-X").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: Timestamp(ts),
        valid_from: None,
        valid_until: None,
    }
}

fn test_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn ingest_first(engine: &mut Engine, obs: Observation) -> NodeId {
    match engine.ingest(obs).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("expected fresh node"),
    }
}

#[test]
fn ingest_seeds_creation_trace() {
    // A freshly ingested node carries its creation event as a trace, so B_i is
    // finite at birth (compute_base_level returns NEG_INFINITY on empty history).
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("alpha", 0));
    let node = engine.graph().get_node(id).unwrap();
    assert_eq!(
        node.access_history.len(),
        1,
        "ingest must seed exactly the creation trace"
    );
    assert_eq!(
        node.access_history.front().unwrap().at,
        Timestamp(0),
        "creation trace must be stamped at created_at"
    );
    assert!(
        node.retained_action.is_finite(),
        "fresh node A_i must be finite (creation trace seeds B_i)"
    );
}

#[test]
fn touch_appends_trace_and_updates_accessed_at() {
    let mut engine = test_engine();
    let t0 = Timestamp(0);
    let id = ingest_first(&mut engine, observation_at("beta", t0.0));

    let before = engine.graph().get_node(id).unwrap().access_history.len();
    assert_eq!(
        engine.graph().storage().get_accessed_at(id).unwrap(),
        t0,
        "accessed_at starts at creation timestamp"
    );

    let t_touch = Timestamp(5 * DAY_MS);
    engine.touch(id, t_touch).unwrap();

    let node = engine.graph().get_node(id).unwrap();
    assert_eq!(
        node.access_history.len(),
        before + 1,
        "touch must append an access trace (raising B_i)"
    );
    assert_eq!(
        node.access_history.back().unwrap().at,
        t_touch,
        "appended trace must be stamped at now"
    );
    assert_eq!(
        engine.graph().storage().get_accessed_at(id).unwrap(),
        t_touch,
        "touch must update accessed_at to now"
    );
}

#[test]
fn tick_recomputes_salience_without_appending_a_trace() {
    // tick is a TimeElapsed interaction: it recomputes B_i(now) but appends no
    // trace and never touches accessed_at (no scalar decay baseline exists).
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("gamma", 0));

    let traces_before = engine.graph().get_node(id).unwrap().access_history.len();
    let s_before = engine.graph().storage().get_salience(id).unwrap();

    engine.tick(Timestamp(30 * DAY_MS)).unwrap();

    let node = engine.graph().get_node(id).unwrap();
    assert_eq!(
        node.access_history.len(),
        traces_before,
        "tick must NOT append a trace"
    );
    assert_eq!(
        engine.graph().storage().get_accessed_at(id).unwrap(),
        Timestamp(0),
        "tick must NOT pollute accessed_at; last-access semantics preserved"
    );
    let s_after = engine.graph().storage().get_salience(id).unwrap();
    assert!(
        s_after < s_before,
        "B_i(now) fell with elapsed time, so salience drops: {s_after} !< {s_before}"
    );
}

#[test]
fn touch_after_tick_recovers_salience_via_fresh_trace() {
    // Under the base-level model an old site cannot gain fresh strength without
    // first paying accumulated leakage (decay-first is intrinsic), but a committed
    // access appends a now-stamped trace that raises B_i back up.
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("delta", 0));

    let s_initial = engine.graph().storage().get_salience(id).unwrap();
    engine.tick(Timestamp(20 * DAY_MS)).unwrap();
    let s_decayed = engine.graph().storage().get_salience(id).unwrap();
    assert!(s_decayed < s_initial, "tick lowered salience");

    engine.touch(id, Timestamp(20 * DAY_MS)).unwrap();
    let s_touched = engine.graph().storage().get_salience(id).unwrap();
    assert!(
        s_touched > s_decayed,
        "a fresh trace raises B_i (hence salience): {s_touched} !> {s_decayed}"
    );
}

#[test]
fn evidence_prior_is_decay_exempt_under_tick() {
    // P_i is a decay-exempt offset: tick recomputes B_i(now) but never touches P_i.
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("epsilon", 0));

    let p_before = engine.graph().storage().get_evidence_prior(id).unwrap();
    engine.tick(Timestamp(365 * DAY_MS)).unwrap();
    let p_after = engine.graph().storage().get_evidence_prior(id).unwrap();
    assert_eq!(
        p_before, p_after,
        "evidence prior P_i must be unchanged by elapsed time"
    );
}
