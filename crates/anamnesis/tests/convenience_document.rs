//! Tests for ingest_document() convenience method (T16).

use anamnesis::Engine;
use anamnesis::api::DocumentInput;
use anamnesis::engine::SourceKind;
use anamnesis::engine::{EngineConfig, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, ScopePath};

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

#[test]
fn ingest_document_creates_chunk_nodes() {
    let mut e = engine();
    let ids = e
        .ingest_document(DocumentInput {
            name: "test-doc".to_string(),
            chunks: vec![
                "chunk 1 content".to_string(),
                "chunk 2 content".to_string(),
                "chunk 3 content".to_string(),
            ],
            confidence: None,
            entity_tags: vec![],
            origin: default_origin(),
            timestamp: None,
        })
        .unwrap();
    assert_eq!(ids.len(), 3);
}

#[test]
fn ingest_document_creates_temporal_edges() {
    let mut e = engine();
    let ids = e
        .ingest_document(DocumentInput {
            name: "test-doc".to_string(),
            chunks: vec![
                "chunk 1".to_string(),
                "chunk 2".to_string(),
                "chunk 3".to_string(),
            ],
            confidence: None,
            entity_tags: vec![],
            origin: default_origin(),
            timestamp: None,
        })
        .unwrap();
    // Verify Temporal edges: chunk1->chunk2->chunk3
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
    assert!(
        has_temporal_1_2,
        "chunk1 -> chunk2 Temporal edge should exist"
    );
    assert!(
        has_temporal_2_3,
        "chunk2 -> chunk3 Temporal edge should exist"
    );
}

#[test]
fn ingest_document_single_chunk_no_edges() {
    let mut e = engine();
    let ids = e
        .ingest_document(DocumentInput {
            name: "single-chunk".to_string(),
            chunks: vec!["only chunk".to_string()],
            confidence: None,
            entity_tags: vec![],
            origin: default_origin(),
            timestamp: None,
        })
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(e.graph().edge_count(), 0);
}
