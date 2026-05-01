use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::query::{ContextPackage, Fragment, Query, QueryConfig};
use anamnesis::{EnergyModel, Engine, EngineConfig, IngestResult, NodeId, SpreadingModel};

fn origin() -> Origin {
    Origin {
        agent_id: "agent-1".to_string(),
        session_id: "session-1".to_string(),
        scope: anamnesis::graph::ScopePath::universal(),
        confidence: 1.0,
    }
}

fn observation(name: &str, embedding: Option<Vec<f64>>) -> Observation {
    Observation {
        name: name.to_string(),
        summary: Some(format!("summary {name}")),
        content: format!("content {name}"),
        embedding,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: origin(),
        timestamp: Timestamp(0),
    }
}

fn engine_config() -> EngineConfig {
    EngineConfig::new()
        .with_novelty_threshold(0.0)
        .with_dedup_enabled(false)
}

fn ingest(engine: &mut Engine, name: &str, embedding: Option<Vec<f64>>) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name, embedding)).unwrap() else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

fn fragment_ids(package: &ContextPackage) -> Vec<NodeId> {
    package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .map(|fragment| fragment.node_id)
        .collect()
}

fn find_fragment(package: &ContextPackage, node_id: NodeId) -> &Fragment {
    package
        .identity
        .iter()
        .chain(package.knowledge.iter())
        .chain(package.memories.iter())
        .find(|fragment| fragment.node_id == node_id)
        .unwrap_or_else(|| panic!("missing fragment for node {node_id:?}"))
}

#[test]
fn random_walk_restart_routing_reaches_beyond_bfs_hop_limit() {
    let mut bfs_engine = Engine::with_config(engine_config());
    let seed = ingest(&mut bfs_engine, "seed", None);
    let middle = ingest(&mut bfs_engine, "middle", None);
    let downstream = ingest(&mut bfs_engine, "downstream", None);
    bfs_engine
        .link(seed, middle, EdgeType::Semantic, 1.0)
        .unwrap();
    bfs_engine
        .link(middle, downstream, EdgeType::Semantic, 1.0)
        .unwrap();

    let query = Query::Associative { seed, budget: 10 };
    let mut bfs_query_config = QueryConfig::default();
    bfs_query_config.max_hops = 1;
    bfs_query_config.min_activation = 0.001;
    let bfs_package = bfs_engine.query(&query, &bfs_query_config).unwrap();
    let bfs_ids = fragment_ids(&bfs_package);
    assert!(bfs_ids.contains(&seed));
    assert!(bfs_ids.contains(&middle));
    assert!(!bfs_ids.contains(&downstream));

    let mut rwr_config = engine_config();
    rwr_config.spreading_model = SpreadingModel::RandomWalkRestart;
    let mut rwr_engine = Engine::with_config(rwr_config);
    let rwr_seed = ingest(&mut rwr_engine, "seed", None);
    let rwr_middle = ingest(&mut rwr_engine, "middle", None);
    let rwr_downstream = ingest(&mut rwr_engine, "downstream", None);
    rwr_engine
        .link(rwr_seed, rwr_middle, EdgeType::Semantic, 1.0)
        .unwrap();
    rwr_engine
        .link(rwr_middle, rwr_downstream, EdgeType::Semantic, 1.0)
        .unwrap();

    let rwr_query = Query::Associative {
        seed: rwr_seed,
        budget: 10,
    };
    let rwr_package = rwr_engine.query(&rwr_query, &bfs_query_config).unwrap();
    let rwr_ids = fragment_ids(&rwr_package);
    assert!(rwr_ids.contains(&rwr_seed));
    assert!(rwr_ids.contains(&rwr_middle));
    assert!(rwr_ids.contains(&rwr_downstream));
    assert!(rwr_package.total_fragments() >= 3);
}

#[test]
fn hopfield_energy_routing_changes_relevance_when_embeddings_are_available() {
    let mut weighted_engine = Engine::with_config(engine_config());
    let seed = ingest(&mut weighted_engine, "seed", Some(vec![1.0, 1.0, 0.0, 0.0]));
    let intended = ingest(
        &mut weighted_engine,
        "intended",
        Some(vec![1.0, 1.0, 1.0, 1.0]),
    );
    let distractor = ingest(
        &mut weighted_engine,
        "distractor",
        Some(vec![1.0, -1.0, 1.0, -1.0]),
    );
    weighted_engine
        .link(seed, intended, EdgeType::Semantic, 1.0)
        .unwrap();
    weighted_engine
        .link(seed, distractor, EdgeType::Semantic, 1.0)
        .unwrap();

    let query = Query::Associative { seed, budget: 10 };
    let mut query_config = QueryConfig::default();
    query_config.query_embedding = Some(vec![1.0, 1.0, 0.0, 0.0]);
    query_config.min_activation = 0.001;
    let weighted_package = weighted_engine.query(&query, &query_config).unwrap();
    let weighted_intended = find_fragment(&weighted_package, intended).relevance;

    let mut hopfield_config = engine_config();
    hopfield_config.energy_model = EnergyModel::Hopfield;
    let mut hopfield_engine = Engine::with_config(hopfield_config);
    let hopfield_seed = ingest(&mut hopfield_engine, "seed", Some(vec![1.0, 1.0, 0.0, 0.0]));
    let hopfield_intended = ingest(
        &mut hopfield_engine,
        "intended",
        Some(vec![1.0, 1.0, 1.0, 1.0]),
    );
    let hopfield_distractor = ingest(
        &mut hopfield_engine,
        "distractor",
        Some(vec![1.0, -1.0, 1.0, -1.0]),
    );
    hopfield_engine
        .link(hopfield_seed, hopfield_intended, EdgeType::Semantic, 1.0)
        .unwrap();
    hopfield_engine
        .link(hopfield_seed, hopfield_distractor, EdgeType::Semantic, 1.0)
        .unwrap();

    let hopfield_query = Query::Associative {
        seed: hopfield_seed,
        budget: 10,
    };
    let hopfield_package = hopfield_engine
        .query(&hopfield_query, &query_config)
        .unwrap();
    let hopfield_intended_relevance = find_fragment(&hopfield_package, hopfield_intended).relevance;

    assert!(hopfield_package.total_fragments() >= 3);
    assert!(
        find_fragment(&hopfield_package, hopfield_distractor)
            .relevance
            .is_finite()
    );
    assert!(
        hopfield_intended_relevance > weighted_intended + 0.03,
        "expected Hopfield scoring to change relevance: weighted={weighted_intended}, hopfield={hopfield_intended_relevance}"
    );
}
