use anamnesis::api::{Engine, EngineConfig, IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, MemoryTier, Timestamp};
use anamnesis::storage::StorageAdapter;

fn make_obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: name.to_string(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
    }
}

#[test]
fn core_tier_protects_from_decay() {
    let config = EngineConfig::default();
    let mut e = Engine::with_config(config);
    let id = match e.ingest(make_obs("rule")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    e.set_tier(id, MemoryTier::Core).unwrap();
    assert_eq!(e.get_tier(id).unwrap(), MemoryTier::Core);
    let before = e.graph().storage().get_salience(id).unwrap();

    e.tick(Timestamp(365 * 86_400_000)).unwrap();

    let after = e.graph().storage().get_salience(id).unwrap();
    assert!(
        (before - after).abs() < 0.01,
        "Core tier should protect from decay: before={before}, after={after}"
    );
}

#[test]
fn auto_tier_allows_decay() {
    let config = EngineConfig::default();
    let mut e = Engine::with_config(config);
    let id = match e.ingest(make_obs("ephemeral")).unwrap() {
        IngestResult::Created(ids) => ids[0],
        _ => panic!("expected Created"),
    };
    let before = e.graph().storage().get_salience(id).unwrap();

    e.tick(Timestamp(365 * 86_400_000)).unwrap();

    let after = e.graph().storage().get_salience(id).unwrap();
    assert!(
        after < before,
        "Auto tier should allow decay: before={before}, after={after}"
    );
}
