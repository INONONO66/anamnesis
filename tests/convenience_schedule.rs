//! Tests for schedule() convenience method (T16).

use anamnesis::api::ScheduleInput;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::peer::TrustLevel;
use anamnesis::{Engine, EngineConfig};

fn engine() -> Engine {
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

#[test]
fn schedule_sets_valid_from() {
    let mut e = engine();
    let peer_id = e.register_peer("alice", TrustLevel::Owner).unwrap();
    let valid_from = Timestamp(1_000_000);
    let (_pid, result) = e
        .schedule(ScheduleInput {
            peer_name: "alice".to_string(),
            name: "team meeting".to_string(),
            summary: None,
            content: "Weekly sync".to_string(),
            embedding: None,
            confidence: None,
            participants: vec!["bob".to_string(), "charlie".to_string()],
            entity_tags: vec![],
            session_id: None,
            timestamp: None,
            valid_from,
            valid_until: None,
        })
        .unwrap();
    let node_id = match result {
        anamnesis::IngestResult::Created(ids) => ids[0],
        anamnesis::IngestResult::Reinforced { existing_id, .. } => existing_id,
    };
    let node = e.graph().get_node(node_id).unwrap();
    assert_eq!(node.valid_from, Some(valid_from));
    assert_eq!(node.node_type, KnowledgeType::Event);
    // Participants should be in entity_tags
    assert!(node.entity_tags.contains(&"bob".to_string()));
    assert!(node.entity_tags.contains(&"charlie".to_string()));
    // Scope should be peer/{id}/activity
    let expected_scope = format!("peer/{}/activity", peer_id.0);
    assert_eq!(node.origin.scope.as_str(), expected_scope);
}

#[test]
fn schedule_auto_registers_peer() {
    let mut e = engine();
    let (_peer_id, _result) = e
        .schedule(ScheduleInput {
            peer_name: "new-person".to_string(),
            name: "event".to_string(),
            summary: None,
            content: "Some event".to_string(),
            embedding: None,
            confidence: None,
            participants: vec![],
            entity_tags: vec![],
            session_id: None,
            timestamp: None,
            valid_from: Timestamp::now(),
            valid_until: None,
        })
        .unwrap();
    assert!(e.resolve_peer("new-person").is_some());
}
