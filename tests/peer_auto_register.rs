//! Tests for peer auto-register + alias↔entity_tags (T17).

use anamnesis::api::PeerProfileInput;
use anamnesis::graph::Timestamp;
use anamnesis::peer::TrustLevel;
use anamnesis::{Engine, EngineConfig};

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

#[test]
fn remember_peer_auto_registers_with_member_trust() {
    let mut e = engine();
    let (peer_id, _) = e
        .remember_peer(PeerProfileInput {
            peer_name: "new-person".to_string(),
            name: "new-person profile".to_string(),
            summary: None,
            content: "A new person".to_string(),
            embedding: None,
            confidence: None,
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: Some(Timestamp::now()),
        })
        .unwrap();
    let profile = e.get_peer(peer_id).unwrap();
    assert_eq!(profile.trust_level, TrustLevel::Member);
}

#[test]
fn remember_peer_existing_peer_not_re_registered() {
    let mut e = engine();
    let id1 = e.register_peer("alice", TrustLevel::Owner).unwrap();
    let (id2, _) = e
        .remember_peer(PeerProfileInput {
            peer_name: "alice".to_string(),
            name: "alice profile".to_string(),
            summary: None,
            content: "Alice's profile".to_string(),
            embedding: None,
            confidence: None,
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: None,
        })
        .unwrap();
    // Same peer ID — not re-registered
    assert_eq!(id1, id2);
    // Trust level should remain Owner (not downgraded to Member)
    let profile = e.get_peer(id1).unwrap();
    assert_eq!(profile.trust_level, TrustLevel::Owner);
}

#[test]
fn peer_aliases_included_in_entity_tags() {
    let mut e = engine();
    let peer_id = e.register_peer("chulsoo", TrustLevel::Member).unwrap();
    e.add_peer_alias(peer_id, "김철수").unwrap();

    let (_, result) = e
        .remember_peer(PeerProfileInput {
            peer_name: "chulsoo".to_string(),
            name: "chulsoo profile".to_string(),
            summary: None,
            content: "Developer".to_string(),
            embedding: None,
            confidence: None,
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: None,
        })
        .unwrap();
    let node_id = match result {
        anamnesis::IngestResult::Created(ids) => ids[0],
        anamnesis::IngestResult::Reinforced { existing_id, .. } => existing_id,
        anamnesis::IngestResult::CreatedWithConflict { node_ids, .. } => node_ids[0],
    };
    let node = e.graph().get_node(node_id).unwrap();
    // Both "chulsoo" and "김철수" should be in entity_tags
    assert!(
        node.entity_tags.contains(&"chulsoo".to_string()),
        "primary name should be in entity_tags"
    );
    assert!(
        node.entity_tags.contains(&"김철수".to_string()),
        "alias should be in entity_tags"
    );
}
