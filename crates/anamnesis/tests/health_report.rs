//! Tests for Engine::health() and HealthReport (T11).

use anamnesis::Engine;
use anamnesis::api::{HealthGrade, IngestResult, Observation};
use anamnesis::engine::EngineConfig;
use anamnesis::engine::SourceKind;
use anamnesis::graph::node::Origin;
use anamnesis::graph::types::PeerId;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};

fn obs(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
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
    Engine::with_config(EngineConfig::new().with_novelty_threshold(0.0))
}

#[test]
fn health_returns_report() {
    let e = engine();
    let report = e.health();
    assert_eq!(report.total_nodes, 0);
    assert_eq!(report.grade, HealthGrade::A);
}

#[test]
fn clean_graph_gets_grade_a() {
    let mut e = engine();
    let IngestResult::Created(ids1) = e.ingest(obs("node-a")).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = e.ingest(obs("node-b")).unwrap() else {
        panic!("expected Created");
    };
    e.link(ids1[0], ids2[0], EdgeType::Semantic).unwrap();
    let report = e.health();
    assert_eq!(report.grade, HealthGrade::A);
    assert_eq!(report.orphan_count, 0); // both nodes have edges
}

#[test]
fn orphan_nodes_lower_grade() {
    let mut e = engine();
    // Create many orphan nodes (no edges)
    for i in 0..20 {
        e.ingest(obs(&format!("orphan-{i}"))).unwrap();
    }
    let report = e.health();
    // 20 orphans out of 20 nodes = 100% orphan rate -> grade D
    assert!(report.orphan_count > 0);
    assert!(report.orphan_rate > 0.0);
    // With 100% orphan rate, grade should be worse than A (B, C, or D)
    assert!(
        report.grade > HealthGrade::A,
        "grade should be worse than A with many orphans"
    );
}

#[test]
fn contradiction_edges_counted() {
    let mut e = engine();
    let IngestResult::Created(ids1) = e.ingest(obs("node-a")).unwrap() else {
        panic!("expected Created");
    };
    let IngestResult::Created(ids2) = e.ingest(obs("node-b")).unwrap() else {
        panic!("expected Created");
    };
    e.link(ids1[0], ids2[0], EdgeType::Contradicts).unwrap();
    let report = e.health();
    assert_eq!(report.contradiction_count, 1);
}

#[test]
fn retracted_nodes_counted() {
    let mut e = engine();
    let IngestResult::Created(ids) = e.ingest(obs("node-a")).unwrap() else {
        panic!("expected Created");
    };
    e.retract(ids[0], "wrong", Timestamp::now()).unwrap();
    let report = e.health();
    assert_eq!(report.retracted_count, 1);
}
