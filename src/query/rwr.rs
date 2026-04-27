//! Random walk with restart over the storage-backed graph.
//!
//! The implementation is matrix-free: each iteration pushes probability mass
//! through `StorageAdapter::edges_from`, normalizing each source row by the sum
//! of valid outgoing edge weights.

use std::collections::HashMap;

use crate::graph::NodeId;
use crate::storage::StorageAdapter;

const DEFAULT_RESTART_PROBABILITY: f64 = 0.15;
const CONVERGENCE_EPSILON: f64 = 1e-12;

/// Computes random-walk-with-restart affinities from a single seed node.
///
/// Uses the recurrence `r(t+1) = (1 - alpha) * W * r(t) + alpha * e_seed`,
/// where `W` is represented by outgoing adjacency lists and row-normalized edge
/// weights. Dangling nodes return their walk mass to the seed, preserving total
/// probability without requiring a dense transition matrix.
///
/// Missing or deleted nodes/edges are skipped gracefully. If the seed is not a
/// live node, the result is empty.
pub fn random_walk_restart(
    seed: NodeId,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
) -> HashMap<NodeId, f64> {
    if storage.get_node(seed).is_err() {
        return HashMap::new();
    }

    let node_ids: Vec<NodeId> = storage
        .all_node_ids()
        .into_iter()
        .filter(|id| storage.get_node(*id).is_ok())
        .collect();

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

            let outgoing = valid_outgoing_edges(*source, storage);
            let total_weight: f64 = outgoing.iter().map(|(_, weight)| *weight).sum();

            if !total_weight.is_finite() || total_weight <= 0.0 {
                add_mass(&mut next, seed, walk_mass);
                continue;
            }

            for (target, weight) in outgoing {
                add_mass(&mut next, target, walk_mass * weight / total_weight);
            }
        }

        normalize_distribution(&mut next, &node_ids, seed);
        let delta = l1_delta(&current, &next, &node_ids);
        current = next;

        if delta < CONVERGENCE_EPSILON {
            break;
        }
    }

    current.retain(|id, score| storage.get_node(*id).is_ok() && score.is_finite());
    current
}

fn restart_probability(alpha: f64) -> f64 {
    if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        DEFAULT_RESTART_PROBABILITY
    }
}

fn initial_distribution(seed: NodeId, node_ids: &[NodeId]) -> HashMap<NodeId, f64> {
    let mut distribution = HashMap::with_capacity(node_ids.len());
    for id in node_ids {
        distribution.insert(*id, if *id == seed { 1.0 } else { 0.0 });
    }
    distribution
}

fn valid_outgoing_edges(source: NodeId, storage: &impl StorageAdapter) -> Vec<(NodeId, f64)> {
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

fn add_mass(distribution: &mut HashMap<NodeId, f64>, node_id: NodeId, mass: f64) {
    if mass.is_finite() && mass > 0.0 {
        *distribution.entry(node_id).or_insert(0.0) += mass;
    }
}

fn normalize_distribution(
    distribution: &mut HashMap<NodeId, f64>,
    node_ids: &[NodeId],
    seed: NodeId,
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
            distribution.insert(*id, if *id == seed { 1.0 } else { 0.0 });
        }
        return;
    }

    for id in node_ids {
        let normalized = distribution.get(id).copied().unwrap_or(0.0).max(0.0) / sum;
        distribution.insert(*id, normalized);
    }
}

fn l1_delta(
    current: &HashMap<NodeId, f64>,
    next: &HashMap<NodeId, f64>,
    node_ids: &[NodeId],
) -> f64 {
    node_ids
        .iter()
        .map(|id| {
            let a = current.get(id).copied().unwrap_or(0.0);
            let b = next.get(id).copied().unwrap_or(0.0);
            (a - b).abs()
        })
        .sum()
}
