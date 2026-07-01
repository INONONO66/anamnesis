use anamnesis_core::{
    EdgeEndpoint, EdgeId, EdgeKind, EntityKind, MemoryId, MemoryKind, NodeId, NodeKind, Origin,
    OriginInput, ScopePath, SourceKind, TemporalValidity, Timestamp, valid_at,
};

#[test]
fn ids_and_time_are_distinct_coordinate_spaces() {
    let node = NodeId::new(7);
    let edge = EdgeId::new(7);
    let memory = MemoryId::new(7);

    assert_eq!(node.get(), edge.get());
    assert_eq!(memory.get(), 7);
    assert_eq!(Timestamp::from_millis(42).as_millis(), 42);
}

#[test]
fn scope_paths_are_stable_and_hierarchical() {
    let project = ScopePath::from_segments(["project", "anamnesis"]).unwrap();
    let child = ScopePath::from_segments(["project", "anamnesis", "core"]).unwrap();

    assert!(project.contains(&child));
    assert!(!child.contains(&project));
    assert_eq!(ScopePath::universal().as_str(), "universal");
}

#[test]
fn malformed_scope_paths_are_rejected() {
    assert!(ScopePath::from_segments(["project", ""]).is_err());
    assert!(ScopePath::from_segments(["project/core"]).is_err());
    assert!(ScopePath::from_segments(["universal"]).is_err());
}

#[test]
fn origin_preserves_source_scope_and_confidence() {
    let origin = Origin::new(OriginInput {
        agent_id: "codex".to_string(),
        source_kind: SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: ScopePath::from_segments(["project", "anamnesis"]).unwrap(),
        confidence: 0.82,
    })
    .unwrap();

    assert_eq!(origin.agent_id(), "codex");
    assert_eq!(origin.session_id(), "session-1");
    assert_eq!(origin.confidence().get(), 0.82);
}

#[test]
fn origin_rejects_invalid_confidence_and_empty_identity() {
    let scope = ScopePath::from_segments(["project", "anamnesis"]).unwrap();

    assert!(
        Origin::new(OriginInput {
            agent_id: String::new(),
            source_kind: SourceKind::HumanInput,
            session_id: "session-1".to_string(),
            scope: scope.clone(),
            confidence: 0.5,
        })
        .is_err()
    );
    assert!(
        Origin::new(OriginInput {
            agent_id: "codex".to_string(),
            source_kind: SourceKind::HumanInput,
            session_id: String::new(),
            scope: scope.clone(),
            confidence: 0.5,
        })
        .is_err()
    );
    assert!(
        Origin::new(OriginInput {
            agent_id: "codex".to_string(),
            source_kind: SourceKind::HumanInput,
            session_id: "session-1".to_string(),
            scope,
            confidence: 1.1,
        })
        .is_err()
    );
}

#[test]
fn temporal_validity_supports_as_of_queries() {
    let validity = TemporalValidity::new(
        Some(Timestamp::from_millis(10)),
        Some(Timestamp::from_millis(20)),
    )
    .unwrap();

    assert!(!valid_at(validity, Timestamp::from_millis(9)));
    assert!(valid_at(validity, Timestamp::from_millis(10)));
    assert!(valid_at(validity, Timestamp::from_millis(19)));
    assert!(!valid_at(validity, Timestamp::from_millis(20)));
    assert!(
        TemporalValidity::new(
            Some(Timestamp::from_millis(20)),
            Some(Timestamp::from_millis(10)),
        )
        .is_err()
    );
}

#[test]
fn edge_endpoint_records_fact_shape_without_engine_policy() {
    let endpoint = EdgeEndpoint::new(NodeId::new(1), NodeId::new(2), EdgeKind::Derives);

    assert_eq!(endpoint.source(), NodeId::new(1));
    assert_eq!(endpoint.target(), NodeId::new(2));
    assert_eq!(endpoint.kind(), EdgeKind::Derives);
}

#[test]
fn schema_kinds_stay_stable() {
    assert_eq!(NodeKind::Source.as_str(), "source");
    assert_eq!(MemoryKind::Evidence.as_str(), "evidence");
    assert_eq!(EntityKind::Agent.as_str(), "agent");
    assert_eq!(EdgeKind::Sequence.as_str(), "sequence");
}
