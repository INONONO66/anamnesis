//! Tests for learn(), remember_peer(), log_activity() convenience methods (T15).

use anamnesis::api::{ActivityInput, LearnInput, PeerProfileInput};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::peer::{SourceKind, TrustLevel};
use anamnesis::{Engine, EngineConfig};

fn default_origin() -> Origin {
    Origin {
        peer_id: PeerId(0),
        source_kind: SourceKind::AgentObservation,
        session_id: "s1".to_string(),
        scope: ScopePath::universal(),
        confidence: 0.9,
    }
}

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

// ── learn() ───────────────────────────────────────────────────────────────────

#[test]
fn learn_ingests_semantic_node() {
    let mut e = engine();
    let result = e
        .learn(LearnInput {
            name: "auth uses factory pattern".to_string(),
            summary: None,
            content: "The auth module uses factory pattern".to_string(),
            embedding: None,
            confidence: Some(0.9),
            node_type: Some(KnowledgeType::Convention),
            entity_tags: vec!["auth".to_string()],
            origin: default_origin(),
            timestamp: Some(Timestamp::now()),
        })
        .unwrap();
    assert!(matches!(result, anamnesis::IngestResult::Created(_)));
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
        anamnesis::IngestResult::Created(ids) => ids[0],
        anamnesis::IngestResult::Reinforced { existing_id, .. } => existing_id,
    };
    let node = e.graph().get_node(node_id).unwrap();
    let expected_scope = format!("peer/{}/profile", peer_id.0);
    assert_eq!(node.origin.scope.as_str(), expected_scope);
}

// ── log_activity() ────────────────────────────────────────────────────────────

#[test]
fn log_activity_uses_activity_scope() {
    let mut e = engine();
    let peer_id = e.register_peer("alice", TrustLevel::Owner).unwrap();
    let (_pid, result) = e
        .log_activity(ActivityInput {
            peer_name: "alice".to_string(),
            name: "alice activity".to_string(),
            summary: None,
            content: "Worked on auth module".to_string(),
            embedding: None,
            confidence: Some(0.8),
            node_type: None,
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: Some(Timestamp::now()),
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
    let node_id = match result {
        anamnesis::IngestResult::Created(ids) => ids[0],
        anamnesis::IngestResult::Reinforced { existing_id, .. } => existing_id,
    };
    let node = e.graph().get_node(node_id).unwrap();
    let expected_scope = format!("peer/{}/activity", peer_id.0);
    assert_eq!(node.origin.scope.as_str(), expected_scope);
}

#[test]
fn log_activity_auto_registers_peer() {
    let mut e = engine();
    let (_peer_id, _result) = e
        .log_activity(ActivityInput {
            peer_name: "bob".to_string(),
            name: "bob activity".to_string(),
            summary: None,
            content: "Did something".to_string(),
            embedding: None,
            confidence: None,
            node_type: None,
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
    assert!(e.resolve_peer("bob").is_some());
}
