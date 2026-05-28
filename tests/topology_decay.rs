use anamnesis::api::{DecayModel, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, MemoryTier, ScopePath, Timestamp};
use anamnesis::{Engine, EngineConfig, IngestResult, NodeId, StorageAdapter};

const DAY_MS: u64 = 86_400_000;

fn origin(scope: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: ScopePath::new(scope).expect("valid scope"),
        confidence: 0.9,
    }
}

fn observation(name: &str, node_type: KnowledgeType, scope: &str, ts: Timestamp) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding: None,
        confidence: 0.9,
        node_type,
        entity_tags: vec![name.to_string()],
        origin: origin(scope),
        timestamp: ts,
    }
}

fn config() -> EngineConfig {
    EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false)
}

fn topology_config(isolation_factor: f64, bridge_factor: f64) -> EngineConfig {
    config()
        .with_topology_decay(true)
        .with_isolation_decay_factor(isolation_factor)
        .with_bridge_protection_factor(bridge_factor)
}

fn ingest(engine: &mut Engine, name: &str, node_type: KnowledgeType, scope: &str) -> NodeId {
    let result = engine
        .ingest(observation(name, node_type, scope, Timestamp(0)))
        .expect("ingest should succeed");
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("expected fresh node"),
    }
}

fn salience(engine: &Engine, id: NodeId) -> f64 {
    engine
        .graph()
        .storage()
        .get_salience(id)
        .expect("node salience should exist")
}

fn build_orphan_and_connected(engine: &mut Engine) -> (NodeId, NodeId) {
    let orphan = ingest(engine, "orphan", KnowledgeType::Semantic, "project-a");
    let connected = ingest(engine, "connected", KnowledgeType::Semantic, "project-a");
    let neighbor = ingest(engine, "neighbor", KnowledgeType::Semantic, "project-a");
    engine
        .link(connected, neighbor, EdgeType::Semantic, 1.0)
        .expect("link should succeed");
    (orphan, connected)
}

fn build_bridge_and_leaf(engine: &mut Engine) -> (NodeId, NodeId) {
    let bridge = ingest(engine, "bridge", KnowledgeType::Semantic, "project-a");
    let bridge_neighbor_a = ingest(engine, "bridge-a", KnowledgeType::Semantic, "project-b");
    let bridge_neighbor_b = ingest(engine, "bridge-b", KnowledgeType::Semantic, "project-c");
    let bridge_neighbor_c = ingest(engine, "bridge-c", KnowledgeType::Semantic, "project-d");
    let leaf = ingest(engine, "leaf", KnowledgeType::Semantic, "project-a");
    let leaf_neighbor = ingest(
        engine,
        "leaf-neighbor",
        KnowledgeType::Semantic,
        "project-a",
    );

    for neighbor in [bridge_neighbor_a, bridge_neighbor_b, bridge_neighbor_c] {
        engine
            .link(bridge, neighbor, EdgeType::Semantic, 1.0)
            .expect("bridge link should succeed");
    }
    engine
        .link(leaf, leaf_neighbor, EdgeType::Semantic, 1.0)
        .expect("leaf link should succeed");

    (bridge, leaf)
}

#[test]
fn orphan_nodes_decay_faster_when_topology_decay_enabled() {
    let mut engine = Engine::with_config(topology_config(1.0, 0.0));
    let (orphan, connected) = build_orphan_and_connected(&mut engine);

    engine.tick(Timestamp(30 * DAY_MS)).expect("tick succeeds");

    let orphan_salience = salience(&engine, orphan);
    let connected_salience = salience(&engine, connected);
    assert!(
        orphan_salience < connected_salience,
        "orphan should decay faster: orphan={orphan_salience}, connected={connected_salience}"
    );
}

#[test]
fn bridge_nodes_decay_slower_when_topology_decay_enabled() {
    let mut engine = Engine::with_config(topology_config(0.0, 0.8));
    let (bridge, leaf) = build_bridge_and_leaf(&mut engine);

    engine.tick(Timestamp(30 * DAY_MS)).expect("tick succeeds");

    let bridge_salience = salience(&engine, bridge);
    let leaf_salience = salience(&engine, leaf);
    assert!(
        bridge_salience > leaf_salience,
        "bridge should decay slower: bridge={bridge_salience}, leaf={leaf_salience}"
    );
}

#[test]
fn identity_core_and_core_tier_skip_topology_decay() {
    let mut engine = Engine::with_config(topology_config(2.0, 0.9));
    let identity = ingest(
        &mut engine,
        "identity-core",
        KnowledgeType::IdentityCore,
        "project-a",
    );
    let core = ingest(
        &mut engine,
        "core-tier",
        KnowledgeType::Semantic,
        "project-a",
    );
    engine
        .set_tier(core, MemoryTier::Core)
        .expect("set tier succeeds");

    let identity_before = salience(&engine, identity);
    let core_before = salience(&engine, core);

    engine.tick(Timestamp(365 * DAY_MS)).expect("tick succeeds");

    assert_eq!(salience(&engine, identity), identity_before);
    assert_eq!(salience(&engine, core), core_before);
}

#[test]
fn default_config_matches_topology_disabled_behavior() {
    let mut default_engine = Engine::with_config(config());
    let mut disabled_engine = Engine::with_config(
        config()
            .with_topology_decay(false)
            .with_isolation_decay_factor(1.0)
            .with_bridge_protection_factor(0.8),
    );

    let (default_orphan, default_connected) = build_orphan_and_connected(&mut default_engine);
    let (disabled_orphan, disabled_connected) = build_orphan_and_connected(&mut disabled_engine);

    default_engine
        .tick(Timestamp(30 * DAY_MS))
        .expect("default tick succeeds");
    disabled_engine
        .tick(Timestamp(30 * DAY_MS))
        .expect("disabled tick succeeds");

    assert_eq!(
        salience(&default_engine, default_orphan),
        salience(&disabled_engine, disabled_orphan)
    );
    assert_eq!(
        salience(&default_engine, default_connected),
        salience(&disabled_engine, disabled_connected)
    );
}

#[test]
fn zero_topology_factors_match_disabled_behavior() {
    let mut disabled_engine = Engine::with_config(config());
    let mut zero_factor_engine = Engine::with_config(topology_config(0.0, 0.0));

    let (disabled_bridge, disabled_leaf) = build_bridge_and_leaf(&mut disabled_engine);
    let (zero_bridge, zero_leaf) = build_bridge_and_leaf(&mut zero_factor_engine);

    disabled_engine
        .tick(Timestamp(30 * DAY_MS))
        .expect("disabled tick succeeds");
    zero_factor_engine
        .tick(Timestamp(30 * DAY_MS))
        .expect("zero-factor tick succeeds");

    assert_eq!(
        salience(&disabled_engine, disabled_bridge),
        salience(&zero_factor_engine, zero_bridge)
    );
    assert_eq!(
        salience(&disabled_engine, disabled_leaf),
        salience(&zero_factor_engine, zero_leaf)
    );
}

#[test]
fn touch_uses_topology_decay_before_reinforcement() {
    let mut engine = Engine::with_config(topology_config(1.0, 0.0));
    let (orphan, connected) = build_orphan_and_connected(&mut engine);

    engine
        .touch(orphan, Timestamp(30 * DAY_MS))
        .expect("orphan touch succeeds");
    engine
        .touch(connected, Timestamp(30 * DAY_MS))
        .expect("connected touch succeeds");

    let orphan_salience = salience(&engine, orphan);
    let connected_salience = salience(&engine, connected);
    assert!(
        orphan_salience < connected_salience,
        "topology-adjusted lazy decay should happen before reinforcement: orphan={orphan_salience}, connected={connected_salience}"
    );
}

#[test]
fn power_law_topology_decay_uses_effective_decay_parameter() {
    let mut power_law_config = topology_config(1.0, 0.0);
    power_law_config.decay_model = DecayModel::PowerLaw;
    let mut engine = Engine::with_config(power_law_config);
    let (orphan, connected) = build_orphan_and_connected(&mut engine);

    engine
        .touch(orphan, Timestamp(1))
        .expect("orphan touch succeeds");
    engine
        .touch(connected, Timestamp(1))
        .expect("connected touch succeeds");
    engine.tick(Timestamp(30 * DAY_MS)).expect("tick succeeds");

    let orphan_salience = salience(&engine, orphan);
    let connected_salience = salience(&engine, connected);
    assert!(
        orphan_salience < connected_salience,
        "PowerLaw topology decay should decay orphans faster: orphan={orphan_salience}, connected={connected_salience}"
    );
}
