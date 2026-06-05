//! Random walk with restart over the storage-backed graph.
//!
//! The implementation is matrix-free: each iteration pushes probability mass
//! through graph adjacency, normalizing each source row by the sum
//! of valid typed transition weights.

use std::collections::HashMap;

use crate::graph::{EdgeType, NodeId, Timestamp};
use crate::query::activation::edge_valid_at;
use crate::storage::StorageAdapter;

const DEFAULT_RESTART_PROBABILITY: f64 = crate::mechanics::priors::RWR_RESTART_PROBABILITY;
const CONVERGENCE_EPSILON: f64 = 1e-12;
const IDENTITY_RESTART_WEIGHT: f64 = 0.10;

/// Computes random-walk-with-restart affinities from a single seed node.
///
/// Uses the recurrence `r(t+1) = (1 - alpha) * W * r(t) + alpha * e_seed`,
/// where `W` is represented by typed adjacency lists and row-normalized edge
/// weights. Dangling nodes return their walk mass to the restart vector,
/// preserving total probability without requiring a dense transition matrix.
///
/// Missing or deleted nodes/edges are skipped gracefully. If the seed is not a
/// live node, the result is empty.
pub fn random_walk_restart(
    seed: NodeId,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
) -> HashMap<NodeId, f64> {
    random_walk_restart_at(seed, alpha, max_iter, storage, Timestamp::now())
}

/// Computes random-walk-with-restart affinities at a domain timestamp.
pub fn random_walk_restart_at(
    seed: NodeId,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
    now: Timestamp,
) -> HashMap<NodeId, f64> {
    if storage.get_node(seed).is_err() {
        return HashMap::new();
    }

    random_walk_restart_from_distribution_at(
        &HashMap::from([(seed, 1.0)]),
        None,
        alpha,
        max_iter,
        storage,
        now,
    )
}

/// Computes random-walk-with-restart affinities from a restart distribution.
///
/// The restart distribution is also used as the initial distribution. When an
/// identity prior is provided, each prior score contributes the same `0.10`
/// weight used by the associative BFS initial-activation equation, then the
/// restart vector is normalized.
///
/// Transition rows are built from both outgoing and incoming edges. Supportive
/// edges are weighted by `EdgeType::kappa()`, `Contradicts` edges are excluded,
/// and `Supersedes` uses the edge direction (`source→target` = forward).
pub fn random_walk_restart_from_distribution(
    restart_distribution: &HashMap<NodeId, f64>,
    identity_prior: Option<&HashMap<NodeId, f64>>,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
) -> HashMap<NodeId, f64> {
    random_walk_restart_from_distribution_at(
        restart_distribution,
        identity_prior,
        alpha,
        max_iter,
        storage,
        Timestamp::now(),
    )
}

/// Computes random-walk-with-restart affinities at a domain timestamp.
pub fn random_walk_restart_from_distribution_at(
    restart_distribution: &HashMap<NodeId, f64>,
    identity_prior: Option<&HashMap<NodeId, f64>>,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
    now: Timestamp,
) -> HashMap<NodeId, f64> {
    random_walk_restart_from_distribution_with_kappa(
        restart_distribution,
        identity_prior,
        alpha,
        max_iter,
        storage,
        true,
        now,
    )
}

fn random_walk_restart_from_distribution_with_kappa(
    restart_distribution: &HashMap<NodeId, f64>,
    identity_prior: Option<&HashMap<NodeId, f64>>,
    alpha: f64,
    max_iter: usize,
    storage: &impl StorageAdapter,
    apply_kappa: bool,
    now: Timestamp,
) -> HashMap<NodeId, f64> {
    let node_ids: Vec<NodeId> = storage
        .all_node_ids()
        .into_iter()
        .filter(|id| storage.get_node(*id).is_ok())
        .collect();

    if node_ids.is_empty() {
        return HashMap::new();
    }

    let restart =
        normalized_restart_distribution(restart_distribution, identity_prior, &node_ids, storage);

    if restart.is_empty() {
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

            let transitions = valid_transition_edges(*source, storage, apply_kappa, now);
            let total_weight: f64 = transitions.iter().map(|(_, weight)| *weight).sum();

            if !total_weight.is_finite() || total_weight <= 0.0 {
                add_scaled_restart(&mut next, &restart, walk_mass);
                continue;
            }

            for (target, weight) in transitions {
                add_mass(&mut next, target, walk_mass * weight / total_weight);
            }
        }

        normalize_distribution(&mut next, &node_ids, &restart);
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

fn normalized_restart_distribution(
    restart_distribution: &HashMap<NodeId, f64>,
    identity_prior: Option<&HashMap<NodeId, f64>>,
    node_ids: &[NodeId],
    storage: &impl StorageAdapter,
) -> HashMap<NodeId, f64> {
    let mut distribution = HashMap::with_capacity(node_ids.len());

    for (node_id, mass) in restart_distribution {
        if storage.get_node(*node_id).is_err() || !mass.is_finite() || *mass <= 0.0 {
            continue;
        }
        add_mass(&mut distribution, *node_id, *mass);
    }

    if let Some(prior) = identity_prior {
        for (node_id, mass) in prior {
            if storage.get_node(*node_id).is_err() || !mass.is_finite() || *mass <= 0.0 {
                continue;
            }
            add_mass(&mut distribution, *node_id, IDENTITY_RESTART_WEIGHT * *mass);
        }
    }

    normalize_distribution(&mut distribution, node_ids, &HashMap::new());

    if distribution
        .values()
        .any(|score| score.is_finite() && *score > 0.0)
    {
        distribution
    } else {
        HashMap::new()
    }
}

fn valid_transition_edges(
    source: NodeId,
    storage: &impl StorageAdapter,
    apply_kappa: bool,
    now: Timestamp,
) -> Vec<(NodeId, f64)> {
    let mut transitions = Vec::new();

    transitions.extend(storage.edges_from(source).iter().filter_map(|edge_id| {
        let edge = storage.get_edge(*edge_id).ok()?;
        if !edge_valid_at(edge, now) {
            return None;
        }
        weighted_transition(
            edge.target,
            edge.weight,
            &edge.edge_type,
            true,
            storage,
            apply_kappa,
        )
    }));

    transitions.extend(storage.edges_to(source).iter().filter_map(|edge_id| {
        let edge = storage.get_edge(*edge_id).ok()?;
        if !edge_valid_at(edge, now) {
            return None;
        }
        weighted_transition(
            edge.source,
            edge.weight,
            &edge.edge_type,
            false,
            storage,
            apply_kappa,
        )
    }));

    transitions
}

fn weighted_transition(
    target: NodeId,
    weight: f64,
    edge_type: &EdgeType,
    is_forward: bool,
    storage: &impl StorageAdapter,
    apply_kappa: bool,
) -> Option<(NodeId, f64)> {
    if matches!(edge_type, EdgeType::Contradicts)
        || storage.get_node(target).is_err()
        || !weight.is_finite()
        || weight <= 0.0
    {
        return None;
    }

    let kappa = if apply_kappa {
        edge_type.kappa(is_forward)
    } else {
        1.0
    };
    if !kappa.is_finite() || kappa <= 0.0 {
        return None;
    }

    let weighted = weight * kappa;
    if weighted.is_finite() && weighted > 0.0 {
        Some((target, weighted))
    } else {
        None
    }
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
