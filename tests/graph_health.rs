use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, ScopePath, Timestamp};
use anamnesis::{Engine, EngineConfig, IngestResult, StorageAdapter};

fn make_origin(_agent: &str, session: &str, scope: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: session.to_string(),
        scope: ScopePath::new(scope).expect("valid scope"),
        confidence: 0.9,
    }
}

fn make_observation(name: &str, node_type: KnowledgeType) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("Content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: vec![],
        origin: make_origin("agent-1", "session-1", "project-a"),
        timestamp: Timestamp(1000),
    }
}

fn ingest_node(engine: &mut Engine, name: &str, node_type: KnowledgeType) -> anamnesis::NodeId {
    let result = engine.ingest(make_observation(name, node_type)).unwrap();
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    }
}

#[test]
fn empty_graph_returns_zero_counts_and_zero_entropy() {
    let engine = Engine::new();
    let health = engine.health();

    assert_eq!(health.node_count, 0);
    assert_eq!(health.edge_count, 0);
    assert_eq!(health.orphan_count, 0);
    assert_eq!(health.component_count, 0);
    assert_eq!(health.contradiction_count, 0);
    assert_eq!(health.supersede_count, 0);
    assert_eq!(health.salience_entropy, 0.0);
    assert_eq!(health.type_entropy, 0.0);
    assert_eq!(health.edge_type_entropy, 0.0);
    assert_eq!(health.bridge_candidate_count, 0);
}

#[test]
fn health_is_read_only_salience_unchanged() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let id1 = ingest_node(&mut engine, "node-1", KnowledgeType::Semantic);
    let id2 = ingest_node(&mut engine, "node-2", KnowledgeType::Episodic);

    let salience_before_1 = engine.graph().storage().get_salience(id1).unwrap();
    let salience_before_2 = engine.graph().storage().get_salience(id2).unwrap();

    let _health = engine.health();

    let salience_after_1 = engine.graph().storage().get_salience(id1).unwrap();
    let salience_after_2 = engine.graph().storage().get_salience(id2).unwrap();

    assert_eq!(salience_before_1, salience_after_1);
    assert_eq!(salience_before_2, salience_after_2);
}

#[test]
fn single_orphan_node() {
    let mut engine = Engine::new();
    ingest_node(&mut engine, "lonely", KnowledgeType::Semantic);

    let health = engine.health();
    assert_eq!(health.node_count, 1);
    assert_eq!(health.edge_count, 0);
    assert_eq!(health.orphan_count, 1);
    assert_eq!(health.component_count, 1);
}

#[test]
fn component_count_star_topology() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let center = ingest_node(&mut engine, "center", KnowledgeType::Entity);
    let leaf1 = ingest_node(&mut engine, "leaf-1", KnowledgeType::Semantic);
    let leaf2 = ingest_node(&mut engine, "leaf-2", KnowledgeType::Semantic);
    let leaf3 = ingest_node(&mut engine, "leaf-3", KnowledgeType::Semantic);

    engine.link(center, leaf1, EdgeType::Semantic, 0.8).unwrap();
    engine.link(center, leaf2, EdgeType::Semantic, 0.8).unwrap();
    engine.link(center, leaf3, EdgeType::Semantic, 0.8).unwrap();

    let health = engine.health();
    assert_eq!(health.node_count, 4);
    assert_eq!(health.edge_count, 3);
    assert_eq!(health.orphan_count, 0);
    assert_eq!(health.component_count, 1);
}

#[test]
fn component_count_disconnected_pairs() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let a1 = ingest_node(&mut engine, "a1", KnowledgeType::Semantic);
    let a2 = ingest_node(&mut engine, "a2", KnowledgeType::Semantic);
    let b1 = ingest_node(&mut engine, "b1", KnowledgeType::Semantic);
    let b2 = ingest_node(&mut engine, "b2", KnowledgeType::Semantic);
    let orphan = ingest_node(&mut engine, "orphan", KnowledgeType::Semantic);

    engine.link(a1, a2, EdgeType::Semantic, 0.8).unwrap();
    engine.link(b1, b2, EdgeType::Causal, 0.7).unwrap();

    let health = engine.health();
    assert_eq!(health.node_count, 5);
    assert_eq!(health.component_count, 3);
    assert_eq!(health.orphan_count, 1);
    let _ = orphan;
}

#[test]
fn contradiction_and_supersede_counts() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    let n3 = ingest_node(&mut engine, "n3", KnowledgeType::Semantic);
    let n4 = ingest_node(&mut engine, "n4", KnowledgeType::Semantic);

    engine.link(n1, n2, EdgeType::Contradicts, 0.9).unwrap();
    engine.link(n3, n4, EdgeType::Contradicts, 0.8).unwrap();
    engine.link(n1, n3, EdgeType::Supersedes, 0.7).unwrap();

    let health = engine.health();
    assert_eq!(health.contradiction_count, 2);
    assert_eq!(health.supersede_count, 1);
}

#[test]
fn entropy_zero_for_single_bucket_salience() {
    let mut engine = Engine::new();
    ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n3", KnowledgeType::Semantic);

    let health = engine.health();
    assert_eq!(health.salience_entropy, 0.0);
}

#[test]
fn entropy_zero_for_single_type() {
    let mut engine = Engine::new();
    ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n2", KnowledgeType::Semantic);

    let health = engine.health();
    assert_eq!(health.type_entropy, 0.0);
}

#[test]
fn type_entropy_positive_for_mixed_types() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    ingest_node(&mut engine, "semantic", KnowledgeType::Semantic);
    ingest_node(&mut engine, "episodic", KnowledgeType::Episodic);
    ingest_node(&mut engine, "entity", KnowledgeType::Entity);
    ingest_node(&mut engine, "decision", KnowledgeType::Decision);

    let health = engine.health();
    assert!(
        (health.type_entropy - 2.0).abs() < 1e-10,
        "expected 2.0, got {}",
        health.type_entropy
    );
}

#[test]
fn edge_type_entropy_positive_for_mixed_edges() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    let n3 = ingest_node(&mut engine, "n3", KnowledgeType::Semantic);
    let n4 = ingest_node(&mut engine, "n4", KnowledgeType::Semantic);

    engine.link(n1, n2, EdgeType::Semantic, 0.8).unwrap();
    engine.link(n2, n3, EdgeType::Causal, 0.7).unwrap();
    engine.link(n3, n4, EdgeType::Temporal, 0.6).unwrap();
    engine.link(n4, n1, EdgeType::Reason, 0.5).unwrap();

    let health = engine.health();
    assert!(
        (health.edge_type_entropy - 2.0).abs() < 1e-10,
        "expected 2.0, got {}",
        health.edge_type_entropy
    );
}

#[test]
fn edge_type_entropy_zero_for_single_type() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Semantic);
    let n3 = ingest_node(&mut engine, "n3", KnowledgeType::Semantic);

    engine.link(n1, n2, EdgeType::Semantic, 0.8).unwrap();
    engine.link(n2, n3, EdgeType::Semantic, 0.7).unwrap();

    let health = engine.health();
    assert_eq!(health.edge_type_entropy, 0.0);
}

#[test]
fn bridge_candidates_with_cross_scope_connections() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let obs_bridge = Observation {
        name: "bridge".to_string(),
        summary: None,
        content: "Bridge node".to_string(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Entity,
        entity_tags: vec!["shared".to_string()],
        origin: make_origin("agent-1", "session-1", "project-a"),
        timestamp: Timestamp(1000),
    };
    let bridge_id = match engine.ingest(obs_bridge).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    };

    let obs_a = Observation {
        name: "scope-a-node".to_string(),
        summary: None,
        content: "Node in scope A".to_string(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["tag-a".to_string()],
        origin: make_origin("agent-1", "session-1", "project-b"),
        timestamp: Timestamp(1000),
    };
    let a_id = match engine.ingest(obs_a).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    };

    let obs_b = Observation {
        name: "scope-b-node".to_string(),
        summary: None,
        content: "Node in scope B".to_string(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["tag-b".to_string()],
        origin: make_origin("agent-1", "session-1", "project-c"),
        timestamp: Timestamp(1000),
    };
    let b_id = match engine.ingest(obs_b).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    };

    let obs_c = Observation {
        name: "scope-c-node".to_string(),
        summary: None,
        content: "Node in scope C".to_string(),
        embedding: None,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec!["tag-c".to_string()],
        origin: make_origin("agent-1", "session-1", "project-d"),
        timestamp: Timestamp(1000),
    };
    let c_id = match engine.ingest(obs_c).unwrap() {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { existing_id, .. } => existing_id,
    };

    engine
        .link(bridge_id, a_id, EdgeType::Semantic, 0.9)
        .unwrap();
    engine
        .link(bridge_id, b_id, EdgeType::Semantic, 0.9)
        .unwrap();
    engine
        .link(bridge_id, c_id, EdgeType::Semantic, 0.9)
        .unwrap();

    let health = engine.health();
    assert!(
        health.bridge_candidate_count >= 1,
        "expected at least 1 bridge candidate, got {}",
        health.bridge_candidate_count
    );
}

#[test]
fn node_count_and_edge_count_match_storage() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    let n1 = ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    let n2 = ingest_node(&mut engine, "n2", KnowledgeType::Episodic);
    let n3 = ingest_node(&mut engine, "n3", KnowledgeType::Entity);

    engine.link(n1, n2, EdgeType::Semantic, 0.8).unwrap();
    engine.link(n2, n3, EdgeType::Causal, 0.7).unwrap();

    let health = engine.health();
    assert_eq!(health.node_count, engine.graph().node_count());
    assert_eq!(health.edge_count, engine.graph().edge_count());
}

#[test]
fn multiple_calls_return_consistent_results() {
    let config = EngineConfig::new().with_novelty_threshold(0.0);
    let mut engine = Engine::with_config(config);

    ingest_node(&mut engine, "n1", KnowledgeType::Semantic);
    ingest_node(&mut engine, "n2", KnowledgeType::Episodic);

    let h1 = engine.health();
    let h2 = engine.health();
    assert_eq!(h1, h2);
}
