use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::{Engine, IngestResult, SessionSummary};

fn observation(name: &str, agent_id: &str, session_id: &str, tags: &[&str]) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        origin: Origin {
            agent_id: agent_id.to_string(),
            session_id: session_id.to_string(),
            project_id: Some("project-1".to_string()),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
    }
}

fn insert_node(engine: &mut Engine, name: &str, agent_id: &str, tags: &[&str]) -> NodeId {
    let IngestResult::Created(ids) = engine
        .ingest(observation(name, agent_id, "session-1", tags))
        .unwrap()
    else {
        panic!("expected node creation");
    };
    ids[0]
}

fn session(agent_id: &str, node_ids: Vec<NodeId>) -> SessionSummary {
    SessionSummary {
        agent_id: agent_id.to_string(),
        session_id: "session-1".to_string(),
        node_ids,
    }
}

#[test]
fn shared_tag_across_two_agents_creates_entity_edge() {
    let mut engine = Engine::new();
    let first = insert_node(&mut engine, "first auth note", "agent-a", &["auth"]);
    let second = insert_node(&mut engine, "second auth note", "agent-b", &["auth"]);

    let report = engine
        .reflect_batch(&[
            session("agent-a", vec![first]),
            session("agent-a", vec![second]),
        ])
        .unwrap();

    assert_eq!(report.entity_edges_created, 1);
    assert_eq!(report.clusters_found, 1);
    assert_eq!(engine.graph().edge_count(), 1);

    let edge = engine
        .graph()
        .edges_from(first)
        .iter()
        .find_map(|edge_id| engine.graph().get_edge(*edge_id).ok())
        .unwrap();
    assert_eq!(edge.source, first);
    assert_eq!(edge.target, second);
    assert_eq!(edge.edge_type, EdgeType::Entity);
    assert_eq!(edge.weight, 1.0);
    assert!(edge.weight.is_finite() && edge.weight > 0.0);
}

#[test]
fn same_agent_nodes_are_ignored() {
    let mut engine = Engine::new();
    let first = insert_node(&mut engine, "first auth note", "agent-a", &["auth"]);
    let second = insert_node(&mut engine, "second auth note", "agent-a", &["auth"]);

    let report = engine
        .reflect_batch(&[
            session("agent-a", vec![first]),
            session("agent-a", vec![second]),
        ])
        .unwrap();

    assert_eq!(report.entity_edges_created, 0);
    assert_eq!(report.clusters_found, 0);
    assert_eq!(engine.graph().edge_count(), 0);
}

#[test]
fn duplicate_calls_and_multiple_shared_tags_do_not_duplicate_edges() {
    let mut engine = Engine::new();
    let first = insert_node(&mut engine, "first auth note", "agent-a", &["auth", "api"]);
    let second = insert_node(&mut engine, "second auth note", "agent-b", &["api", "auth"]);
    let sessions = vec![
        session("agent-a", vec![first]),
        session("agent-b", vec![second]),
    ];

    let first_report = engine.reflect_batch(&sessions).unwrap();
    let second_report = engine.reflect_batch(&sessions).unwrap();

    assert_eq!(first_report.entity_edges_created, 1);
    assert_eq!(first_report.clusters_found, 2);
    assert_eq!(second_report.entity_edges_created, 0);
    assert_eq!(second_report.clusters_found, 2);
    assert_eq!(engine.graph().edge_count(), 1);
}

#[test]
fn missing_node_ids_are_skipped() {
    let mut engine = Engine::new();
    let first = insert_node(&mut engine, "first auth note", "agent-a", &["auth"]);

    let report = engine
        .reflect_batch(&[session("agent-a", vec![first, NodeId(999)])])
        .unwrap();

    assert_eq!(report.entity_edges_created, 0);
    assert_eq!(report.clusters_found, 0);
    assert_eq!(engine.graph().edge_count(), 0);
}
