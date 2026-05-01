use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, Timestamp};
use anamnesis::{DebugOutcome, Engine, Error, EvidenceResult};

fn origin() -> Origin {
    Origin {
        agent_id: "agent-1".to_string(),
        session_id: "session-1".to_string(),
        scope: anamnesis::graph::ScopePath::new("project-1").expect("valid scope"),
        confidence: 0.9,
    }
}

fn start_session(engine: &mut Engine) -> NodeId {
    engine
        .start_debug(
            "Intermittent cache invalidation failure",
            origin(),
            Timestamp(1000),
        )
        .unwrap()
}

fn log_hypothesis(engine: &mut Engine, session: NodeId) -> NodeId {
    engine
        .log_hypothesis(
            session,
            "Cache key omits tenant namespace",
            origin(),
            Timestamp(1100),
        )
        .unwrap()
}

fn edge_between(engine: &Engine, source: NodeId, target: NodeId, edge_type: EdgeType) -> bool {
    engine.graph().edges_from(source).iter().any(|edge_id| {
        engine.graph().get_edge(*edge_id).is_ok_and(|edge| {
            edge.source == source && edge.target == target && edge.edge_type == edge_type
        })
    })
}

#[test]
fn start_debug_creates_debug_session_node_with_metadata() {
    let mut engine = Engine::new();

    let session = engine
        .start_debug("Parser returns stale errors", origin(), Timestamp(42))
        .unwrap();

    let node = engine.graph().get_node(session).unwrap();
    assert_eq!(node.node_type, KnowledgeType::DebugSession);
    assert_eq!(node.name, "Parser returns stale errors");
    assert_eq!(node.content, "Parser returns stale errors");
    assert_eq!(node.created_at, Timestamp(42));
    assert_eq!(node.updated_at, Timestamp(42));
    assert_eq!(node.salience, 1.0);
    assert_eq!(
        node.metadata.get("debug_kind").map(String::as_str),
        Some("session")
    );
    assert_eq!(
        node.metadata.get("debug_started_at").map(String::as_str),
        Some("42")
    );
}

#[test]
fn log_hypothesis_creates_hypothesis_and_belongs_to_edge() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);

    let hypothesis = log_hypothesis(&mut engine, session);

    let node = engine.graph().get_node(hypothesis).unwrap();
    assert_eq!(node.node_type, KnowledgeType::Hypothesis);
    assert_eq!(
        node.metadata.get("debug_kind").map(String::as_str),
        Some("hypothesis")
    );
    assert_eq!(
        node.metadata.get("debug_session_id").map(String::as_str),
        Some(session.0.to_string().as_str())
    );
    assert_eq!(
        node.metadata.get("hypothesis_status").map(String::as_str),
        Some("open")
    );
    assert_eq!(engine.graph().node_count(), 2);
    assert_eq!(engine.graph().edge_count(), 1);
    assert!(edge_between(
        &engine,
        hypothesis,
        session,
        EdgeType::BelongsTo
    ));
}

#[test]
fn log_evidence_creates_supports_and_refutes_edges() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);
    let hypothesis = log_hypothesis(&mut engine, session);

    let supporting = engine
        .log_evidence(
            hypothesis,
            "Failing requests share the same missing tenant key",
            EvidenceResult::Supports,
            origin(),
            Timestamp(1200),
        )
        .unwrap();
    let refuting = engine
        .log_evidence(
            hypothesis,
            "A failing request has the expected tenant key",
            EvidenceResult::Contradicts,
            origin(),
            Timestamp(1300),
        )
        .unwrap();

    assert_eq!(
        engine.graph().get_node(supporting).unwrap().node_type,
        KnowledgeType::Evidence
    );
    assert_eq!(
        engine.graph().get_node(refuting).unwrap().node_type,
        KnowledgeType::Evidence
    );
    assert!(edge_between(
        &engine,
        supporting,
        hypothesis,
        EdgeType::Supports
    ));
    assert!(edge_between(
        &engine,
        refuting,
        hypothesis,
        EdgeType::Refutes
    ));
    assert_eq!(engine.graph().edge_count(), 3);
}

#[test]
fn neutral_and_inconclusive_evidence_do_not_link_or_reject() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);
    let hypothesis = log_hypothesis(&mut engine, session);

    let neutral = engine
        .log_evidence(
            hypothesis,
            "Log volume is unchanged between passing and failing runs",
            EvidenceResult::Neutral,
            origin(),
            Timestamp(1200),
        )
        .unwrap();
    let inconclusive = engine
        .log_evidence(
            hypothesis,
            "Trace sample is truncated before cache lookup",
            EvidenceResult::Inconclusive,
            origin(),
            Timestamp(1300),
        )
        .unwrap();

    for evidence in [neutral, inconclusive] {
        let node = engine.graph().get_node(evidence).unwrap();
        assert_eq!(node.node_type, KnowledgeType::Evidence);
        assert_eq!(
            node.metadata
                .get("automatic_hypothesis_action")
                .map(String::as_str),
            Some("none")
        );
        assert!(engine.graph().edges_from(evidence).is_empty());
    }
    assert_eq!(engine.graph().edge_count(), 1);
    let hypothesis_node = engine.graph().get_node(hypothesis).unwrap();
    assert_eq!(
        hypothesis_node
            .metadata
            .get("hypothesis_status")
            .map(String::as_str),
        Some("open")
    );
}

#[test]
fn reject_and_confirm_hypothesis_write_string_metadata() {
    let mut engine = Engine::new();
    let first_session = start_session(&mut engine);
    let rejected = log_hypothesis(&mut engine, first_session);
    let second_session = start_session(&mut engine);
    let confirmed = log_hypothesis(&mut engine, second_session);

    engine
        .reject_hypothesis(
            rejected,
            "Tenant key is present in failing traces",
            Timestamp(2000),
        )
        .unwrap();
    engine
        .confirm_hypothesis(
            confirmed,
            "Tenant namespace was missing in derived keys",
            Timestamp(2100),
        )
        .unwrap();

    let rejected_node = engine.graph().get_node(rejected).unwrap();
    assert_eq!(
        rejected_node
            .metadata
            .get("hypothesis_status")
            .map(String::as_str),
        Some("rejected")
    );
    assert_eq!(
        rejected_node
            .metadata
            .get("rejection_reason")
            .map(String::as_str),
        Some("Tenant key is present in failing traces")
    );
    assert_eq!(
        rejected_node
            .metadata
            .get("rejected_at")
            .map(String::as_str),
        Some("2000")
    );

    let confirmed_node = engine.graph().get_node(confirmed).unwrap();
    assert_eq!(
        confirmed_node
            .metadata
            .get("hypothesis_status")
            .map(String::as_str),
        Some("confirmed")
    );
    assert_eq!(
        confirmed_node
            .metadata
            .get("confirmation_conclusion")
            .map(String::as_str),
        Some("Tenant namespace was missing in derived keys")
    );
    assert_eq!(
        confirmed_node
            .metadata
            .get("confirmed_at")
            .map(String::as_str),
        Some("2100")
    );
}

#[test]
fn end_debug_records_resolved_unresolved_and_abandoned_outcomes() {
    let mut engine = Engine::new();
    let resolved = start_session(&mut engine);
    let unresolved = start_session(&mut engine);
    let abandoned = start_session(&mut engine);

    engine
        .end_debug(
            resolved,
            DebugOutcome::Resolved("Fixed by namespacing cache keys".to_string()),
            Timestamp(3000),
        )
        .unwrap();
    engine
        .end_debug(
            unresolved,
            DebugOutcome::Unresolved("Need production trace sampling".to_string()),
            Timestamp(3100),
        )
        .unwrap();
    engine
        .end_debug(abandoned, DebugOutcome::Abandoned, Timestamp(3200))
        .unwrap();

    let resolved_node = engine.graph().get_node(resolved).unwrap();
    assert_eq!(
        resolved_node
            .metadata
            .get("debug_outcome")
            .map(String::as_str),
        Some("resolved")
    );
    assert_eq!(
        resolved_node
            .metadata
            .get("debug_resolution")
            .map(String::as_str),
        Some("Fixed by namespacing cache keys")
    );
    assert_eq!(
        resolved_node
            .metadata
            .get("debug_ended_at")
            .map(String::as_str),
        Some("3000")
    );

    let unresolved_node = engine.graph().get_node(unresolved).unwrap();
    assert_eq!(
        unresolved_node
            .metadata
            .get("debug_outcome")
            .map(String::as_str),
        Some("unresolved")
    );
    assert_eq!(
        unresolved_node
            .metadata
            .get("debug_unresolved_reason")
            .map(String::as_str),
        Some("Need production trace sampling")
    );

    let abandoned_node = engine.graph().get_node(abandoned).unwrap();
    assert_eq!(
        abandoned_node
            .metadata
            .get("debug_outcome")
            .map(String::as_str),
        Some("abandoned")
    );
    assert_eq!(
        abandoned_node
            .metadata
            .get("debug_ended_at")
            .map(String::as_str),
        Some("3200")
    );
}

#[test]
fn debug_api_validates_referenced_node_existence_and_type() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);
    let hypothesis = log_hypothesis(&mut engine, session);

    let missing = engine.log_hypothesis(NodeId(999), "Missing session", origin(), Timestamp(1));
    assert!(matches!(missing, Err(Error::NodeNotFound(NodeId(999)))));

    let wrong_session = engine.log_hypothesis(hypothesis, "Wrong type", origin(), Timestamp(1));
    assert!(
        matches!(wrong_session, Err(Error::InvalidInput(message)) if message.contains("DebugSession"))
    );

    let wrong_hypothesis = engine.log_evidence(
        session,
        "Wrong type",
        EvidenceResult::Supports,
        origin(),
        Timestamp(1),
    );
    assert!(
        matches!(wrong_hypothesis, Err(Error::InvalidInput(message)) if message.contains("Hypothesis"))
    );

    let missing_hypothesis = engine.reject_hypothesis(NodeId(998), "missing", Timestamp(1));
    assert!(matches!(
        missing_hypothesis,
        Err(Error::NodeNotFound(NodeId(998)))
    ));

    let wrong_end = engine.end_debug(hypothesis, DebugOutcome::Abandoned, Timestamp(1));
    assert!(
        matches!(wrong_end, Err(Error::InvalidInput(message)) if message.contains("DebugSession"))
    );
}

#[test]
fn debug_types_are_reexported_from_crate_root() {
    let supports = EvidenceResult::Supports;
    let outcome = DebugOutcome::Abandoned;

    assert_eq!(supports, EvidenceResult::Supports);
    assert_eq!(outcome, DebugOutcome::Abandoned);
}

#[test]
fn search_rejected_hypotheses_returns_only_rejected_matching_query() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);

    let h_rejected_cache = engine
        .log_hypothesis(
            session,
            "Cache key omits tenant namespace",
            origin(),
            Timestamp(1100),
        )
        .unwrap();
    let h_rejected_timeout = engine
        .log_hypothesis(
            session,
            "Request timeout causes stale reads",
            origin(),
            Timestamp(1200),
        )
        .unwrap();
    let h_confirmed = engine
        .log_hypothesis(session, "Cache TTL is too short", origin(), Timestamp(1300))
        .unwrap();

    engine
        .reject_hypothesis(
            h_rejected_cache,
            "Tenant key present in traces",
            Timestamp(2000),
        )
        .unwrap();
    engine
        .reject_hypothesis(
            h_rejected_timeout,
            "Timeouts are unrelated to staleness",
            Timestamp(2100),
        )
        .unwrap();
    engine
        .confirm_hypothesis(h_confirmed, "TTL was indeed misconfigured", Timestamp(2200))
        .unwrap();

    let results = engine.search_rejected_hypotheses("cache", 10).unwrap();
    assert_eq!(results, vec![h_rejected_cache]);

    let results = engine.search_rejected_hypotheses("timeout", 10).unwrap();
    assert_eq!(results, vec![h_rejected_timeout]);

    let results = engine.search_rejected_hypotheses("TTL", 10).unwrap();
    assert!(results.is_empty(), "confirmed hypothesis must not appear");
}

#[test]
fn search_rejected_hypotheses_matches_rejection_reason() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);

    let h = engine
        .log_hypothesis(
            session,
            "DNS resolution is flaky",
            origin(),
            Timestamp(1100),
        )
        .unwrap();
    engine
        .reject_hypothesis(h, "DNS latency is within normal bounds", Timestamp(2000))
        .unwrap();

    let results = engine
        .search_rejected_hypotheses("normal bounds", 10)
        .unwrap();
    assert_eq!(results, vec![h], "should match rejection_reason text");

    let results = engine
        .search_rejected_hypotheses("NORMAL BOUNDS", 10)
        .unwrap();
    assert_eq!(
        results,
        vec![h],
        "case-insensitive match on rejection_reason"
    );
}

#[test]
fn search_rejected_hypotheses_respects_limit() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);

    let mut rejected_ids = Vec::new();
    for i in 0..5 {
        let h = engine
            .log_hypothesis(
                session,
                &format!("Hypothesis about cache issue {i}"),
                origin(),
                Timestamp(1100 + i * 100),
            )
            .unwrap();
        engine
            .reject_hypothesis(
                h,
                &format!("Disproved cache issue {i}"),
                Timestamp(2000 + i * 100),
            )
            .unwrap();
        rejected_ids.push(h);
    }

    let results = engine.search_rejected_hypotheses("cache", 2).unwrap();
    assert_eq!(results.len(), 2);

    let results = engine.search_rejected_hypotheses("cache", 0).unwrap();
    assert!(results.is_empty(), "limit=0 must return empty");
}

#[test]
fn search_rejected_hypotheses_empty_query_returns_all_rejected() {
    let mut engine = Engine::new();
    let session = start_session(&mut engine);

    let h1 = engine
        .log_hypothesis(
            session,
            "Memory leak in worker pool",
            origin(),
            Timestamp(1100),
        )
        .unwrap();
    let h2 = engine
        .log_hypothesis(
            session,
            "Thread starvation under load",
            origin(),
            Timestamp(1200),
        )
        .unwrap();
    let h_open = engine
        .log_hypothesis(
            session,
            "Still investigating GC pauses",
            origin(),
            Timestamp(1300),
        )
        .unwrap();

    engine
        .reject_hypothesis(h1, "No leak detected with valgrind", Timestamp(2000))
        .unwrap();
    engine
        .reject_hypothesis(h2, "Thread count is stable", Timestamp(2100))
        .unwrap();

    let results = engine.search_rejected_hypotheses("", 10).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.contains(&h1));
    assert!(results.contains(&h2));
    assert!(
        !results.contains(&h_open),
        "open hypothesis must not appear"
    );
}

#[test]
fn search_rejected_hypotheses_full_debug_lifecycle() {
    let mut engine = Engine::new();

    let session = engine
        .start_debug(
            "API returns 500 on large payloads",
            origin(),
            Timestamp(1000),
        )
        .unwrap();

    let h_body_parsing = engine
        .log_hypothesis(
            session,
            "Body parser chokes on payloads > 10MB",
            origin(),
            Timestamp(1100),
        )
        .unwrap();
    let h_oom = engine
        .log_hypothesis(
            session,
            "OOM kill from unbounded buffer",
            origin(),
            Timestamp(1200),
        )
        .unwrap();
    let h_serialization = engine
        .log_hypothesis(
            session,
            "Serialization timeout on nested objects",
            origin(),
            Timestamp(1300),
        )
        .unwrap();

    engine
        .log_evidence(
            h_body_parsing,
            "Parser handles 50MB in test harness without error",
            EvidenceResult::Contradicts,
            origin(),
            Timestamp(1400),
        )
        .unwrap();
    engine
        .reject_hypothesis(
            h_body_parsing,
            "Parser handles large payloads fine",
            Timestamp(1500),
        )
        .unwrap();

    engine
        .log_evidence(
            h_oom,
            "dmesg shows OOM kill for the API process",
            EvidenceResult::Supports,
            origin(),
            Timestamp(1600),
        )
        .unwrap();
    engine
        .confirm_hypothesis(
            h_oom,
            "Unbounded buffer causes OOM on large payloads",
            Timestamp(1700),
        )
        .unwrap();

    engine
        .log_evidence(
            h_serialization,
            "Logs show no serialization timeout entries",
            EvidenceResult::Neutral,
            origin(),
            Timestamp(1800),
        )
        .unwrap();
    engine
        .reject_hypothesis(
            h_serialization,
            "No evidence of serialization issues",
            Timestamp(1900),
        )
        .unwrap();

    engine
        .end_debug(
            session,
            DebugOutcome::Resolved("OOM from unbounded buffer; added streaming parser".to_string()),
            Timestamp(2000),
        )
        .unwrap();

    let rejected = engine.search_rejected_hypotheses("", 10).unwrap();
    assert_eq!(rejected.len(), 2);
    assert!(rejected.contains(&h_body_parsing));
    assert!(rejected.contains(&h_serialization));
    assert!(
        !rejected.contains(&h_oom),
        "confirmed hypothesis must not appear in rejected search"
    );

    let parser_results = engine.search_rejected_hypotheses("parser", 10).unwrap();
    assert_eq!(parser_results, vec![h_body_parsing]);

    let serial_results = engine
        .search_rejected_hypotheses("serialization", 10)
        .unwrap();
    assert_eq!(serial_results, vec![h_serialization]);

    let oom_results = engine.search_rejected_hypotheses("OOM", 10).unwrap();
    assert!(
        oom_results.is_empty(),
        "confirmed hypothesis must not appear"
    );
}
