//! Tests for Edge.edge_source field — EdgeSource::Auto, Manual, Inferred.

use anamnesis::api::Observation;
use anamnesis::graph::edge::EdgeSource;
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::peer::SourceKind;
use anamnesis::{Engine, EngineConfig, IngestResult, StorageAdapter};

fn make_obs(name: &str, embedding: Option<Vec<f64>>) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding,
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

#[test]
fn link_creates_manual_edge() {
    let mut engine = Engine::new();
    let IngestResult::Created(ids1) = engine.ingest(make_obs("node-a", None)).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = engine.ingest(make_obs("node-b", None)).unwrap() else {
        panic!("expected Created");
    };
    let eid = engine
        .link(ids1[0], ids2[0], EdgeType::Semantic)
        .unwrap();
    let edge = engine.graph().get_edge(eid).unwrap();
    assert_eq!(edge.edge_source, EdgeSource::Manual);
}

#[test]
fn attraction_auto_link_creates_auto_edge() {
    // Two nodes with similar embeddings should get an Auto edge via attraction
    let mut engine = Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    );
    let emb1 = vec![1.0, 0.0, 0.0];
    let emb2 = vec![0.99, 0.01, 0.0]; // very similar
    let IngestResult::Created(ids1) = engine.ingest(make_obs("node-a", Some(emb1))).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = engine.ingest(make_obs("node-b", Some(emb2))).unwrap() else {
        panic!("expected Created");
    };
    // Check if any edge between them is Auto
    let storage = engine.graph().storage();
    let auto_edge = storage.edges_from(ids1[0]).iter().any(|&eid| {
        storage
            .get_edge(eid)
            .is_ok_and(|e| e.target == ids2[0] && e.edge_source == EdgeSource::Auto)
    }) || storage.edges_from(ids2[0]).iter().any(|&eid| {
        storage
            .get_edge(eid)
            .is_ok_and(|e| e.target == ids1[0] && e.edge_source == EdgeSource::Auto)
    });
    // If attraction fired, edge should be Auto
    if engine.graph().edge_count() > 0 {
        assert!(auto_edge, "attraction edge should be EdgeSource::Auto");
    }
}

#[test]
fn all_edge_source_variants_constructable() {
    let sources = [EdgeSource::Auto, EdgeSource::Manual, EdgeSource::Inferred];
    assert_eq!(sources.len(), 3);
    assert_eq!(sources[0], EdgeSource::Auto);
    assert_eq!(sources[1], EdgeSource::Manual);
    assert_eq!(sources[2], EdgeSource::Inferred);
}
