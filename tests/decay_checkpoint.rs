//! Decay checkpoint invariant tests.
//!
//! `decay_checkpoint` is an internal SoA hot field separate from `accessed_at`.
//! These tests pin the invariant that `Engine::touch()` updates BOTH fields,
//! while `Engine::tick()` updates ONLY `decay_checkpoint`, leaving `accessed_at`
//! as a stable "last user access" signal.

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, NodeId, Timestamp};
use anamnesis::{Engine, EngineConfig, IngestResult, StorageAdapter};

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
            agent_id: "agent-A".to_string(),
            session_id: "sess-1".to_string(),
            scope: anamnesis::graph::ScopePath::new("proj-X").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: Timestamp(ts),
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
fn decay_checkpoint_invariant() {
    let mut engine = test_engine();

    let t0 = Timestamp(0);
    let id = ingest_first(&mut engine, observation_at("alpha", t0.0));

    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        t0,
        "accessed_at should equal creation timestamp"
    );
    assert_eq!(
        storage.get_decay_checkpoint(id).unwrap(),
        t0,
        "decay_checkpoint should be initialized from accessed_at on set_node"
    );

    let t_tick = Timestamp(2 * DAY_MS);
    engine.tick(t_tick).unwrap();

    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        t0,
        "tick must NOT pollute accessed_at; last-access semantics preserved"
    );
    assert_eq!(
        storage.get_decay_checkpoint(id).unwrap(),
        t_tick,
        "tick must advance decay_checkpoint to now"
    );

    let t_touch = Timestamp(5 * DAY_MS);
    engine.touch(id, t_touch).unwrap();

    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        t_touch,
        "touch must update accessed_at"
    );
    assert_eq!(
        storage.get_decay_checkpoint(id).unwrap(),
        t_touch,
        "touch must update decay_checkpoint to match accessed_at"
    );
}

#[test]
fn set_node_initializes_checkpoint_from_accessed_at() {
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("beta", 1000));

    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        storage.get_decay_checkpoint(id).unwrap(),
        "after set_node, checkpoint == accessed_at"
    );
}

#[test]
fn tick_without_salience_change_keeps_checkpoint_stable() {
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("gamma", 0));

    let initial_checkpoint = engine.graph().storage().get_decay_checkpoint(id).unwrap();

    engine.tick(Timestamp(0)).unwrap();

    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_decay_checkpoint(id).unwrap(),
        initial_checkpoint,
        "tick at t=0 (zero elapsed) should not advance checkpoint"
    );
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        Timestamp(0),
        "accessed_at remains untouched by tick"
    );
}

#[test]
fn touch_uses_checkpoint_not_accessed_at_for_decay_baseline() {
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("delta", 0));

    let s_initial = engine.graph().storage().get_salience(id).unwrap();

    engine.tick(Timestamp(10 * DAY_MS)).unwrap();
    let s_after_tick = engine.graph().storage().get_salience(id).unwrap();
    assert!(
        s_after_tick < s_initial,
        "tick should reduce salience over 10 days"
    );

    let pre_touch_checkpoint = engine.graph().storage().get_decay_checkpoint(id).unwrap();
    assert_eq!(
        pre_touch_checkpoint,
        Timestamp(10 * DAY_MS),
        "tick advanced checkpoint"
    );
    assert_eq!(
        engine.graph().storage().get_accessed_at(id).unwrap(),
        Timestamp(0),
        "tick did not touch accessed_at"
    );

    engine.touch(id, Timestamp(11 * DAY_MS)).unwrap();
    let s_after_touch = engine.graph().storage().get_salience(id).unwrap();

    assert!(
        s_after_touch > s_after_tick,
        "touch with checkpoint baseline (1 day elapsed) should reinforce more than minimal decay; \
         if touch had used accessed_at (11 days elapsed) the result would dip lower"
    );

    let storage = engine.graph().storage();
    assert_eq!(storage.get_accessed_at(id).unwrap(), Timestamp(11 * DAY_MS));
    assert_eq!(
        storage.get_decay_checkpoint(id).unwrap(),
        Timestamp(11 * DAY_MS)
    );
}

#[test]
fn delete_node_clears_checkpoint_slot() {
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("epsilon", 1234));

    assert_eq!(
        engine.graph().storage().get_decay_checkpoint(id).unwrap(),
        Timestamp(1234)
    );

    engine.graph_mut().storage_mut().delete_node(id).unwrap();

    let err = engine
        .graph()
        .storage()
        .get_decay_checkpoint(id)
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("NodeNotFound"),
        "deleted node lookup should fail; got {msg}"
    );
}

#[test]
fn snapshot_round_trip_preserves_checkpoint() {
    let mut engine = test_engine();
    let id = ingest_first(&mut engine, observation_at("zeta", 0));

    engine.tick(Timestamp(3 * DAY_MS)).unwrap();
    let snap = engine.snapshot("after-tick");

    engine.touch(id, Timestamp(4 * DAY_MS)).unwrap();
    assert_eq!(
        engine.graph().storage().get_decay_checkpoint(id).unwrap(),
        Timestamp(4 * DAY_MS)
    );

    engine.restore(&snap).unwrap();

    let storage = engine.graph().storage();
    assert_eq!(
        storage.get_decay_checkpoint(id).unwrap(),
        Timestamp(3 * DAY_MS),
        "snapshot must preserve decay_checkpoint via Storage: Clone"
    );
    assert_eq!(
        storage.get_accessed_at(id).unwrap(),
        Timestamp(0),
        "snapshot also preserves accessed_at"
    );
}
