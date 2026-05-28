//! Spreading activation for the Anamnesis query pipeline.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equations
//! - (10) Initial activation: y_i^(0) = clamp(0.60*seed + 0.30*vector_sim + 0.10*identity_prior, 0, 1)
//! - (11) Spreading: y_j^(h+1) = y_i^(h) * w_eff * delta * psi(s) * (1 + 0.20*m)

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::api::SpreadingModel;
use crate::graph::{Edge, EdgeType, NodeId, Timestamp};

/// Computes the initial activation for a node.
///
/// Equation (10): y_i^(0) = clamp(0.60*seed + 0.30*vector_sim + 0.10*identity_prior, 0, 1)
///
/// - `is_seed`: true if this is the seed node
/// - `vector_sim`: cosine similarity between query embedding and node embedding [0, 1]
/// - `identity_prior`: identity prior from equation (9) [0, 1]
pub fn initial_activation(is_seed: bool, vector_sim: f64, identity_prior: f64) -> f64 {
    let seed_component = if is_seed { 0.60 } else { 0.0 };
    (seed_component + 0.30 * vector_sim + 0.10 * identity_prior).clamp(0.0, 1.0)
}

/// Computes the salience gate value.
///
/// Low-salience nodes still receive some activation (floor = 0.2).
/// psi(s) = 0.2 + 0.8 * s
pub fn salience_gate(salience: f64) -> f64 {
    0.2 + 0.8 * salience
}

/// Computes the propagation strength for one hop.
///
/// Equation (11): y_j = y_i * w_eff * delta * psi(s_j) * (1 + 0.20 * m_j)
///
/// - `source_activation`: activation of the source node
/// - `edge_weight`: weight of the edge [0, 1]
/// - `kappa`: edge type propagation multiplier
/// - `hop_decay`: decay per hop (0.65)
/// - `target_salience_gate`: psi(s_j) for the target node
/// - `target_gravity_boost`: 1 + 0.20 * m_j for the target node
pub fn propagation_strength(
    source_activation: f64,
    edge_weight: f64,
    kappa: f64,
    hop_decay: f64,
    target_salience_gate: f64,
    target_gravity_boost: f64,
) -> f64 {
    (source_activation
        * edge_weight
        * kappa
        * hop_decay
        * target_salience_gate
        * target_gravity_boost)
        .clamp(0.0, 1.0)
}

/// Computes the fan-out normalization factor for a source node.
///
/// F(i) = 1 / sqrt(max(1, valid_fan_out(i)))
pub fn fan_out_normalization_factor(valid_fan_out: usize) -> f64 {
    1.0 / (valid_fan_out.max(1) as f64).sqrt()
}

/// Returns whether an edge is valid at a domain timestamp.
///
/// Edges without validity bounds are always valid for backward compatibility.
pub fn edge_valid_at(edge: &Edge, as_of: Timestamp) -> bool {
    let from_ok = edge.valid_from.is_none_or(|valid_from| as_of >= valid_from);
    let until_ok = edge
        .valid_until
        .is_none_or(|valid_until| as_of <= valid_until);
    from_ok && until_ok
}

#[derive(Debug, Clone, PartialEq)]
struct ActivationEntry {
    activation: f64,
    node_id: NodeId,
    depth: usize,
}

impl Eq for ActivationEntry {}

impl PartialOrd for ActivationEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ActivationEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher activation = higher priority
        self.activation
            .partial_cmp(&other.activation)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.node_id.0.cmp(&other.node_id.0))
    }
}

/// Input for a single node during spreading activation.
pub struct NodeInfo {
    /// Current salience [0, 1].
    pub salience: f64,
    /// Node mass (from gravity computation) [0, 1].
    pub mass: f64,
    /// Edges traversable from this node.
    pub outgoing_edges: Vec<ActivationEdge>,
}

/// Edge input for spreading activation.
pub struct ActivationEdge {
    /// Node reached by traversing this edge from the current node.
    pub target_id: NodeId,
    /// Full graph edge, including validity bounds.
    pub edge: Edge,
    /// Whether traversal follows the edge source→target direction.
    pub is_forward: bool,
}

/// Result of spreading activation with trace counters.
pub struct SpreadingActivationResult {
    /// Final activation score by visited node.
    pub activations: HashMap<NodeId, f64>,
    /// Number of invalid temporal edges skipped during traversal.
    pub edge_count_skipped_invalid: usize,
    /// Number of convergence check rounds performed.
    pub convergence_rounds: usize,
    /// Whether spreading activation converged early.
    pub converged: bool,
}

/// Runs spreading activation from a set of initially activated nodes.
///
/// Uses a priority queue (highest activation first) with a visited set for cycle safety.
///
/// # Parameters
/// - `initial_activations`: map of NodeId → initial activation score
/// - `node_info_fn`: function to get `NodeInfo` for a node
/// - `budget`: maximum number of nodes to visit
/// - `min_activation`: stop spreading when activation falls below this
/// - `hop_decay`: activation decay per hop (0.65)
///
/// # Returns
/// Map of NodeId → final activation score for all visited nodes.
pub fn spread_activation<F>(
    initial_activations: HashMap<NodeId, f64>,
    node_info_fn: F,
    budget: usize,
    min_activation: f64,
    hop_decay: f64,
    max_hops: usize,
) -> HashMap<NodeId, f64>
where
    F: Fn(NodeId) -> Option<NodeInfo>,
{
    spread_activation_with_convergence(
        initial_activations,
        node_info_fn,
        budget,
        min_activation,
        hop_decay,
        max_hops,
        Timestamp::now(),
        None,
    )
    .activations
}

/// Runs spreading activation at a given domain timestamp and returns trace counters.
pub fn spread_activation_at<F>(
    initial_activations: HashMap<NodeId, f64>,
    node_info_fn: F,
    budget: usize,
    min_activation: f64,
    hop_decay: f64,
    max_hops: usize,
    now: Timestamp,
) -> SpreadingActivationResult
where
    F: Fn(NodeId) -> Option<NodeInfo>,
{
    spread_activation_with_convergence(
        initial_activations,
        node_info_fn,
        budget,
        min_activation,
        hop_decay,
        max_hops,
        now,
        None,
    )
}

fn valid_fan_out(info: &NodeInfo, now: Timestamp) -> usize {
    info.outgoing_edges
        .iter()
        .filter(|activation_edge| edge_valid_at(&activation_edge.edge, now))
        .count()
}

/// Runs spreading activation with optional convergence termination.
#[allow(clippy::too_many_arguments)]
pub fn spread_activation_with_convergence<F>(
    initial_activations: HashMap<NodeId, f64>,
    node_info_fn: F,
    budget: usize,
    min_activation: f64,
    hop_decay: f64,
    max_hops: usize,
    now: Timestamp,
    convergence_config: Option<crate::query::types::ConvergenceConfig>,
) -> SpreadingActivationResult
where
    F: Fn(NodeId) -> Option<NodeInfo>,
{
    spread_activation_with_model_and_convergence(
        initial_activations,
        node_info_fn,
        budget,
        min_activation,
        hop_decay,
        max_hops,
        now,
        SpreadingModel::PriorityQueueBfs,
        convergence_config,
    )
}

/// Runs spreading activation with a selected spreading model and optional convergence termination.
#[allow(clippy::too_many_arguments)]
pub fn spread_activation_with_model_and_convergence<F>(
    initial_activations: HashMap<NodeId, f64>,
    node_info_fn: F,
    budget: usize,
    min_activation: f64,
    hop_decay: f64,
    max_hops: usize,
    now: Timestamp,
    spreading_model: SpreadingModel,
    convergence_config: Option<crate::query::types::ConvergenceConfig>,
) -> SpreadingActivationResult
where
    F: Fn(NodeId) -> Option<NodeInfo>,
{
    let mut activations: HashMap<NodeId, f64> = initial_activations.clone();
    let mut best_depth: HashMap<NodeId, usize> = HashMap::new();
    let mut queue: BinaryHeap<ActivationEntry> = BinaryHeap::new();

    for (node_id, activation) in &initial_activations {
        if *activation > min_activation {
            queue.push(ActivationEntry {
                activation: *activation,
                node_id: *node_id,
                depth: 0,
            });
        }
    }

    let mut nodes_visited = 0usize;
    let mut edge_count_skipped_invalid = 0usize;
    let mut convergence_rounds = 0usize;
    let mut converged = false;
    let mut stable_count = 0usize;
    let mut prev_top_k: Vec<NodeId> = Vec::new();

    while let Some(entry) = queue.pop() {
        if nodes_visited >= budget {
            break;
        }
        if entry.activation < min_activation {
            break;
        }
        if let Some(&prev_depth) = best_depth.get(&entry.node_id) {
            if entry.depth >= prev_depth {
                continue;
            }
        }

        best_depth.insert(entry.node_id, entry.depth);
        nodes_visited += 1;

        let info = match node_info_fn(entry.node_id) {
            Some(info) => info,
            None => continue,
        };

        if entry.depth < max_hops {
            let source_fan_out_normalization = match spreading_model {
                SpreadingModel::NormalizedPriorityQueueBfs => {
                    fan_out_normalization_factor(valid_fan_out(&info, now))
                }
                SpreadingModel::PriorityQueueBfs | SpreadingModel::RandomWalkRestart => 1.0,
            };

            for activation_edge in &info.outgoing_edges {
                if matches!(activation_edge.edge.edge_type, EdgeType::Contradicts) {
                    continue;
                }

                if !edge_valid_at(&activation_edge.edge, now) {
                    edge_count_skipped_invalid += 1;
                    continue;
                }

                let kappa = activation_edge
                    .edge
                    .edge_type
                    .kappa(activation_edge.is_forward);
                if kappa == 0.0 {
                    continue;
                }

                let target_info = match node_info_fn(activation_edge.target_id) {
                    Some(info) => info,
                    None => continue,
                };

                let target_gate = salience_gate(target_info.salience);
                let target_boost = 1.0 + 0.20 * target_info.mass;

                let new_activation = (propagation_strength(
                    entry.activation,
                    activation_edge.edge.weight,
                    kappa,
                    hop_decay,
                    target_gate,
                    target_boost,
                ) * source_fan_out_normalization)
                    .clamp(0.0, 1.0);

                if new_activation < min_activation {
                    continue;
                }

                let current = activations
                    .get(&activation_edge.target_id)
                    .copied()
                    .unwrap_or(0.0);
                if new_activation > current {
                    activations.insert(activation_edge.target_id, new_activation);

                    if !best_depth.contains_key(&activation_edge.target_id) {
                        queue.push(ActivationEntry {
                            activation: new_activation,
                            node_id: activation_edge.target_id,
                            depth: entry.depth + 1,
                        });
                    }
                }
            }
        }

        if let Some(ref config) = convergence_config {
            if nodes_visited % 10 == 0 {
                convergence_rounds += 1;
                let mut current_top_k: Vec<_> =
                    activations.iter().map(|(id, &act)| (*id, act)).collect();
                current_top_k.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
                current_top_k.truncate(config.compare_top_k);
                let current_top_k_ids: Vec<NodeId> =
                    current_top_k.iter().map(|(id, _)| *id).collect();

                if current_top_k_ids == prev_top_k {
                    stable_count += 1;
                    if stable_count >= config.stable_rounds {
                        converged = true;
                        break;
                    }
                } else {
                    stable_count = 0;
                    prev_top_k = current_top_k_ids;
                }
            }
        }
    }

    activations.retain(|id, _| best_depth.contains_key(id));
    SpreadingActivationResult {
        activations,
        edge_count_skipped_invalid,
        convergence_rounds,
        converged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EdgeId;

    // ── Initial activation ───────────────────────────────────────────────────

    #[test]
    fn seed_node_gets_high_activation() {
        let act = initial_activation(true, 0.0, 0.0);
        assert!((act - 0.60).abs() < 1e-10);
    }

    #[test]
    fn non_seed_with_vector_sim() {
        let act = initial_activation(false, 1.0, 0.0);
        assert!((act - 0.30).abs() < 1e-10);
    }

    #[test]
    fn full_activation_clamped_to_one() {
        let act = initial_activation(true, 1.0, 1.0);
        assert!((act - 1.0).abs() < 1e-10);
    }

    #[test]
    fn zero_activation() {
        let act = initial_activation(false, 0.0, 0.0);
        assert_eq!(act, 0.0);
    }

    // ── Salience gate ────────────────────────────────────────────────────────

    #[test]
    fn salience_gate_at_zero() {
        assert!((salience_gate(0.0) - 0.2).abs() < 1e-10);
    }

    #[test]
    fn salience_gate_at_one() {
        assert!((salience_gate(1.0) - 1.0).abs() < 1e-10);
    }

    // ── Spreading activation ─────────────────────────────────────────────────

    fn activation_edge(target_id: NodeId, weight: f64, edge_type: EdgeType) -> ActivationEdge {
        ActivationEdge {
            target_id,
            edge: Edge {
                id: EdgeId(target_id.0),
                source: NodeId(0),
                target: target_id,
                edge_type,
                weight,
                edge_source: crate::graph::edge::EdgeSource::Auto,
                created_at: Timestamp(0),
                valid_from: None,
                valid_until: None,
                metadata: HashMap::new(),
            },
            is_forward: true,
        }
    }

    fn make_linear_chain() -> (HashMap<NodeId, f64>, impl Fn(NodeId) -> Option<NodeInfo>) {
        // A → B → C (linear chain)
        let a = NodeId(0);
        let b = NodeId(1);
        let c = NodeId(2);

        let mut initial = HashMap::new();
        initial.insert(a, 0.8);

        let info_fn = move |id: NodeId| -> Option<NodeInfo> {
            match id.0 {
                0 => Some(NodeInfo {
                    salience: 0.8,
                    mass: 0.5,
                    outgoing_edges: vec![activation_edge(b, 0.9, EdgeType::Semantic)],
                }),
                1 => Some(NodeInfo {
                    salience: 0.7,
                    mass: 0.4,
                    outgoing_edges: vec![activation_edge(c, 0.8, EdgeType::Semantic)],
                }),
                2 => Some(NodeInfo {
                    salience: 0.6,
                    mass: 0.3,
                    outgoing_edges: vec![],
                }),
                _ => None,
            }
        };

        (initial, info_fn)
    }

    #[test]
    fn linear_chain_activation_decays() {
        let (initial, info_fn) = make_linear_chain();
        let result = spread_activation(initial, info_fn, 100, 0.01, 0.65, 10);

        let a_act = result[&NodeId(0)];
        let b_act = result[&NodeId(1)];
        let c_act = result[&NodeId(2)];

        assert!(
            a_act > b_act,
            "A ({a_act}) should have higher activation than B ({b_act})"
        );
        assert!(
            b_act > c_act,
            "B ({b_act}) should have higher activation than C ({c_act})"
        );
    }

    #[test]
    fn cyclic_graph_terminates() {
        // A → B → C → A (cycle)
        let a = NodeId(0);
        let b = NodeId(1);
        let c = NodeId(2);

        let mut initial = HashMap::new();
        initial.insert(a, 0.8);

        let info_fn = move |id: NodeId| -> Option<NodeInfo> {
            match id.0 {
                0 => Some(NodeInfo {
                    salience: 0.8,
                    mass: 0.5,
                    outgoing_edges: vec![activation_edge(b, 0.9, EdgeType::Semantic)],
                }),
                1 => Some(NodeInfo {
                    salience: 0.7,
                    mass: 0.4,
                    outgoing_edges: vec![activation_edge(c, 0.8, EdgeType::Semantic)],
                }),
                2 => Some(NodeInfo {
                    salience: 0.6,
                    mass: 0.3,
                    outgoing_edges: vec![activation_edge(a, 0.7, EdgeType::Semantic)], // back to A
                }),
                _ => None,
            }
        };

        // Should terminate without panic
        let result = spread_activation(initial, info_fn, 100, 0.01, 0.65, 10);
        assert!(result.len() <= 3, "should visit at most 3 nodes in cycle");
    }

    #[test]
    fn contradicts_edge_not_traversed() {
        let a = NodeId(0);
        let b = NodeId(1);

        let mut initial = HashMap::new();
        initial.insert(a, 0.8);

        let info_fn = move |id: NodeId| -> Option<NodeInfo> {
            match id.0 {
                0 => Some(NodeInfo {
                    salience: 0.8,
                    mass: 0.5,
                    outgoing_edges: vec![activation_edge(b, 0.9, EdgeType::Contradicts)],
                }),
                1 => Some(NodeInfo {
                    salience: 0.7,
                    mass: 0.4,
                    outgoing_edges: vec![],
                }),
                _ => None,
            }
        };

        let result = spread_activation(initial, info_fn, 100, 0.01, 0.65, 10);
        // B should NOT be activated (Contradicts edge skipped)
        assert!(
            !result.contains_key(&b) || result[&b] == 0.0,
            "B should not be activated via Contradicts edge"
        );
    }

    #[test]
    fn budget_limits_traversal() {
        // Create a star graph: A → B, C, D, E, F (5 neighbors)
        let a = NodeId(0);
        let neighbors: Vec<NodeId> = (1..=5).map(NodeId).collect();

        let mut initial = HashMap::new();
        initial.insert(a, 0.8);

        let info_fn = move |id: NodeId| -> Option<NodeInfo> {
            if id == a {
                Some(NodeInfo {
                    salience: 0.8,
                    mass: 0.5,
                    outgoing_edges: neighbors
                        .iter()
                        .map(|&n| activation_edge(n, 0.9, EdgeType::Semantic))
                        .collect(),
                })
            } else if id.0 >= 1 && id.0 <= 5 {
                Some(NodeInfo {
                    salience: 0.7,
                    mass: 0.4,
                    outgoing_edges: vec![],
                })
            } else {
                None
            }
        };

        let result = spread_activation(initial, info_fn, 2, 0.01, 0.65, 10);
        assert!(
            result.len() <= 2,
            "budget=2 should visit at most 2 nodes, got {} entries",
            result.len()
        );
    }

    #[test]
    fn only_visited_nodes_returned() {
        let (initial, info_fn) = make_linear_chain();
        let result = spread_activation(initial, info_fn, 1, 0.01, 0.65, 10);

        assert!(
            result.contains_key(&NodeId(0)),
            "seed (visited) should be in results"
        );
        assert!(
            !result.contains_key(&NodeId(1)),
            "unvisited neighbor should not be in results"
        );
    }

    #[test]
    fn max_hops_limits_depth() {
        let (initial, info_fn) = make_linear_chain();
        let result = spread_activation(initial, info_fn, 100, 0.001, 0.65, 1);

        assert!(
            result.contains_key(&NodeId(0)),
            "seed at depth 0 should be visited"
        );
        assert!(
            result.contains_key(&NodeId(1)),
            "node at depth 1 should be visited"
        );
        assert!(
            !result.contains_key(&NodeId(2)),
            "node at depth 2 should NOT be visited with max_hops=1"
        );
    }

    #[test]
    fn activation_threshold_stops_spreading() {
        // Long chain where activation decays below threshold
        let mut initial = HashMap::new();
        initial.insert(NodeId(0), 0.05); // Very low initial activation

        let info_fn = |id: NodeId| -> Option<NodeInfo> {
            if id.0 < 10 {
                Some(NodeInfo {
                    salience: 0.5,
                    mass: 0.3,
                    outgoing_edges: vec![activation_edge(
                        NodeId(id.0 + 1),
                        0.5,
                        EdgeType::Semantic,
                    )],
                })
            } else {
                None
            }
        };

        let result = spread_activation(initial, info_fn, 100, 0.02, 0.65, 10);
        // With 0.05 initial and 0.65 decay, should stop quickly
        assert!(
            result.len() < 10,
            "should stop before visiting all 10 nodes, got {}",
            result.len()
        );
    }
}
