//! Tests for PeerRegistry — register, resolve, alias, platform, update.

use anamnesis::error::Error;
use anamnesis::graph::types::PeerId;
use anamnesis::peer::{PeerRegistry, TrustLevel};

// ── register_peer ─────────────────────────────────────────────────────────────

#[test]
fn register_peer_returns_peer_id() {
    let mut reg = PeerRegistry::new();
    let id = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    assert_eq!(id, PeerId(0));
}

#[test]
fn register_multiple_peers_increments_id() {
    let mut reg = PeerRegistry::new();
    let id1 = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    let id2 = reg.register_peer("bob", TrustLevel::Member).unwrap();
    assert_ne!(id1, id2);
}

#[test]
fn register_duplicate_name_returns_error() {
    let mut reg = PeerRegistry::new();
    reg.register_peer("alice", TrustLevel::Owner).unwrap();
    let result = reg.register_peer("alice", TrustLevel::Member);
    assert!(matches!(result, Err(Error::DuplicateAlias(_))));
}

// ── resolve_peer ──────────────────────────────────────────────────────────────

#[test]
fn resolve_peer_by_name() {
    let mut reg = PeerRegistry::new();
    let id = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    assert_eq!(reg.resolve_peer("alice"), Some(id));
}

#[test]
fn resolve_unknown_identifier_returns_none() {
    let reg = PeerRegistry::new();
    assert_eq!(reg.resolve_peer("nobody"), None);
}

// ── add_alias ─────────────────────────────────────────────────────────────────

#[test]
fn add_alias_and_resolve() {
    let mut reg = PeerRegistry::new();
    let id = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    reg.add_alias(id, "앨리스").unwrap();
    assert_eq!(reg.resolve_peer("앨리스"), Some(id));
}

#[test]
fn add_alias_duplicate_returns_error() {
    let mut reg = PeerRegistry::new();
    let id1 = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    let id2 = reg.register_peer("charlie", TrustLevel::Member).unwrap();
    reg.add_alias(id1, "bob").unwrap();
    // Try to add same alias to another peer
    let result = reg.add_alias(id2, "bob");
    assert!(matches!(result, Err(Error::DuplicateAlias(_))));
}

#[test]
fn add_alias_to_unknown_peer_returns_error() {
    let mut reg = PeerRegistry::new();
    let result = reg.add_alias(PeerId(999), "ghost");
    assert!(matches!(result, Err(Error::PeerNotFound(_))));
}

// ── add_platform ──────────────────────────────────────────────────────────────

#[test]
fn add_platform_and_resolve() {
    let mut reg = PeerRegistry::new();
    let id = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    reg.add_platform(id, "discord", "ino").unwrap();
    assert_eq!(reg.resolve_peer("ino"), Some(id));
}

#[test]
fn add_platform_duplicate_username_returns_error() {
    let mut reg = PeerRegistry::new();
    let id1 = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    let id2 = reg.register_peer("bob", TrustLevel::Member).unwrap();
    reg.add_platform(id1, "discord", "shared_handle").unwrap();
    let result = reg.add_platform(id2, "discord", "shared_handle");
    assert!(matches!(result, Err(Error::DuplicateAlias(_))));
}

// ── update_trust ──────────────────────────────────────────────────────────────

#[test]
fn update_trust_level() {
    let mut reg = PeerRegistry::new();
    let id = reg.register_peer("alice", TrustLevel::Member).unwrap();
    reg.update_trust(id, TrustLevel::Admin).unwrap();
    let profile = reg.get_peer(id).unwrap();
    assert_eq!(profile.trust_level, TrustLevel::Admin);
}

// ── get_peer ──────────────────────────────────────────────────────────────────

#[test]
fn get_peer_returns_profile() {
    let mut reg = PeerRegistry::new();
    let id = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    let profile = reg.get_peer(id).unwrap();
    assert_eq!(profile.name, "alice");
    assert_eq!(profile.trust_level, TrustLevel::Owner);
}

#[test]
fn get_peer_unknown_returns_none() {
    let reg = PeerRegistry::new();
    assert!(reg.get_peer(PeerId(999)).is_none());
}

// ── list_peers ────────────────────────────────────────────────────────────────

#[test]
fn list_peers_returns_all() {
    let mut reg = PeerRegistry::new();
    reg.register_peer("alice", TrustLevel::Owner).unwrap();
    reg.register_peer("bob", TrustLevel::Member).unwrap();
    assert_eq!(reg.list_peers().len(), 2);
}

// ── all_identifiers ───────────────────────────────────────────────────────────

#[test]
fn all_identifiers_includes_name_alias_platform() {
    let mut reg = PeerRegistry::new();
    let id = reg.register_peer("alice", TrustLevel::Owner).unwrap();
    reg.add_alias(id, "앨리스").unwrap();
    reg.add_platform(id, "discord", "alice#1234").unwrap();
    let ids = reg.all_identifiers(id);
    assert!(ids.contains(&"alice".to_string()));
    assert!(ids.contains(&"앨리스".to_string()));
    assert!(ids.contains(&"alice#1234".to_string()));
}

// ── Full lifecycle ────────────────────────────────────────────────────────────

#[test]
fn full_peer_lifecycle() {
    let mut reg = PeerRegistry::new();

    // Register
    let id = reg.register_peer("alice", TrustLevel::Member).unwrap();

    // Resolve by name
    assert_eq!(reg.resolve_peer("alice"), Some(id));

    // Add alias
    reg.add_alias(id, "앨리스").unwrap();
    assert_eq!(reg.resolve_peer("앨리스"), Some(id));

    // Add platform
    reg.add_platform(id, "discord", "ino").unwrap();
    assert_eq!(reg.resolve_peer("ino"), Some(id));

    // Update trust
    reg.update_trust(id, TrustLevel::Admin).unwrap();
    assert_eq!(reg.get_peer(id).unwrap().trust_level, TrustLevel::Admin);

    // Peer count
    assert_eq!(reg.peer_count(), 1);
}
