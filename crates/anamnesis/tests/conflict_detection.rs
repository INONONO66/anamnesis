//! Tests for ingest routing after the auto-Contradicts heuristic was removed.
//!
//! The legacy ingest heuristic created a `Contradicts` edge whenever a new
//! observation landed in a mid-similarity "conflict zone". That heuristic
//! violated the non-destructive / frustration-as-surfaced-stress principle
//! (ADR-0006): contradictions surface during retrieval from explicit
//! `Contradicts` edges, not from an ingest-time similarity guess. Ingest now
//! only ever allocates a new site (`Created`) or routes to an existing one
//! (`Reinforced`); it never fabricates a contradiction.

use anamnesis::Engine;
use anamnesis::api::{IngestResult, Observation};
use anamnesis::engine::SourceKind;
use anamnesis::engine::{EdgeType, EngineConfig, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{KnowledgeType, ScopePath, Timestamp};

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
        valid_from: None,
        valid_until: None,
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
fn mid_similarity_creates_without_conflict() {
    let mut e = engine();
    e.ingest(obs("node-a", vec![1.0, 0.0, 0.0])).unwrap();
    // similarity ~0.80 — formerly the "conflict zone". Ingest now allocates a
    // plain new site with no auto-Contradicts heuristic.
    let result = e.ingest(obs("node-b", vec![0.8, 0.6, 0.0])).unwrap();
    assert!(
        matches!(result, IngestResult::Created(_)),
        "mid-similarity must create normally, got {result:?}"
    );
}

#[test]
fn ingest_never_fabricates_contradicts_edge() {
    let mut e = engine();
    let IngestResult::Created(ids1) = e.ingest(obs("node-a", vec![1.0, 0.0, 0.0])).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = e.ingest(obs("node-b", vec![0.8, 0.6, 0.0])).unwrap() else {
        panic!("expected Created");
    };

    let storage = e.graph().storage();
    let has_contradicts = storage.edges_from(ids2[0]).iter().any(|&eid| {
        storage
            .get_edge(eid)
            .is_ok_and(|edge| edge.edge_type == EdgeType::Contradicts)
    }) || storage.edges_from(ids1[0]).iter().any(|&eid| {
        storage
            .get_edge(eid)
            .is_ok_and(|edge| edge.edge_type == EdgeType::Contradicts)
    });
    assert!(
        !has_contradicts,
        "ingest must not fabricate a Contradicts edge"
    );
}
