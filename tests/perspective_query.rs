use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::query::QueryConfig;
use anamnesis::{Engine, EngineConfig, IngestResult, ObservedRef, PerspectiveKey};

fn make_observation(
    name: &str,
    agent_id: &str,
    tags: &[&str],
    timestamp: Timestamp,
    scope: &ScopePath,
) -> Observation {
    // Map agent string to a stable PeerId for test isolation
    let peer_id = anamnesis::graph::types::PeerId(match agent_id {
        "agent-1" => 1,
        "agent-2" => 2,
        _ => 0,
    });
    Observation {
        name: name.to_string(),
        summary: Some(format!("Summary of {name}")),
        content: format!("Full content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags.iter().map(|t| (*t).to_string()).collect(),
        origin: Origin {
            peer_id,
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: scope.clone(),
            confidence: 0.9,
        },
        timestamp,
        valid_from: None,
        valid_until: None,
    }
}

fn ingest(engine: &mut Engine, obs: Observation) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(obs).expect("ingest should succeed") else {
        panic!("expected Created");
    };
    ids[0]
}

fn default_config() -> QueryConfig {
    QueryConfig::default()
}

#[test]
fn observer_only_fragments_returned() {
    let mut engine = Engine::with_config(EngineConfig::new().with_dedup_enabled(false));
    let scope = ScopePath::new("project").expect("valid");
    let ts = Timestamp(1000);

    let a1_node = ingest(
        &mut engine,
        make_observation("agent-1 knows auth", "agent-1", &["auth"], ts, &scope),
    );
    let a2_node = ingest(
        &mut engine,
        make_observation("agent-2 knows auth", "agent-2", &["auth"], ts, &scope),
    );
    engine
        .link(a1_node, a2_node, EdgeType::Semantic)
        .expect("link");

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1), // agent-1
        observed: ObservedRef::EntityTag("auth".to_string()),
        scope: ScopePath::universal(),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    let all_node_ids: Vec<NodeId> = pkg
        .knowledge
        .iter()
        .chain(pkg.identity.iter())
        .chain(pkg.memories.iter())
        .map(|f| f.node_id)
        .collect();

    assert!(
        all_node_ids.contains(&a1_node),
        "observer's own node should be in results"
    );
    // agent-2's node may appear via spreading activation (it's connected),
    // but agent-1's node MUST be present as a seed
    assert!(pkg.total_fragments() >= 1);
}

#[test]
fn observed_ref_entity_tag_filters_correctly() {
    let mut engine = Engine::with_config(EngineConfig::new().with_dedup_enabled(false));
    let scope = ScopePath::new("project").expect("valid");
    let ts = Timestamp(1000);

    let _auth_node = ingest(
        &mut engine,
        make_observation("agent-1 auth fact", "agent-1", &["auth"], ts, &scope),
    );
    let _db_node = ingest(
        &mut engine,
        make_observation("agent-1 db fact", "agent-1", &["database"], ts, &scope),
    );

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1), // agent-1
        observed: ObservedRef::EntityTag("auth".to_string()),
        scope: ScopePath::universal(),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    // Only the auth-tagged node should be a seed
    assert!(pkg.total_fragments() >= 1);
    let has_auth = pkg.knowledge.iter().any(|f| f.name.contains("auth"));
    assert!(has_auth, "auth-tagged node should appear in results");
}

#[test]
fn observed_ref_agent_filters_correctly() {
    let mut engine = Engine::with_config(EngineConfig::new().with_dedup_enabled(false));
    let scope = ScopePath::new("project").expect("valid");
    let ts = Timestamp(1000);

    let a1_node = ingest(
        &mut engine,
        make_observation("agent-1 observes auth", "agent-1", &["auth"], ts, &scope),
    );
    let a2_node = ingest(
        &mut engine,
        make_observation("agent-2 auth work", "agent-2", &["auth"], ts, &scope),
    );
    // agent-1 links to agent-2's node (has observed it)
    engine
        .link(a1_node, a2_node, EdgeType::Semantic)
        .expect("link");

    // Also add agent-1 node with no connection to agent-2
    let _isolated = ingest(
        &mut engine,
        make_observation("agent-1 unrelated", "agent-1", &["unrelated"], ts, &scope),
    );

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1), // agent-1
        observed: ObservedRef::Agent(anamnesis::graph::types::PeerId(2)), // agent-2
        scope: ScopePath::universal(),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    // a1_node is connected to agent-2's work, so it should be in results
    let result_ids: Vec<NodeId> = pkg
        .knowledge
        .iter()
        .chain(pkg.memories.iter())
        .map(|f| f.node_id)
        .collect();
    assert!(
        result_ids.contains(&a1_node),
        "observer node connected to target agent should appear"
    );
}

#[test]
fn observed_ref_node_filters_correctly() {
    let mut engine = Engine::with_config(EngineConfig::new().with_dedup_enabled(false));
    let scope = ScopePath::new("project").expect("valid");
    let ts = Timestamp(1000);

    let target = ingest(
        &mut engine,
        make_observation("shared entity", "agent-1", &["entity"], ts, &scope),
    );
    let connected = ingest(
        &mut engine,
        make_observation("connected to target", "agent-1", &["other"], ts, &scope),
    );
    let _disconnected = ingest(
        &mut engine,
        make_observation("no connection", "agent-1", &["isolated"], ts, &scope),
    );
    engine
        .link(connected, target, EdgeType::Semantic)
        .expect("link");

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1),
        observed: ObservedRef::Node(target),
        scope: ScopePath::universal(),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    let result_ids: Vec<NodeId> = pkg
        .knowledge
        .iter()
        .chain(pkg.memories.iter())
        .chain(pkg.identity.iter())
        .map(|f| f.node_id)
        .collect();

    // target itself (owned by agent-1) and connected node should appear
    assert!(result_ids.contains(&target), "target node should appear");
    assert!(
        result_ids.contains(&connected),
        "connected node should appear"
    );
}

#[test]
fn scope_filtering_applied() {
    let mut engine = Engine::with_config(EngineConfig::new().with_dedup_enabled(false));
    let scope_a = ScopePath::new("work/team-a").expect("valid");
    let scope_b = ScopePath::new("work/team-b").expect("valid");
    let ts = Timestamp(1000);

    let _in_scope = ingest(
        &mut engine,
        make_observation("agent-1 team-a work", "agent-1", &["auth"], ts, &scope_a),
    );
    let _out_scope = ingest(
        &mut engine,
        make_observation("agent-1 team-b work", "agent-1", &["auth"], ts, &scope_b),
    );

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1),
        observed: ObservedRef::EntityTag("auth".to_string()),
        scope: ScopePath::new("work/team-a").expect("valid"),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    // Only team-a scoped node should be a seed
    let names: Vec<&str> = pkg.knowledge.iter().map(|f| f.name.as_str()).collect();
    assert!(
        names.iter().any(|n| n.contains("team-a")),
        "in-scope node should be present"
    );
    // team-b node should not appear as a direct result (it may still appear via activation
    // spreading through edges, but since there are no edges here, it won't)
    let has_team_b_direct = pkg.knowledge.iter().any(|f| f.name.contains("team-b"));
    assert!(
        !has_team_b_direct,
        "out-of-scope node should not appear without edge connections"
    );
}

#[test]
fn non_retroactive_excludes_pre_join_events() {
    let mut engine = Engine::with_config(EngineConfig::new().with_dedup_enabled(false));
    let scope = ScopePath::new("project").expect("valid");

    // agent-2 creates a node at t=500 (before agent-1 joins)
    let pre_join = ingest(
        &mut engine,
        make_observation(
            "pre-existing fact",
            "agent-2",
            &["auth"],
            Timestamp(500),
            &scope,
        ),
    );

    // agent-1 joins at t=1000
    let a1_node = ingest(
        &mut engine,
        make_observation(
            "agent-1 first note",
            "agent-1",
            &["auth"],
            Timestamp(1000),
            &scope,
        ),
    );

    // Link agent-1's node to the pre-existing node
    engine
        .link(a1_node, pre_join, EdgeType::Semantic)
        .expect("link");

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1), // agent-1
        observed: ObservedRef::EntityTag("auth".to_string()),
        scope: ScopePath::universal(),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    // The pre-join node (t=500) should be excluded by non-retroactive filter
    // since agent-1's earliest timestamp is 1000
    let result_ids: Vec<NodeId> = pkg
        .knowledge
        .iter()
        .chain(pkg.memories.iter())
        .chain(pkg.identity.iter())
        .map(|f| f.node_id)
        .collect();

    assert!(
        !result_ids.contains(&pre_join),
        "pre-join node should be excluded by non-retroactive filter"
    );
    assert!(
        result_ids.contains(&a1_node),
        "observer's own node should be present"
    );
}

#[test]
fn empty_observer_returns_empty_package() {
    let engine = Engine::new();

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1),
        observed: ObservedRef::EntityTag("anything".to_string()),
        scope: ScopePath::universal(),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    assert_eq!(pkg.total_fragments(), 0);
}

#[test]
fn no_matching_observed_ref_returns_empty() {
    let mut engine = Engine::with_config(EngineConfig::new().with_dedup_enabled(false));
    let scope = ScopePath::new("project").expect("valid");
    let ts = Timestamp(1000);

    let _node = ingest(
        &mut engine,
        make_observation("agent-1 knows db", "agent-1", &["database"], ts, &scope),
    );

    let perspective = PerspectiveKey {
        observer_peer_id: anamnesis::graph::types::PeerId(1),
        observed: ObservedRef::EntityTag("nonexistent-tag".to_string()),
        scope: ScopePath::universal(),
    };

    let pkg = engine
        .query_perspective(perspective, &default_config())
        .expect("query_perspective");

    assert_eq!(pkg.total_fragments(), 0);
}
