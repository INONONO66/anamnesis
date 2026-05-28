use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, NodeId, ScopePath, Timestamp};
use anamnesis::mechanics::topology;
use anamnesis::{Engine, EngineConfig, IngestResult};

fn origin(scope: &str) -> Origin {
    Origin {
        peer_id: anamnesis::graph::types::PeerId(0),
        source_kind: anamnesis::peer::SourceKind::AgentObservation,
        session_id: "session-1".to_string(),
        scope: ScopePath::new(scope).expect("valid scope"),
        confidence: 0.9,
    }
}

fn observation(name: &str, tags: &[&str], embedding: Option<Vec<f64>>) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("content for {name}"),
        embedding,
        confidence: 0.9,
        node_type: KnowledgeType::Semantic,
        entity_tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        origin: origin("topology-ingest"),
        timestamp: Timestamp(1000),
    }
}

fn config(topology_ingest_enabled: bool) -> EngineConfig {
    EngineConfig::new()
        .with_dedup_enabled(false)
        .with_novelty_threshold(0.0)
        .with_topology_ingest(topology_ingest_enabled)
}

fn ingest(engine: &mut Engine, name: &str, tags: &[&str], embedding: Option<Vec<f64>>) -> NodeId {
    match engine
        .ingest(observation(name, tags, embedding))
        .expect("ingest succeeds")
    {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("dedup is disabled"),
    }
}

fn add_recent_fillers(engine: &mut Engine, count: usize) {
    for index in 0..count {
        let name = format!("filler-{index}");
        let _ = ingest(engine, &name, &[], None);
    }
}

fn set_embedding(engine: &mut Engine, node_id: NodeId, embedding: Vec<f64>) {
    engine
        .graph_mut()
        .get_node_mut(node_id)
        .expect("node exists")
        .embedding = Some(embedding);
}

fn link(engine: &mut Engine, from: NodeId, to: NodeId) {
    engine
        .link(from, to, EdgeType::Semantic, 1.0)
        .expect("link succeeds");
}

fn created_id(result: IngestResult) -> NodeId {
    match result {
        IngestResult::Created(ids) => ids[0],
        IngestResult::Reinforced { .. } => panic!("dedup is disabled"),
    }
}

fn has_edge(engine: &Engine, source: NodeId, target: NodeId) -> bool {
    engine.graph().edges_from(source).iter().any(|edge_id| {
        engine
            .graph()
            .get_edge(*edge_id)
            .is_ok_and(|edge| edge.target == target)
    })
}

#[test]
fn topology_ingest_config_defaults_off_and_builder_enables() {
    assert!(!EngineConfig::default().topology_ingest_enabled);
    assert!(
        EngineConfig::new()
            .with_topology_ingest(true)
            .topology_ingest_enabled
    );
}

#[test]
fn neighborhood_expansion_finds_one_and_two_hop_neighbors() {
    let mut engine = Engine::with_config(config(true));
    let one_hop = ingest(&mut engine, "one-hop", &[], None);
    let two_hop = ingest(&mut engine, "two-hop", &[], None);
    let middle = ingest(&mut engine, "middle", &[], None);
    add_recent_fillers(&mut engine, 260);
    let trigger = ingest(&mut engine, "trigger", &["cue"], None);

    set_embedding(&mut engine, one_hop, vec![1.0, 0.0]);
    set_embedding(&mut engine, two_hop, vec![0.99, 0.01]);
    link(&mut engine, trigger, one_hop);
    link(&mut engine, trigger, middle);
    link(&mut engine, middle, two_hop);

    let new_id = created_id(
        engine
            .ingest(observation("new", &["cue"], Some(vec![1.0, 0.0])))
            .expect("ingest succeeds"),
    );

    assert!(has_edge(&engine, new_id, one_hop));
    assert!(has_edge(&engine, new_id, two_hop));
}

#[test]
fn old_structurally_important_neighbor_is_surfaced() {
    let mut engine = Engine::with_config(config(true));
    let bridge = ingest(&mut engine, "old-bridge", &[], None);
    let spoke_a = ingest(&mut engine, "spoke-a", &["a"], None);
    let spoke_b = ingest(&mut engine, "spoke-b", &["b"], None);
    let spoke_c = ingest(&mut engine, "spoke-c", &["c"], None);
    add_recent_fillers(&mut engine, 260);
    let trigger = ingest(&mut engine, "trigger", &["cue"], None);

    set_embedding(&mut engine, bridge, vec![1.0, 0.0]);
    link(&mut engine, trigger, bridge);
    link(&mut engine, bridge, spoke_a);
    link(&mut engine, bridge, spoke_b);
    link(&mut engine, bridge, spoke_c);

    let bridge_score = topology::bridge_score(engine.graph().storage(), bridge, 4)
        .expect("bridge score should be computable");
    assert!(
        bridge_score > 0.5,
        "expected high bridge score, got {bridge_score}"
    );

    let new_id = created_id(
        engine
            .ingest(observation("new", &["cue"], Some(vec![1.0, 0.0])))
            .expect("ingest succeeds"),
    );

    assert!(has_edge(&engine, new_id, bridge));
}

#[test]
fn expansion_is_bounded_to_depth_two() {
    let mut engine = Engine::with_config(config(true));
    let depth_three = ingest(&mut engine, "depth-three", &[], None);
    let depth_two = ingest(&mut engine, "depth-two", &[], None);
    let depth_one = ingest(&mut engine, "depth-one", &[], None);
    add_recent_fillers(&mut engine, 260);
    let trigger = ingest(&mut engine, "trigger", &["cue"], None);

    set_embedding(&mut engine, depth_three, vec![1.0, 0.0]);
    link(&mut engine, trigger, depth_one);
    link(&mut engine, depth_one, depth_two);
    link(&mut engine, depth_two, depth_three);

    let new_id = created_id(
        engine
            .ingest(observation("new", &["cue"], Some(vec![1.0, 0.0])))
            .expect("ingest succeeds"),
    );

    assert!(!has_edge(&engine, new_id, depth_three));
}

#[test]
fn disabled_topology_ingest_keeps_legacy_candidate_pool() {
    let mut engine = Engine::with_config(config(false));
    let old_neighbor = ingest(&mut engine, "old-neighbor", &[], None);
    add_recent_fillers(&mut engine, 260);
    let trigger = ingest(&mut engine, "trigger", &["cue"], None);

    set_embedding(&mut engine, old_neighbor, vec![1.0, 0.0]);
    link(&mut engine, trigger, old_neighbor);

    let new_id = created_id(
        engine
            .ingest(observation("new", &["cue"], Some(vec![1.0, 0.0])))
            .expect("ingest succeeds"),
    );

    assert!(!has_edge(&engine, new_id, old_neighbor));
}

#[test]
fn attraction_gate_still_blocks_final_edge_creation() {
    let mut engine = Engine::with_config(config(true));
    let candidate = ingest(&mut engine, "orthogonal-candidate", &[], None);
    let spoke_a = ingest(&mut engine, "spoke-a", &["a"], None);
    let spoke_b = ingest(&mut engine, "spoke-b", &["b"], None);
    let spoke_c = ingest(&mut engine, "spoke-c", &["c"], None);
    add_recent_fillers(&mut engine, 260);
    let trigger = ingest(&mut engine, "trigger", &["cue"], None);

    set_embedding(&mut engine, candidate, vec![0.0, 1.0]);
    link(&mut engine, trigger, candidate);
    link(&mut engine, candidate, spoke_a);
    link(&mut engine, candidate, spoke_b);
    link(&mut engine, candidate, spoke_c);

    let new_id = created_id(
        engine
            .ingest(observation("new", &["cue"], Some(vec![1.0, 0.0])))
            .expect("ingest succeeds"),
    );

    assert!(!has_edge(&engine, new_id, candidate));
}
