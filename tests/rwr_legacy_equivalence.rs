use std::collections::HashMap;

use anamnesis::api::Observation;
use anamnesis::graph::node::Origin;
use anamnesis::graph::{EdgeType, KnowledgeType, Timestamp};
use anamnesis::{Engine, EngineConfig, IngestResult, NodeId, StorageAdapter};

const DEFAULT_RESTART_PROBABILITY: f64 = 0.15;

fn observation(name: &str) -> Observation {
    Observation {
        name: name.to_string(),
        summary: None,
        content: format!("RWR legacy test node: {name}"),
        embedding: None,
        confidence: 1.0,
        node_type: KnowledgeType::Semantic,
        entity_tags: vec![name.to_string()],
        origin: Origin {
            peer_id: anamnesis::graph::types::PeerId(0),
            source_kind: anamnesis::peer::SourceKind::AgentObservation,
            session_id: "session-1".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 1.0,
        },
        timestamp: Timestamp(0),
        valid_from: None,
        valid_until: None,
    }
}

fn engine() -> Engine {
    Engine::with_config(
        EngineConfig::new()
            .with_novelty_threshold(0.0)
            .with_dedup_enabled(false),
    )
}

fn ingest(engine: &mut Engine, name: &str) -> NodeId {
    let IngestResult::Created(ids) = engine.ingest(observation(name)).unwrap() else {
        panic!("expected Created for {name}");
    };
    ids[0]
}

fn restart_probability(alpha: f64) -> f64 {
    if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        DEFAULT_RESTART_PROBABILITY
    }
}

fn live_node_ids(storage: &impl StorageAdapter) -> Vec<NodeId> {
    storage
        .all_node_ids()
        .into_iter()
        .filter(|id| storage.get_node(*id).is_ok())
        .collect()
}

fn initial_distribution(seed: NodeId, node_ids: &[NodeId]) -> HashMap<NodeId, f64> {
    let mut distribution = HashMap::with_capacity(node_ids.len());
    for id in node_ids {
        distribution.insert(*id, if *id == seed { 1.0 } else { 0.0 });
    }
    distribution
}

fn normalized_restart_distribution(
    restart_distribution: &HashMap<NodeId, f64>,
    node_ids: &[NodeId],
    storage: &impl StorageAdapter,
) -> HashMap<NodeId, f64> {
    let mut distribution = HashMap::with_capacity(node_ids.len());
    for (node_id, mass) in restart_distribution {
        if storage.get_node(*node_id).is_ok() && mass.is_finite() && *mass > 0.0 {
            add_mass(&mut distribution, *node_id, *mass);
        }
    }
    normalize_distribution(&mut distribution, node_ids, &HashMap::new());
    distribution
}

fn valid_legacy_outgoing_edges(
    source: NodeId,
    storage: &impl StorageAdapter,
) -> Vec<(NodeId, f64)> {
    storage
        .edges_from(source)
        .iter()
        .filter_map(|edge_id| {
            let edge = storage.get_edge(*edge_id).ok()?;
            if storage.get_node(edge.target).is_err()
                || !edge.weight.is_finite()
                || edge.weight <= 0.0
            {
                return None;
            }
            Some((edge.target, edge.weight))
        })
        .collect()
}

fn add_scaled_restart(
    distribution: &mut HashMap<NodeId, f64>,
    restart: &HashMap<NodeId, f64>,
    mass: f64,
) {
    if !mass.is_finite() || mass <= 0.0 {
        return;
    }
    for (node_id, restart_mass) in restart {
        add_mass(distribution, *node_id, mass * *restart_mass);
    }
}

fn add_mass(distribution: &mut HashMap<NodeId, f64>, node_id: NodeId, mass: f64) {
    if mass.is_finite() && mass > 0.0 {
        *distribution.entry(node_id).or_insert(0.0) += mass;
    }
}

fn normalize_distribution(
    distribution: &mut HashMap<NodeId, f64>,
    node_ids: &[NodeId],
    fallback: &HashMap<NodeId, f64>,
) {
    for id in node_ids {
        distribution.entry(*id).or_insert(0.0);
    }

    let sum: f64 = node_ids
        .iter()
        .map(|id| distribution.get(id).copied().unwrap_or(0.0).max(0.0))
        .sum();

    if !sum.is_finite() || sum <= f64::EPSILON {
        distribution.clear();
        for id in node_ids {
            distribution.insert(*id, fallback.get(id).copied().unwrap_or(0.0));
        }
        return;
    }

    for id in node_ids {
        let normalized = distribution.get(id).copied().unwrap_or(0.0).max(0.0) / sum;
        distribution.insert(*id, normalized);
    }
}

fn normalize_legacy_distribution(
    distribution: &mut HashMap<NodeId, f64>,
    node_ids: &[NodeId],
    seed: NodeId,
) {
    normalize_distribution(
        distribution,
        node_ids,
        &initial_distribution(seed, node_ids),
    );
}

fn legacy_no_kappa_rwr(
    seed: NodeId,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
) -> HashMap<NodeId, f64> {
    if storage.get_node(seed).is_err() {
        return HashMap::new();
    }

    let node_ids = live_node_ids(storage);
    if node_ids.is_empty() {
        return HashMap::new();
    }

    let alpha = restart_probability(alpha);
    let mut current = initial_distribution(seed, &node_ids);

    for _ in 0..max_iter {
        let mut next = HashMap::with_capacity(node_ids.len());
        next.insert(seed, alpha);

        for source in &node_ids {
            let mass = current.get(source).copied().unwrap_or(0.0);
            if !mass.is_finite() || mass <= 0.0 {
                continue;
            }

            let walk_mass = (1.0 - alpha) * mass;
            if walk_mass <= 0.0 {
                continue;
            }

            let outgoing = valid_legacy_outgoing_edges(*source, storage);
            let total_weight: f64 = outgoing.iter().map(|(_, weight)| *weight).sum();

            if !total_weight.is_finite() || total_weight <= 0.0 {
                add_mass(&mut next, seed, walk_mass);
                continue;
            }

            for (target, weight) in outgoing {
                add_mass(&mut next, target, walk_mass * weight / total_weight);
            }
        }

        normalize_legacy_distribution(&mut next, &node_ids, seed);
        current = next;
    }

    current.retain(|id, score| storage.get_node(*id).is_ok() && score.is_finite());
    current
}

fn kappa_disabled_distribution_rwr(
    restart_distribution: &HashMap<NodeId, f64>,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
) -> HashMap<NodeId, f64> {
    let node_ids = live_node_ids(storage);
    if node_ids.is_empty() {
        return HashMap::new();
    }

    let restart = normalized_restart_distribution(restart_distribution, &node_ids, storage);
    if !restart
        .values()
        .any(|score| score.is_finite() && *score > 0.0)
    {
        return HashMap::new();
    }

    let alpha = restart_probability(alpha);
    let mut current = restart.clone();

    for _ in 0..max_iter {
        let mut next = HashMap::with_capacity(node_ids.len());
        add_scaled_restart(&mut next, &restart, alpha);

        for source in &node_ids {
            let mass = current.get(source).copied().unwrap_or(0.0);
            if !mass.is_finite() || mass <= 0.0 {
                continue;
            }

            let walk_mass = (1.0 - alpha) * mass;
            if walk_mass <= 0.0 {
                continue;
            }

            let outgoing = valid_legacy_outgoing_edges(*source, storage);
            let total_weight: f64 = outgoing.iter().map(|(_, weight)| *weight).sum();

            if !total_weight.is_finite() || total_weight <= 0.0 {
                add_scaled_restart(&mut next, &restart, walk_mass);
                continue;
            }

            for (target, weight) in outgoing {
                add_mass(&mut next, target, walk_mass * weight / total_weight);
            }
        }

        normalize_distribution(&mut next, &node_ids, &restart);
        current = next;
    }

    current.retain(|id, score| storage.get_node(*id).is_ok() && score.is_finite());
    current
}

#[test]
fn rwr_kappa_disabled_matches_legacy() {
    let mut engine = engine();
    let seed = ingest(&mut engine, "seed");
    let semantic = ingest(&mut engine, "semantic");
    let reason = ingest(&mut engine, "reason");
    let supersedes = ingest(&mut engine, "supersedes");
    let refutes = ingest(&mut engine, "refutes");

    engine
        .link(seed, semantic, EdgeType::Semantic, 1.0)
        .unwrap();
    engine.link(seed, reason, EdgeType::Reason, 2.0).unwrap();
    engine
        .link(seed, supersedes, EdgeType::Supersedes, 3.0)
        .unwrap();
    engine.link(seed, refutes, EdgeType::Refutes, 4.0).unwrap();

    let legacy = legacy_no_kappa_rwr(seed, 0.15, 64, engine.graph().storage());
    let distribution = kappa_disabled_distribution_rwr(
        &HashMap::from([(seed, 1.0)]),
        0.15,
        64,
        engine.graph().storage(),
    );

    for node_id in [seed, semantic, reason, supersedes, refutes] {
        let legacy_score = legacy.get(&node_id).copied().unwrap_or(0.0);
        let distribution_score = distribution.get(&node_id).copied().unwrap_or(0.0);
        assert!(
            (legacy_score - distribution_score).abs() <= 1e-9,
            "node {node_id:?}: legacy {legacy_score}, distribution {distribution_score}"
        );
    }
}
