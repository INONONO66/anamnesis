//! Tests for PeerRegistry SQLite persistence — peers survive Engine restart.

use anamnesis::peer::TrustLevel;
use anamnesis::storage::SqliteStorage;
use anamnesis::{Engine, EngineConfig};

#[test]
fn peers_persist_across_engine_restart_file_backed() {
    let tmp =
        std::env::temp_dir().join(format!("anamnesis_peer_persist_{}.db", std::process::id()));

    let peer_id;
    {
        let storage = SqliteStorage::open(&tmp).expect("open");
        let mut engine = Engine::with_storage(EngineConfig::default(), storage);
        peer_id = engine.register_peer("alice", TrustLevel::Owner).unwrap();
        engine.add_peer_alias(peer_id, "앨리스").unwrap();
        engine
            .add_peer_platform(peer_id, "discord", "alice#1234")
            .unwrap();
        assert_eq!(engine.peer_count(), 1);
    }

    {
        let storage = SqliteStorage::open(&tmp).expect("reopen");
        let engine = Engine::with_storage(EngineConfig::default(), storage);
        assert_eq!(engine.peer_count(), 1, "peer should survive restart");
        assert_eq!(engine.resolve_peer("alice"), Some(peer_id));
        assert_eq!(engine.resolve_peer("앨리스"), Some(peer_id));
        assert_eq!(engine.resolve_peer("alice#1234"), Some(peer_id));
        let profile = engine.get_peer(peer_id).unwrap();
        assert_eq!(profile.trust_level, TrustLevel::Owner);
        assert!(profile.aliases.contains(&"앨리스".to_string()));
    }

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn multiple_peers_persist() {
    let tmp = std::env::temp_dir().join(format!("anamnesis_multi_peer_{}.db", std::process::id()));

    {
        let storage = SqliteStorage::open(&tmp).expect("open");
        let mut engine = Engine::with_storage(EngineConfig::default(), storage);
        engine.register_peer("alice", TrustLevel::Owner).unwrap();
        engine.register_peer("bob", TrustLevel::Member).unwrap();
        engine.register_peer("agent-1", TrustLevel::Agent).unwrap();
        assert_eq!(engine.peer_count(), 3);
    }

    {
        let storage = SqliteStorage::open(&tmp).expect("reopen");
        let engine = Engine::with_storage(EngineConfig::default(), storage);
        assert_eq!(engine.peer_count(), 3);
        assert!(engine.resolve_peer("alice").is_some());
        assert!(engine.resolve_peer("bob").is_some());
        assert!(engine.resolve_peer("agent-1").is_some());
    }

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn trust_level_update_persists() {
    let tmp =
        std::env::temp_dir().join(format!("anamnesis_trust_persist_{}.db", std::process::id()));

    let peer_id;
    {
        let storage = SqliteStorage::open(&tmp).expect("open");
        let mut engine = Engine::with_storage(EngineConfig::default(), storage);
        peer_id = engine.register_peer("alice", TrustLevel::Member).unwrap();
        engine
            .update_peer_trust(peer_id, TrustLevel::Admin)
            .unwrap();
    }

    {
        let storage = SqliteStorage::open(&tmp).expect("reopen");
        let engine = Engine::with_storage(EngineConfig::default(), storage);
        let profile = engine.get_peer(peer_id).unwrap();
        assert_eq!(
            profile.trust_level,
            TrustLevel::Admin,
            "trust level update should persist"
        );
    }

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn auto_registered_peers_persist() {
    use anamnesis::api::PeerProfileInput;
    use anamnesis::graph::Timestamp;

    let tmp = std::env::temp_dir().join(format!("anamnesis_auto_peer_{}.db", std::process::id()));

    {
        let storage = SqliteStorage::open(&tmp).expect("open");
        let mut engine =
            Engine::with_storage(EngineConfig::new().with_novelty_threshold(0.0), storage);
        engine
            .remember_peer(PeerProfileInput {
                peer_name: "신규인물".to_string(),
                name: "profile".to_string(),
                summary: None,
                content: "developer".to_string(),
                embedding: None,
                confidence: None,
                entity_tags: vec![],
                source_kind: None,
                session_id: None,
                timestamp: Some(Timestamp::now()),
            })
            .unwrap();
        assert!(engine.resolve_peer("신규인물").is_some());
    }

    {
        let storage = SqliteStorage::open(&tmp).expect("reopen");
        let engine = Engine::with_storage(EngineConfig::new().with_novelty_threshold(0.0), storage);
        assert!(
            engine.resolve_peer("신규인물").is_some(),
            "auto-registered peer should persist"
        );
    }

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn in_memory_engine_loads_empty_registry() {
    let engine = Engine::new();
    assert_eq!(engine.peer_count(), 0);
}
