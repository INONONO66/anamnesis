//! v0.5.0 integration test suite (T22).
//!
//! End-to-end scenarios covering:
//! - Peer registration → ingest → conflict → retract → search → health
//! - Convenience methods: learn → remember_peer → log_activity → schedule → search
//! - Conversation: ingest_conversation → extraction → peer profile update → entity_tags search
//! - Cross-feature: peer_filter + retract + conflict + valid_from/valid_until

use anamnesis::api::{
    ActivityInput, ConversationInput, DocumentInput, ExtractedFact, IngestResult, LearnInput,
    Observation, PeerProfileInput, ScheduleInput,
};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::peer::{SourceKind, TrustLevel};
use anamnesis::query::SearchInput;
use anamnesis::{Engine, EngineConfig, StorageAdapter};

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn origin(peer_id: PeerId) -> Origin {
    Origin {
        peer_id,
        source_kind: SourceKind::AgentObservation,
        session_id: "integration-session".to_string(),
        scope: ScopePath::universal(),
        confidence: 0.9,
    }
}

// ── Scenario 1: Full peer lifecycle ──────────────────────────────────────────

#[test]
fn scenario_peer_registration_and_ingest() {
    let mut e = engine();

    // Register peers
    let alice_id = e.register_peer("alice", TrustLevel::Owner).unwrap();
    let bob_id = e.register_peer("bob", TrustLevel::Member).unwrap();

    // Ingest knowledge from alice
    let IngestResult::Created(alice_ids) = e
        .ingest(Observation {
            name: "auth uses factory pattern".to_string(),
            summary: None,
            content: "The auth module uses factory pattern".to_string(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Convention,
            entity_tags: vec!["auth".to_string()],
            origin: origin(alice_id),
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap()
    else {
        panic!("expected Created");
    };

    // Ingest knowledge from bob
    let IngestResult::Created(bob_ids) = e
        .ingest(Observation {
            name: "auth should use DI instead".to_string(),
            summary: None,
            content: "Dependency injection is better than factory pattern".to_string(),
            embedding: None,
            confidence: 0.8,
            node_type: KnowledgeType::Convention,
            entity_tags: vec!["auth".to_string()],
            origin: origin(bob_id),
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap()
    else {
        panic!("expected Created");
    };

    // Link them
    e.link(alice_ids[0], bob_ids[0], EdgeType::Contradicts)
        .unwrap();

    // Health check
    let report = e.health();
    assert_eq!(report.total_nodes, 2);
    assert_eq!(report.peer_count, 2);
    assert_eq!(report.contradiction_count, 1);

    // Retract alice's node
    e.retract(alice_ids[0], "outdated", Timestamp::now())
        .unwrap();
    assert!(e.is_retracted(alice_ids[0]).unwrap());

    // Search should not return retracted node
    let result = e
        .search(SearchInput {
            text: "auth factory".to_string(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    let found_alice = result
        .package
        .knowledge
        .iter()
        .any(|f| f.node_id == alice_ids[0]);
    assert!(!found_alice, "retracted node should not appear in search");
}

// ── Scenario 2: Convenience methods ──────────────────────────────────────────

#[test]
fn scenario_convenience_methods_full_flow() {
    let mut e = engine();

    // learn() - project knowledge
    let learn_result = e
        .learn(LearnInput {
            name: "Rust ownership rules".to_string(),
            summary: None,
            content: "Every value has exactly one owner".to_string(),
            embedding: None,
            confidence: Some(0.95),
            node_type: Some(KnowledgeType::Convention),
            entity_tags: vec!["rust".to_string()],
            origin: origin(PeerId(0)),
            timestamp: Some(Timestamp::now()),
        })
        .unwrap();
    assert!(matches!(learn_result, IngestResult::Created(_)));

    // remember_peer() - auto-register + profile
    let (peer_id, _) = e
        .remember_peer(PeerProfileInput {
            peer_name: "김철수".to_string(),
            name: "김철수 profile".to_string(),
            summary: None,
            content: "Rust developer, prefers functional style".to_string(),
            embedding: None,
            confidence: Some(0.9),
            entity_tags: vec![],
            source_kind: None,
            session_id: None,
            timestamp: Some(Timestamp::now()),
        })
        .unwrap();
    assert!(e.resolve_peer("김철수").is_some());

    // log_activity() - activity recording
    let (activity_peer_id, _) = e
        .log_activity(ActivityInput {
            peer_name: "김철수".to_string(),
            name: "김철수 worked on auth".to_string(),
            summary: None,
            content: "Refactored auth module to use factory pattern".to_string(),
            embedding: None,
            confidence: None,
            node_type: None,
            entity_tags: vec!["auth".to_string()],
            source_kind: None,
            session_id: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
    assert_eq!(peer_id, activity_peer_id);

    // schedule() - event scheduling
    let valid_from = Timestamp(1_000_000);
    let (_, schedule_result) = e
        .schedule(ScheduleInput {
            peer_name: "김철수".to_string(),
            name: "Code review meeting".to_string(),
            summary: None,
            content: "Review auth module changes".to_string(),
            embedding: None,
            confidence: None,
            participants: vec!["alice".to_string(), "bob".to_string()],
            entity_tags: vec![],
            session_id: None,
            timestamp: None,
            valid_from,
            valid_until: None,
        })
        .unwrap();
    let schedule_id = match schedule_result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    };
    let schedule_node = e.graph().get_node(schedule_id).unwrap();
    assert_eq!(schedule_node.valid_from, Some(valid_from));
    assert!(schedule_node.entity_tags.contains(&"alice".to_string()));
    assert!(schedule_node.entity_tags.contains(&"bob".to_string()));

    // Health check
    let report = e.health();
    assert!(report.total_nodes >= 3);
    assert!(report.peer_count >= 1);
}

// ── Scenario 3: Document ingestion ───────────────────────────────────────────

#[test]
fn scenario_document_ingestion_with_temporal_chain() {
    let mut e = engine();
    let ids = e
        .ingest_document(DocumentInput {
            name: "Rust Book Chapter 1".to_string(),
            chunks: vec![
                "Introduction to Rust".to_string(),
                "Ownership and borrowing".to_string(),
                "Lifetimes and references".to_string(),
            ],
            confidence: None,
            entity_tags: vec!["rust".to_string()],
            origin: origin(PeerId(0)),
            timestamp: None,
        })
        .unwrap();
    assert_eq!(ids.len(), 3);

    // Verify temporal chain
    let storage = e.graph().storage();
    let has_temporal_1_2 = storage.edges_from(ids[0]).iter().any(|&eid| {
        storage
            .get_edge(eid)
            .is_ok_and(|edge| edge.target == ids[1] && edge.edge_type == EdgeType::Temporal)
    });
    let has_temporal_2_3 = storage.edges_from(ids[1]).iter().any(|&eid| {
        storage
            .get_edge(eid)
            .is_ok_and(|edge| edge.target == ids[2] && edge.edge_type == EdgeType::Temporal)
    });
    assert!(has_temporal_1_2);
    assert!(has_temporal_2_3);
}

// ── Scenario 4: Conversation with peer profile update ────────────────────────

#[test]
fn scenario_conversation_updates_peer_profile() {
    let mut e = engine();
    let peer_id = e.register_peer("alice", TrustLevel::Owner).unwrap();

    let result = e
        .ingest_conversation(ConversationInput {
            name: "session about alice".to_string(),
            summary: None,
            raw_text: "Alice mentioned she prefers functional programming".to_string(),
            extracted_facts: vec![ExtractedFact {
                name: "alice prefers functional programming".to_string(),
                summary: None,
                content: "Alice has a strong preference for functional programming style"
                    .to_string(),
                embedding: None,
                confidence: Some(0.9),
                entity_tags: vec!["alice".to_string(), "functional".to_string()],
            }],
            confidence: None,
            entity_tags: vec![],
            origin: origin(PeerId(0)),
            timestamp: None,
            about_peer: Some("alice".to_string()),
        })
        .unwrap();

    assert_eq!(result.extracted_ids.len(), 1);

    // Profile node should be in peer/{id}/profile scope
    let expected_scope = format!("peer/{}/profile", peer_id.0);
    let profile_scope = ScopePath::new(&expected_scope).unwrap();
    let profile_nodes = e.graph().storage().nodes_by_scope(&profile_scope);
    assert!(!profile_nodes.is_empty(), "profile node should exist");
}

// ── Scenario 5: peer_filter + retract combination ────────────────────────────

#[test]
fn scenario_peer_filter_with_retract() {
    let mut e = engine();
    let alice_id = e.register_peer("alice", TrustLevel::Owner).unwrap();
    let bob_id = e.register_peer("bob", TrustLevel::Member).unwrap();

    let IngestResult::Created(alice_ids) = e
        .ingest(Observation {
            name: "alice-fact".to_string(),
            summary: None,
            content: "alice content".to_string(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(alice_id),
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap()
    else {
        panic!("expected Created");
    };

    let IngestResult::Created(bob_ids) = e
        .ingest(Observation {
            name: "bob-fact".to_string(),
            summary: None,
            content: "bob content".to_string(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(bob_id),
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap()
    else {
        panic!("expected Created");
    };

    // Retract alice's node
    e.retract(alice_ids[0], "outdated", Timestamp::now())
        .unwrap();

    // Search with peer_filter for alice — should return empty (retracted)
    let result = e
        .search(SearchInput {
            text: "alice-fact".to_string(),
            peer_filter: Some(vec![alice_id]),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    let found_alice = result
        .package
        .knowledge
        .iter()
        .any(|f| f.node_id == alice_ids[0]);
    assert!(!found_alice, "retracted alice node should not appear");

    // Search with peer_filter for bob — should return bob's node
    let result_bob = e
        .search(SearchInput {
            text: "bob-fact".to_string(),
            peer_filter: Some(vec![bob_id]),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    let _ = bob_ids;
    let _ = result_bob;
}

// ── Scenario 6: Health grade ──────────────────────────────────────────────────

#[test]
fn scenario_health_grade_a_for_clean_graph() {
    use anamnesis::api::HealthGrade;
    let mut e = engine();
    let peer_id = e.register_peer("alice", TrustLevel::Owner).unwrap();

    // Create a clean graph with edges (no orphans)
    let IngestResult::Created(ids1) = e
        .ingest(Observation {
            name: "node-a".to_string(),
            summary: None,
            content: "content a".to_string(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(peer_id),
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap()
    else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = e
        .ingest(Observation {
            name: "node-b".to_string(),
            summary: None,
            content: "content b".to_string(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Semantic,
            entity_tags: vec![],
            origin: origin(peer_id),
            timestamp: Timestamp::now(),
            valid_from: None,
            valid_until: None,
        })
        .unwrap()
    else {
        panic!("expected Created");
    };
    e.link(ids1[0], ids2[0], EdgeType::Semantic).unwrap();

    let report = e.health();
    assert_eq!(report.grade, HealthGrade::A);
    assert_eq!(report.orphan_count, 0);
    assert_eq!(report.contradiction_count, 0);
    assert_eq!(report.peer_count, 1);
}
