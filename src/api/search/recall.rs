//! Graph recall stage — spreading activation from fused seeds.

use std::collections::HashMap;

use crate::api::{EngineConfig, SpreadingModel};
use crate::graph::NodeId;
use crate::mechanics::gravity::compute_mass;
use crate::query::{
    ActivationEdge, FusedCandidate, GraphRecallTrace, NodeInfo, QueryConfig, initial_activation,
};
use crate::query::{
    random_walk_restart_from_distribution_at, spread_activation_with_model_and_convergence,
};
use crate::storage::StorageAdapter;

pub(crate) fn run_graph_recalls<S: StorageAdapter>(
    storage: &S,
    fused_seeds: &[FusedCandidate],
    engine_config: &EngineConfig,
    query_config: &QueryConfig,
    identity_prior: Option<&HashMap<NodeId, f64>>,
) -> (HashMap<NodeId, f64>, GraphRecallTrace) {
    let mut initial_map: HashMap<NodeId, f64> = fused_seeds
        .iter()
        .map(|seed| (seed.node_id, seed.fused_score))
        .collect();

    let now = query_config
        .now
        .unwrap_or_else(crate::graph::Timestamp::now);
    let mut edge_count_skipped_invalid = 0usize;
    let mut convergence_rounds = 0usize;
    let mut converged = false;

    let activated = match engine_config.spreading_model {
        SpreadingModel::PriorityQueueBfs | SpreadingModel::NormalizedPriorityQueueBfs => {
            if let Some(identity_prior) = identity_prior {
                for (&node_id, &prior) in identity_prior {
                    initial_map
                        .entry(node_id)
                        .or_insert_with(|| initial_activation(false, 0.0, prior));
                }
            }

            let node_info_fn = |nid: NodeId| -> Option<NodeInfo> {
                let node = storage.get_node(nid).ok()?;
                let salience = storage.get_salience(nid).unwrap_or(0.0);
                let mass = compute_mass(salience, node.access_count, &node.node_type);

                let mut outgoing_edges: Vec<ActivationEdge> = Vec::new();

                for &edge_id in storage.edges_from(nid) {
                    if let Ok(edge) = storage.get_edge(edge_id) {
                        outgoing_edges.push(ActivationEdge {
                            target_id: edge.target,
                            edge: edge.clone(),
                            is_forward: true,
                        });
                    }
                }

                for &edge_id in storage.edges_to(nid) {
                    if let Ok(edge) = storage.get_edge(edge_id) {
                        outgoing_edges.push(ActivationEdge {
                            target_id: edge.source,
                            edge: edge.clone(),
                            is_forward: false,
                        });
                    }
                }

                Some(NodeInfo {
                    salience,
                    mass,
                    outgoing_edges,
                })
            };

            let result = spread_activation_with_model_and_convergence(
                initial_map,
                node_info_fn,
                query_config.budget,
                query_config.min_activation,
                query_config.decay_per_hop,
                query_config.max_hops,
                now,
                engine_config.spreading_model,
                query_config.convergence.clone(),
            );
            edge_count_skipped_invalid = result.edge_count_skipped_invalid;
            convergence_rounds = result.convergence_rounds;
            converged = result.converged;
            result.activations
        }
        SpreadingModel::RandomWalkRestart => {
            let scores = random_walk_restart_from_distribution_at(
                &initial_map,
                identity_prior,
                super::super::RWR_RESTART_PROBABILITY,
                super::super::RWR_MAX_ITERATIONS,
                storage,
                now,
            );
            limit_activations(
                scores,
                query_config.budget,
                query_config.min_activation,
                storage,
            )
        }
    };

    let trace = GraphRecallTrace {
        invocation_count: 1,
        activated_count: activated.len(),
        model_used: engine_config.spreading_model,
        edge_count_skipped_invalid,
        convergence_rounds,
        converged,
    };

    (activated, trace)
}

fn limit_activations<S: StorageAdapter>(
    activations: HashMap<NodeId, f64>,
    budget: usize,
    min_activation: f64,
    storage: &S,
) -> HashMap<NodeId, f64> {
    let mut ranked: Vec<(NodeId, f64)> = activations
        .into_iter()
        .filter(|(node_id, activation)| {
            activation.is_finite()
                && *activation >= min_activation
                && storage.get_node(*node_id).is_ok()
        })
        .collect();

    ranked.sort_by(|(left_id, left_score), (right_id, right_score)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left_id.cmp(right_id))
    });
    ranked.truncate(budget);
    ranked.into_iter().collect()
}
