use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::random_walk_restart;
use anamnesis::{Engine, EngineConfig, IngestResult, NodeId};

fn make_observation(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("RWR test node: {name}"),
        embedding: None,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            project_id: None,
            confidence: 1.0,
        },
        timestamp: Timestamp(0),
    }
}

fn make_engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn ingest_node(engine: &mut Engine, name: &str) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(make_observation(name)).unwrap() else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

#[test]
fn rwr_converges_on_small_graph() {
    let mut engine = make_engine();
    let seed = ingest_node(&mut engine, "seed");
    let light = ingest_node(&mut engine, "light");
    let heavy = ingest_node(&mut engine, "heavy");

    engine.link(seed, light, EdgeType::Semantic, 1.0).unwrap();
    engine.link(seed, heavy, EdgeType::Semantic, 3.0).unwrap();
    engine.link(light, heavy, EdgeType::Semantic, 1.0).unwrap();

    let scores = random_walk_restart(seed, 0.15, 128, engine.graph().storage());
    let scores_more = random_walk_restart(seed, 0.15, 256, engine.graph().storage());

    let total: f64 = scores.values().sum();
    assert!((total - 1.0).abs() < 1e-10, "probability leaked: {total}");
    assert!(scores.values().all(|score| score.is_finite()));
    assert!(
        scores.get(&heavy).copied().unwrap_or(0.0) > scores.get(&light).copied().unwrap_or(0.0)
    );

    for node_id in [seed, light, heavy] {
        let a = scores.get(&node_id).copied().unwrap_or(0.0);
        let b = scores_more.get(&node_id).copied().unwrap_or(0.0);
        assert!(
            (a - b).abs() < 1e-10,
            "node {node_id:?} did not converge: {a} vs {b}"
        );
    }
}

#[test]
fn rwr_seed_has_highest_activation() {
    let mut engine = make_engine();
    let seed = ingest_node(&mut engine, "seed");
    let left = ingest_node(&mut engine, "left");
    let right = ingest_node(&mut engine, "right");

    engine.link(seed, left, EdgeType::Semantic, 1.0).unwrap();
    engine.link(seed, right, EdgeType::Semantic, 1.0).unwrap();

    let scores = random_walk_restart(seed, 0.15, 128, engine.graph().storage());
    let seed_score = scores.get(&seed).copied().unwrap_or(0.0);

    assert!(seed_score > scores.get(&left).copied().unwrap_or(0.0));
    assert!(seed_score > scores.get(&right).copied().unwrap_or(0.0));
    assert!(scores.get(&left).copied().unwrap_or(0.0) > 0.0);
    assert!(scores.get(&right).copied().unwrap_or(0.0) > 0.0);
}
