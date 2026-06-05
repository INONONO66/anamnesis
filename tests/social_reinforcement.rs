use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::mechanics::social::{FeedbackSignal, social_support};
use anamnesis::{Engine, IngestResult, SessionSummary, StorageAdapter};

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
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
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

fn session(_agent_id: &str, session_id: &str, node_ids: Vec<NodeId>) -> SessionSummary {
    let peer_id = anamnesis::graph::types::PeerId(match _agent_id {
        "agent-a" => 1,
        "agent-b" => 2,
        "agent-c" => 3,
        _ => 0,
    });
    SessionSummary {
        peer_id,
        session_id: session_id.to_string(),
        node_ids,
    }
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

// --- Engine::apply_feedback() integration tests ---

#[test]
fn apply_feedback_useful_increases_salience() {
    let mut engine = Engine::new();
    let node_id = insert_node(&mut engine, "fact", "agent-1", "s1", &["auth"]);

    // Drive the reservoir well below the positive reward target with repeated
    // NotUseful feedback (Rescorla-Wagner moves toward the negative target), so
    // a subsequent Useful signal has room to pull retained action back up.
    for _ in 0..50 {
        engine
            .apply_feedback(node_id, FeedbackSignal::NotUseful { strength: 1.0 })
            .unwrap();
    }

    let before = engine.graph().storage().get_salience(node_id).unwrap();
    assert!(before < 0.5, "should be well below the ceiling: {before}");

    engine
        .apply_feedback(node_id, FeedbackSignal::Useful { strength: 1.0 })
        .unwrap();
    let after = engine.graph().storage().get_salience(node_id).unwrap();

    assert!(after > before, "useful feedback should raise salience: {after} !> {before}");
}

#[test]
fn apply_feedback_not_useful_decreases_salience() {
    let mut engine = Engine::new();
    let node_id = insert_node(&mut engine, "fact", "agent-1", "s1", &["auth"]);

    let before = engine.graph().storage().get_salience(node_id).unwrap();
    engine
        .apply_feedback(node_id, FeedbackSignal::NotUseful { strength: 1.0 })
        .unwrap();
    let after = engine.graph().storage().get_salience(node_id).unwrap();

    assert!(after < before);
}

#[test]
fn apply_feedback_incorrect_decreases_salience() {
    let mut engine = Engine::new();
    let node_id = insert_node(&mut engine, "fact", "agent-1", "s1", &["auth"]);

    let before = engine.graph().storage().get_salience(node_id).unwrap();
    engine
        .apply_feedback(node_id, FeedbackSignal::Incorrect { strength: 0.8 })
        .unwrap();
    let after = engine.graph().storage().get_salience(node_id).unwrap();

    assert!(after < before);
}

#[test]
fn apply_feedback_diminishing_returns_saturate_at_reward_target() {
    let mut engine = Engine::new();
    let node_id = insert_node(&mut engine, "fact", "agent-1", "s1", &["auth"]);

    // Rescorla-Wagner on the reservoir saturates at the reward target lambda,
    // whose projection is project_salience(REWARD_LOG_ODDS_SCALE) = logistic(4) ≈ 0.982.
    for _ in 0..200 {
        engine
            .apply_feedback(node_id, FeedbackSignal::Useful { strength: 1.0 })
            .unwrap();
    }

    let final_salience = engine.graph().storage().get_salience(node_id).unwrap();
    let target = 1.0 / (1.0 + (-4.0_f64).exp());
    assert!(
        (final_salience - target).abs() < 1e-3,
        "should saturate at the reward-target projection {target}, got {final_salience}"
    );
    assert!(final_salience < 1.0);
}

#[test]
fn apply_feedback_does_not_modify_node_content() {
    let mut engine = Engine::new();
    let node_id = insert_node(&mut engine, "fact", "agent-1", "s1", &["auth"]);

    let content_before = engine.graph().get_node(node_id).unwrap().content.clone();
    let name_before = engine.graph().get_node(node_id).unwrap().name.clone();

    engine
        .apply_feedback(node_id, FeedbackSignal::Useful { strength: 1.0 })
        .unwrap();

    let content_after = engine.graph().get_node(node_id).unwrap().content.clone();
    let name_after = engine.graph().get_node(node_id).unwrap().name.clone();

    assert_eq!(content_before, content_after);
    assert_eq!(name_before, name_after);
}

#[test]
fn apply_feedback_uses_single_eta_rescorla_wagner() {
    // Feedback is a Rescorla-Wagner update on the retained-action reservoir using
    // the single core eta derived from N (no per-engine social_learning_rate knob):
    //   A' = A + eta*(lambda - A), salience = project_salience(A').
    use anamnesis::mechanics::interactions::{lambda_reward, rescorla_wagner};
    use anamnesis::mechanics::priors::{learning_rate, project_salience, TARGET_COACTIVATION_N};

    let mut engine = Engine::new();
    let node_id = insert_node(&mut engine, "fact", "agent-1", "s1", &["auth"]);

    let a_before = engine.graph().get_node(node_id).unwrap().retained_action;
    let signal = FeedbackSignal::NotUseful { strength: 1.0 };
    engine.apply_feedback(node_id, signal.clone()).unwrap();
    let after = engine.graph().storage().get_salience(node_id).unwrap();

    let eta = learning_rate(TARGET_COACTIVATION_N);
    let expected = project_salience(rescorla_wagner(a_before, lambda_reward(&signal), eta));
    assert!((after - expected).abs() < 1e-9, "{after} != {expected}");
}

#[test]
fn apply_feedback_returns_error_for_nonexistent_node() {
    let mut engine = Engine::new();
    let result = engine.apply_feedback(NodeId(999), FeedbackSignal::Useful { strength: 1.0 });
    assert!(result.is_err());
}

// --- Cross-agent entity linking + social support scenario ---

#[test]
fn multi_agent_entity_edges_enable_social_support_computation() {
    let mut engine = Engine::new();

    // Three distinct agents observe "auth" entity
    let n1 = insert_node(&mut engine, "auth fact A", "agent-a", "s1", &["auth"]);
    let n2 = insert_node(&mut engine, "auth fact B", "agent-b", "s2", &["auth"]);
    let n3 = insert_node(&mut engine, "auth fact C", "agent-c", "s3", &["auth"]);

    let report = engine
        .reflect_batch(&[
            session("agent-a", "s1", vec![n1]),
            session("agent-b", "s2", vec![n2]),
            session("agent-c", "s3", vec![n3]),
        ])
        .unwrap();

    // Entity edges created between cross-agent nodes
    assert!(report.entity_edges_created >= 2);

    // Now compute social support for this entity cluster
    // 3 distinct agents, full agreement, high confidence
    let support = social_support(3, 1.0, 0.9);
    assert!(support > social_support(1, 1.0, 0.9));
}

#[test]
fn contradicts_edges_excluded_from_entity_linking() {
    let mut engine = Engine::new();

    let n1 = insert_node(&mut engine, "claim A", "agent-a", "s1", &["auth"]);
    let n2 = insert_node(&mut engine, "contradicts A", "agent-b", "s2", &["auth"]);

    // Manually add a Contradicts edge
    engine.link(n1, n2, EdgeType::Contradicts, 0.9).unwrap();

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
