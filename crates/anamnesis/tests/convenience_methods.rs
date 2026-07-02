//! Tests for the remember_peer() convenience method (T15).

use anamnesis::Engine;
use anamnesis::api::PeerProfileInput;
use anamnesis::engine::EngineConfig;
use anamnesis::graph::Timestamp;
use anamnesis::peer::TrustLevel;

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

// ── remember_peer() ───────────────────────────────────────────────────────────

#[test]
fn remember_peer_auto_registers_unknown_peer() {
    let mut e = engine();
    let (peer_id, _result) = e
        .remember_peer(PeerProfileInput {
            peer_name: "신규인물".to_string(),
            name: "신규인물 profile".to_string(),
            summary: None,
            content: "개발자".to_string(),
            embedding: None,
            confidence: Some(0.9),
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: Some(Timestamp::now()),
        })
        .unwrap();
    // Peer should now be registered
    assert!(e.resolve_peer("신규인물").is_some());
    assert_eq!(e.resolve_peer("신규인물"), Some(peer_id));
}

#[test]
fn remember_peer_uses_profile_scope() {
    let mut e = engine();
    let peer_id = e.register_peer("alice", TrustLevel::Owner).unwrap();
    let (_pid, result) = e
        .remember_peer(PeerProfileInput {
            peer_name: "alice".to_string(),
            name: "alice profile".to_string(),
            summary: None,
            content: "Software engineer".to_string(),
            embedding: None,
            confidence: Some(0.9),
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: Some(Timestamp::now()),
        })
        .unwrap();
    let node_id = match result {
        anamnesis::engine::IngestResult::Created(ids) => ids[0],
        anamnesis::engine::IngestResult::Reinforced { existing_id, .. } => existing_id,
    };
    let node = e.graph().get_node(node_id).unwrap();
    let expected_scope = format!("peer/{}/profile", peer_id.0);
    assert_eq!(node.origin.scope.as_str(), expected_scope);
}
