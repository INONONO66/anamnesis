//! Tests for PeerId, TrustLevel, SourceKind, EdgeSource type primitives.

use anamnesis::graph::edge::EdgeSource;
use anamnesis::graph::types::PeerId;
use anamnesis::peer::{SourceKind, TrustLevel};

// ── PeerId ────────────────────────────────────────────────────────────────────

#[test]
fn peer_id_equality() {
    assert_eq!(PeerId(1), PeerId(1));
    assert_ne!(PeerId(1), PeerId(2));
}

#[test]
fn peer_id_is_copy() {
    let id = PeerId(42);
    let id2 = id; // Copy
    assert_eq!(id, id2);
}

#[test]
fn peer_id_ordering() {
    assert!(PeerId(1) < PeerId(2));
    assert!(PeerId(100) > PeerId(50));
}

#[test]
fn peer_id_hash() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(PeerId(1));
    set.insert(PeerId(2));
    set.insert(PeerId(1)); // duplicate
    assert_eq!(set.len(), 2);
}

// ── TrustLevel ────────────────────────────────────────────────────────────────

#[test]
fn trust_level_ordering() {
    // Owner > Admin > Member > Agent > Observer > Untrusted
    assert!(TrustLevel::Owner > TrustLevel::Admin);
    assert!(TrustLevel::Admin > TrustLevel::Member);
    assert!(TrustLevel::Member > TrustLevel::Agent);
    assert!(TrustLevel::Agent > TrustLevel::Observer);
    assert!(TrustLevel::Observer > TrustLevel::Untrusted);
    // Transitive
    assert!(TrustLevel::Owner > TrustLevel::Untrusted);
}

#[test]
fn trust_level_equality() {
    assert_eq!(TrustLevel::Owner, TrustLevel::Owner);
    assert_ne!(TrustLevel::Owner, TrustLevel::Untrusted);
}

#[test]
fn trust_level_is_copy() {
    let t = TrustLevel::Member;
    let t2 = t; // Copy
    assert_eq!(t, t2);
}

#[test]
fn trust_level_scope_weight_bonus() {
    assert_eq!(TrustLevel::Owner.scope_weight_bonus(), 0.10);
    assert_eq!(TrustLevel::Admin.scope_weight_bonus(), 0.07);
    assert_eq!(TrustLevel::Member.scope_weight_bonus(), 0.03);
    assert_eq!(TrustLevel::Agent.scope_weight_bonus(), 0.00);
    assert_eq!(TrustLevel::Observer.scope_weight_bonus(), 0.00);
    assert_eq!(TrustLevel::Untrusted.scope_weight_bonus(), -0.05);
}

#[test]
fn all_trust_levels_constructable() {
    let levels = [
        TrustLevel::Owner,
        TrustLevel::Admin,
        TrustLevel::Member,
        TrustLevel::Agent,
        TrustLevel::Observer,
        TrustLevel::Untrusted,
    ];
    assert_eq!(levels.len(), 6);
}

// ── SourceKind ────────────────────────────────────────────────────────────────

#[test]
fn all_source_kinds_constructable() {
    let kinds = [
        SourceKind::AgentObservation,
        SourceKind::HumanInput,
        SourceKind::DocumentExtract,
        SourceKind::SystemEvent,
        SourceKind::Inferred,
        SourceKind::External,
    ];
    assert_eq!(kinds.len(), 6);
}

#[test]
fn source_kind_equality() {
    assert_eq!(SourceKind::HumanInput, SourceKind::HumanInput);
    assert_ne!(SourceKind::HumanInput, SourceKind::AgentObservation);
}

// ── EdgeSource ────────────────────────────────────────────────────────────────

#[test]
fn all_edge_sources_constructable() {
    let sources = [EdgeSource::Auto, EdgeSource::Manual, EdgeSource::Inferred];
    assert_eq!(sources.len(), 3);
}

#[test]
fn edge_source_equality() {
    assert_eq!(EdgeSource::Auto, EdgeSource::Auto);
    assert_ne!(EdgeSource::Auto, EdgeSource::Manual);
}

#[test]
fn edge_source_is_copy() {
    let s = EdgeSource::Manual;
    let s2 = s; // Copy
    assert_eq!(s, s2);
}

// ── Type safety (compile-time) ────────────────────────────────────────────────
// PeerId cannot be used where NodeId is expected — enforced by the type system.
// The following would NOT compile:
//   let node_id: anamnesis::NodeId = PeerId(1); // type mismatch
// This is verified by the absence of such code compiling successfully.
