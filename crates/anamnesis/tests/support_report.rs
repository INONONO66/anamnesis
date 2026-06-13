use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::{IngestResult, StorageAdapter};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};

fn origin(_agent_id: &str, session_id: &str) -> Origin {
    let peer_id = anamnesis::graph::types::PeerId(match _agent_id {
        "agent-1" => 1,
        "agent-2" => 2,
        "agent-3" => 3,
        _ => 0,
    });
    Origin {
        peer_id,
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session_id.to_string(),
        scope: anamnesis::graph::ScopePath::new("project-1").expect("valid scope"),
        confidence: 0.9,
    }
}

fn observation(name: &str, _agent_id: &str, session_id: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Episodic,
        entity_tags: vec!["test".to_string()],
        origin: origin(_agent_id, session_id),
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

fn insert_node(engine: &mut Engine, name: &str, agent_id: &str, session_id: &str) -> NodeId {
    let IngestResult::Created(ids) = engine
        .ingest(observation(name, agent_id, session_id))
        .unwrap()
    else {
        panic!("expected node creation");
    };
    ids[0]
}

#[test]
fn support_report_empty_node() {
    let mut engine = Engine::new();
    let node = insert_node(&mut engine, "isolated", "agent-1", "session-1");

    let report = engine.support_report(node).unwrap();
    assert_eq!(report.supporting_sources, 0);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 0);
    assert_eq!(report.total_support_salience, 0.0);
}

#[test]
fn support_report_nonexistent_node() {
    let engine = Engine::new();
    let result = engine.support_report(NodeId(999));
    assert!(result.is_err());
}

#[test]
fn support_report_consolidated_from_edges() {
    let mut engine = Engine::new();

    // Create target node
    let target = insert_node(&mut engine, "target", "agent-1", "session-1");

    // Create 3 source nodes with different salience
    let source_a = insert_node(&mut engine, "source-a", "agent-1", "session-1");
    let source_b = insert_node(&mut engine, "source-b", "agent-2", "session-2");
    let source_c = insert_node(&mut engine, "source-c", "agent-1", "session-2");

    // Set salience for sources
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_a, 0.5)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_b, 0.7)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_c, 0.3)
        .unwrap();

    // Create ConsolidatedFrom edges: target -> sources
    engine
        .link(target, source_a, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine
        .link(target, source_b, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine
        .link(target, source_c, EdgeType::ConsolidatedFrom)
        .unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 3);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 3); // (agent-1, session-1), (agent-2, session-2), (agent-1, session-2)
    assert!((report.total_support_salience - 1.5).abs() < 1e-10); // 0.5 + 0.7 + 0.3
}

#[test]
fn support_report_reinforced_by_edges() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let source_a = insert_node(&mut engine, "source-a", "agent-1", "session-1");
    let source_b = insert_node(&mut engine, "source-b", "agent-1", "session-1");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_a, 0.6)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_b, 0.4)
        .unwrap();

    engine
        .link(target, source_a, EdgeType::ReinforcedBy)
        .unwrap();
    engine
        .link(target, source_b, EdgeType::ReinforcedBy)
        .unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 2);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 1); // All same (agent-1, session-1)
    assert!((report.total_support_salience - 1.0).abs() < 1e-10); // 0.6 + 0.4
}

#[test]
fn support_report_supports_edges() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let source = insert_node(&mut engine, "source", "agent-2", "session-3");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source, 0.8)
        .unwrap();

    engine.link(target, source, EdgeType::Supports).unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 1);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 1);
    assert!((report.total_support_salience - 0.8).abs() < 1e-10);
}

#[test]
fn support_report_contradicts_edges() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let contra_a = insert_node(&mut engine, "contra-a", "agent-2", "session-2");
    let contra_b = insert_node(&mut engine, "contra-b", "agent-3", "session-3");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(contra_a, 0.5)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(contra_b, 0.6)
        .unwrap();

    engine
        .link(target, contra_a, EdgeType::Contradicts)
        .unwrap();
    engine
        .link(target, contra_b, EdgeType::Contradicts)
        .unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 0);
    assert_eq!(report.contradicting_sources, 2);
    assert_eq!(report.independent_origins, 2); // (agent-2, session-2), (agent-3, session-3)
    assert_eq!(report.total_support_salience, 0.0); // No supporting sources
}

#[test]
fn support_report_counts_refutes_as_contradicting() {
    // `Refutes` is the debug-lifecycle counter-evidence edge that
    // `log_evidence(EvidenceResult::Contradicts)` creates (Evidence -> Hypothesis).
    // It must be counted as contradicting evidence, in both edge directions.
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    // Outgoing Refutes (target -> refuted_out).
    let refuted_out = insert_node(&mut engine, "refuted-out", "agent-2", "session-2");
    // Incoming Refutes (refuter_in -> target): the direction log_evidence produces.
    let refuter_in = insert_node(&mut engine, "refuter-in", "agent-3", "session-3");

    engine.link(target, refuted_out, EdgeType::Refutes).unwrap();
    engine.link(refuter_in, target, EdgeType::Refutes).unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 0);
    assert_eq!(report.contradicting_sources, 2); // counted in both directions
    assert_eq!(report.independent_origins, 2); // (agent-2, session-2), (agent-3, session-3)
    assert_eq!(report.total_support_salience, 0.0); // No supporting sources
}

#[test]
fn support_report_mixed_edges() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let support_a = insert_node(&mut engine, "support-a", "agent-1", "session-1");
    let support_b = insert_node(&mut engine, "support-b", "agent-2", "session-2");
    let contra = insert_node(&mut engine, "contra", "agent-3", "session-3");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(support_a, 0.4)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(support_b, 0.6)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(contra, 0.5)
        .unwrap();

    engine
        .link(target, support_a, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine
        .link(target, support_b, EdgeType::ReinforcedBy)
        .unwrap();
    engine.link(target, contra, EdgeType::Contradicts).unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 2);
    assert_eq!(report.contradicting_sources, 1);
    assert_eq!(report.independent_origins, 3); // (agent-1, session-1), (agent-2, session-2), (agent-3, session-3)
    assert!((report.total_support_salience - 1.0).abs() < 1e-10); // 0.4 + 0.6
}

#[test]
fn support_report_incoming_edges() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let source_a = insert_node(&mut engine, "source-a", "agent-2", "session-2");
    let source_b = insert_node(&mut engine, "source-b", "agent-3", "session-3");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_a, 0.5)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_b, 0.7)
        .unwrap();

    // Create incoming edges: source -> target
    engine
        .link(source_a, target, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine.link(source_b, target, EdgeType::Supports).unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 2);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 2);
    assert!((report.total_support_salience - 1.2).abs() < 1e-10); // 0.5 + 0.7
}

#[test]
fn support_report_circular_evidence_prevention() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let source_a = insert_node(&mut engine, "source-a", "agent-2", "session-2");
    let source_b = insert_node(&mut engine, "source-b", "agent-3", "session-3");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_a, 0.5)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_b, 0.6)
        .unwrap();

    // Create edges: target -> source_a -> target (circular)
    // and target -> source_b
    engine
        .link(target, source_a, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine
        .link(source_a, target, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine
        .link(target, source_b, EdgeType::ConsolidatedFrom)
        .unwrap();

    let report = engine.support_report(target).unwrap();
    // Should count source_a once (outgoing) and source_b once
    // The incoming edge from source_a back to target should not be counted
    // because source_a was already visited via outgoing edge
    assert_eq!(report.supporting_sources, 2);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 2);
    assert!((report.total_support_salience - 1.1).abs() < 1e-10); // 0.5 + 0.6
}

#[test]
fn support_report_ignores_other_edge_types() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let semantic = insert_node(&mut engine, "semantic", "agent-1", "session-1");
    let causal = insert_node(&mut engine, "causal", "agent-1", "session-1");
    let temporal = insert_node(&mut engine, "temporal", "agent-1", "session-1");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(semantic, 0.5)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(causal, 0.6)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(temporal, 0.7)
        .unwrap();

    // Create edges of types that should be ignored
    engine.link(target, semantic, EdgeType::Semantic).unwrap();
    engine.link(target, causal, EdgeType::Causal).unwrap();
    engine.link(target, temporal, EdgeType::Temporal).unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 0);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 0);
    assert_eq!(report.total_support_salience, 0.0);
}

#[test]
fn support_report_same_agent_different_sessions() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let source_a = insert_node(&mut engine, "source-a", "agent-1", "session-2");
    let source_b = insert_node(&mut engine, "source-b", "agent-1", "session-3");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_a, 0.5)
        .unwrap();
    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source_b, 0.6)
        .unwrap();

    engine
        .link(target, source_a, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine
        .link(target, source_b, EdgeType::ConsolidatedFrom)
        .unwrap();

    let report = engine.support_report(target).unwrap();
    assert_eq!(report.supporting_sources, 2);
    assert_eq!(report.contradicting_sources, 0);
    // Same agent but different sessions count as independent
    // Only sources are counted, not the target itself
    assert_eq!(report.independent_origins, 2); // (agent-1, session-2), (agent-1, session-3)
    assert!((report.total_support_salience - 1.1).abs() < 1e-10); // 0.5 + 0.6
}

#[test]
fn support_report_duplicate_edges_same_target() {
    let mut engine = Engine::new();

    let target = insert_node(&mut engine, "target", "agent-1", "session-1");
    let source = insert_node(&mut engine, "source", "agent-2", "session-2");

    engine
        .graph_mut()
        .storage_mut()
        .set_salience(source, 0.8)
        .unwrap();

    // Create two edges to the same target (should only count once due to visited set)
    engine
        .link(target, source, EdgeType::ConsolidatedFrom)
        .unwrap();
    engine.link(target, source, EdgeType::ReinforcedBy).unwrap();

    let report = engine.support_report(target).unwrap();
    // The visited set prevents counting the same node twice
    assert_eq!(report.supporting_sources, 1);
    assert_eq!(report.contradicting_sources, 0);
    assert_eq!(report.independent_origins, 1);
    assert!((report.total_support_salience - 0.8).abs() < 1e-10);
}
