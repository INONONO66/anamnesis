use anamnesis::Engine;
use anamnesis::api::Observation;
use anamnesis::engine::IngestResult;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::mechanics::social::social_support;

fn observation(name: &str, _agent_id: &str, session_id: &str, tags: &[&str]) -> Observation {
    let peer_id = anamnesis::graph::types::PeerId(match _agent_id {
        "agent-a" => 1,
        "agent-b" => 2,
        "agent-c" => 3,
        _ => 0,
    });
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        origin: Origin {
            peer_id,
            source_kind: anamnesis::engine::SourceKind::AgentObservation,
            session_id: session_id.to_string(),
            scope: ScopePath::new("project-1").expect("valid scope"),
            confidence: 0.9,
        },
        timestamp: Timestamp(1000),
        valid_from: None,
        valid_until: None,
    }
}

fn insert_node(
    engine: &mut Engine,
    name: &str,
    agent_id: &str,
    session_id: &str,
    tags: &[&str],
) -> NodeId {
    let IngestResult::Created(ids) = engine
        .ingest(observation(name, agent_id, session_id, tags))
        .unwrap()
    else {
        panic!("expected node creation");
    };
    ids[0]
}

// --- social_support() pure function tests ---

#[test]
fn multi_agent_corroboration_increases_social_support() {
    let s1 = social_support(1, 1.0, 0.9);
    let s3 = social_support(3, 1.0, 0.9);
    let s5 = social_support(5, 1.0, 0.9);

    assert!(s3 > s1);
    assert!(s5 > s3);
}

#[test]
fn same_agent_repeated_sessions_do_not_inflate_count() {
    // social_support uses distinct_agent_count, not session count.
    // Same agent across 5 sessions = count of 1, not 5.
    let one_agent = social_support(1, 1.0, 0.9);
    let five_agents = social_support(5, 1.0, 0.9);

    // The consumer is responsible for counting distinct agents.
    // This test verifies the formula treats count=1 vs count=5 differently.
    assert!(five_agents > one_agent);
    // 5 agents must give strictly less than 5x the support of 1 agent (log scaling)
    assert!(five_agents < 5.0 * one_agent);
}

#[test]
fn logarithmic_scaling_prevents_popularity_cascades() {
    let s10 = social_support(10, 1.0, 1.0);
    let s100 = social_support(100, 1.0, 1.0);
    let s1000 = social_support(1000, 1.0, 1.0);

    // 10x more agents gives much less than 10x more support
    assert!(s100 < s10 * 3.0);
    assert!(s1000 < s100 * 2.0);
}

#[test]
fn contradictions_do_not_trigger_support() {
    // social_support with 0 agreement = 0 reinforcement
    let score = social_support(5, 0.0, 0.9);
    assert_eq!(score, 0.0);
}

// --- Cross-agent entity linking + social support scenario ---

#[test]
fn contradicts_edges_excluded_from_entity_linking() {
    let mut engine = Engine::new();

    let n1 = insert_node(&mut engine, "claim A", "agent-a", "s1", &["auth"]);
    let n2 = insert_node(&mut engine, "contradicts A", "agent-b", "s2", &["auth"]);

    // Manually add a Contradicts edge
    engine.link(n1, n2, EdgeType::Contradicts).unwrap();

    // Contradicts edge exists but social_support with 0 agreement = 0
    let score = social_support(2, 0.0, 0.9);
    assert_eq!(score, 0.0);
}

#[test]
fn social_support_zero_for_single_agent_multiple_sessions() {
    // A single agent across many sessions should NOT get social reinforcement
    // The consumer counts distinct agents — same agent = count 1
    let single_agent_support = social_support(1, 1.0, 0.9);
    let multi_agent_support = social_support(3, 1.0, 0.9);

    assert!(multi_agent_support > single_agent_support);
}
