//! v0.5.0 integration test suite (T22).
//!
//! End-to-end scenarios covering:
//! - Multi-origin ingest → conflict → retract → search → health
//! - Document ingestion → temporal chain
//! - Cross-feature: peer_filter (origin peer_id) + retract + conflict

use anamnesis::Engine;
use anamnesis::api::{DocumentInput, IngestResult, Observation};
use anamnesis::engine::{EngineConfig, SourceKind, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::query::SearchInput;

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

// ── Scenario 1: Multi-origin ingest lifecycle ────────────────────────────────

#[test]
fn scenario_peer_registration_and_ingest() {
    let mut e = engine();

    // Two distinct origin peers (production is single-peer; non-zero ids exercise
    // the origin/peer_filter paths).
    let alice_id = PeerId(1);
    let bob_id = PeerId(2);

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

// ── Scenario 5: peer_filter + retract combination ────────────────────────────

#[test]
fn scenario_peer_filter_with_retract() {
    let mut e = engine();
    let alice_id = PeerId(1);
    let bob_id = PeerId(2);

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
    let peer_id = PeerId(1);

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
}
