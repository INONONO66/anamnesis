//! Tests for Engine peer registry API (T8).

use anamnesis::graph::types::PeerId;
use anamnesis::peer::TrustLevel;
use anamnesis::{Engine, Error};

#[test]
fn register_peer_returns_peer_id() {
    let mut engine = Engine::new();
    let id = engine.register_peer("alice", TrustLevel::Owner).unwrap();
    assert_eq!(id, PeerId(0));
}

#[test]
fn resolve_peer_by_name() {
    let mut engine = Engine::new();
    let id = engine.register_peer("alice", TrustLevel::Owner).unwrap();
    assert_eq!(engine.resolve_peer("alice"), Some(id));
}

#[test]
fn resolve_unknown_returns_none() {
    let engine = Engine::new();
    assert_eq!(engine.resolve_peer("nobody"), None);
}

#[test]
fn add_alias_and_resolve() {
    let mut engine = Engine::new();
    let id = engine.register_peer("alice", TrustLevel::Owner).unwrap();
    engine.add_peer_alias(id, "앨리스").unwrap();
    assert_eq!(engine.resolve_peer("앨리스"), Some(id));
}

#[test]
fn add_platform_and_resolve() {
    let mut engine = Engine::new();
    let id = engine.register_peer("alice", TrustLevel::Owner).unwrap();
    engine.add_peer_platform(id, "discord", "ino").unwrap();
    assert_eq!(engine.resolve_peer("ino"), Some(id));
}

#[test]
fn update_peer_trust() {
    let mut engine = Engine::new();
    let id = engine.register_peer("alice", TrustLevel::Member).unwrap();
    engine.update_peer_trust(id, TrustLevel::Admin).unwrap();
    let profile = engine.get_peer(id).unwrap();
    assert_eq!(profile.trust_level, TrustLevel::Admin);
}

#[test]
fn list_peers_returns_all() {
    let mut engine = Engine::new();
    engine.register_peer("alice", TrustLevel::Owner).unwrap();
    engine.register_peer("bob", TrustLevel::Member).unwrap();
    assert_eq!(engine.list_peers().len(), 2);
    assert_eq!(engine.peer_count(), 2);
}

#[test]
fn duplicate_name_returns_error() {
    let mut engine = Engine::new();
    engine.register_peer("alice", TrustLevel::Owner).unwrap();
    let result = engine.register_peer("alice", TrustLevel::Member);
    assert!(matches!(result, Err(Error::DuplicateAlias(_))));
}

#[test]
fn get_peer_returns_profile() {
    let mut engine = Engine::new();
    let id = engine.register_peer("alice", TrustLevel::Owner).unwrap();
    let profile = engine.get_peer(id).unwrap();
    assert_eq!(profile.name, "alice");
    assert_eq!(profile.trust_level, TrustLevel::Owner);
}
