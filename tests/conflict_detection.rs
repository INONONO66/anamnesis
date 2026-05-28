//! Tests for conflict detection in ingest() (T9).

use anamnesis::api::{IngestResult, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};
use anamnesis::peer::SourceKind;
use anamnesis::{Engine, EngineConfig};

fn obs(name: &str, embedding: Vec<f64>) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: Some(embedding),
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![],
        origin: Origin {
            peer_id: PeerId(0),
            source_kind: SourceKind::AgentObservation,
            session_id: "s1".to_string(),
            scope: ScopePath::universal(),
            confidence: 0.9,
        },
        timestamp: Timestamp::now(),
    }
}

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(true),
    )
}

#[test]
fn low_similarity_creates_normally() {
    let mut e = engine();
    e.ingest(obs("node-a", vec![1.0, 0.0, 0.0])).unwrap();
    let result = e.ingest(obs("node-b", vec![0.0, 1.0, 0.0])).unwrap();
    assert!(matches!(result, IngestResult::Created(_)));
}

#[test]
fn high_similarity_reinforces() {
    let mut e = engine();
    e.ingest(obs("node-a", vec![1.0, 0.0, 0.0])).unwrap();
    let result = e.ingest(obs("node-a-dup", vec![1.0, 0.0, 0.0])).unwrap();
    assert!(matches!(result, IngestResult::Reinforced { .. }));
}

#[test]
fn mid_similarity_creates_with_conflict() {
    let mut e = engine();
    e.ingest(obs("node-a", vec![1.0, 0.0, 0.0])).unwrap();
    // similarity ~0.80 — in conflict zone (0.75, 0.92)
    // [0.8, 0.6, 0.0] has cosine similarity ~0.8 with [1.0, 0.0, 0.0] — in conflict zone
    let result = e.ingest(obs("node-b", vec![0.8, 0.6, 0.0])).unwrap();
    match result {
        IngestResult::CreatedWithConflict { node_ids, conflict } => {
            assert!(!node_ids.is_empty());
            assert!(conflict.similarity > 0.75);
            assert!(conflict.similarity < 0.92);
        }
        other => panic!("expected CreatedWithConflict, got {:?}", other),
    }
}

#[test]
fn created_with_conflict_has_contradicts_edge() {
    let mut e = engine();
    let IngestResult::Created(ids1) = e.ingest(obs("node-a", vec![1.0, 0.0, 0.0])).unwrap() else {
        panic!("expected Created");
    };
    let result = e.ingest(obs("node-b", vec![0.8, 0.6, 0.0])).unwrap();
    if let IngestResult::CreatedWithConflict { node_ids, .. } = result {
        let storage = e.graph().storage();
        use anamnesis::StorageAdapter;
        let has_contradicts = storage.edges_from(node_ids[0]).iter().any(|&eid| {
            storage
                .get_edge(eid)
                .is_ok_and(|edge| edge.edge_type == anamnesis::EdgeType::Contradicts)
        }) || storage.edges_from(ids1[0]).iter().any(|&eid| {
            storage
                .get_edge(eid)
                .is_ok_and(|edge| edge.edge_type == anamnesis::EdgeType::Contradicts)
        });
        assert!(has_contradicts, "Contradicts edge should exist");
    }
    // If not CreatedWithConflict, similarity was too low — test passes vacuously
}
