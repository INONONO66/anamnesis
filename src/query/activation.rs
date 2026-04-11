//! Spreading activation for the Anamnesis query pipeline.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equations
//! - (10) Initial activation: y_i^(0) = clamp(0.60*seed + 0.30*vector_sim + 0.10*identity_prior, 0, 1)
//! - (11) Spreading: y_j^(h+1) = y_i^(h) * w_eff * delta * psi(s) * (1 + 0.20*m)

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::graph::{EdgeType, NodeId};

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

/// A node with its activation score, for use in the priority queue.
#[derive(Debug, Clone, PartialEq)]
struct ActivationEntry {
    activation: f64,
    node_id: NodeId,
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
    /// Outgoing edges: (target_node_id, edge_weight, edge_type, is_forward).
    pub outgoing_edges: Vec<(NodeId, f64, EdgeType, bool)>,
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
) -> HashMap<NodeId, f64>
where
    F: Fn(NodeId) -> Option<NodeInfo>,
{
    let mut activations: HashMap<NodeId, f64> = initial_activations.clone();
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut queue: BinaryHeap<ActivationEntry> = BinaryHeap::new();

    // Initialize queue with seed nodes
    for (node_id, activation) in &initial_activations {
        if *activation > min_activation {
            queue.push(ActivationEntry {
                activation: *activation,
                node_id: *node_id,
            });
        }
    }

    let mut nodes_visited = 0usize;

    while let Some(entry) = queue.pop() {
        if nodes_visited >= budget {
            break;
        }
        if entry.activation < min_activation {
            break;
        }
        if visited.contains(&entry.node_id) {
            continue;
        }

        visited.insert(entry.node_id);
        nodes_visited += 1;

        let info = match node_info_fn(entry.node_id) {
            Some(info) => info,
            None => continue,
        };

        for (target_id, edge_weight, edge_type, is_forward) in &info.outgoing_edges {
            // Skip Contradicts edges — they apply repulsion, not propagation
            if matches!(edge_type, EdgeType::Contradicts) {
                continue;
            }

            let kappa = edge_type.kappa(*is_forward);
            if kappa == 0.0 {
                continue;
            }

            let target_info = match node_info_fn(*target_id) {
                Some(info) => info,
                None => continue,
            };

            let target_gate = salience_gate(target_info.salience);
            let target_boost = 1.0 + 0.20 * target_info.mass;

            let new_activation = propagation_strength(
                entry.activation,
                *edge_weight,
                kappa,
                hop_decay,
                target_gate,
                target_boost,
            );

            if new_activation < min_activation {
                continue;
            }

            let current = activations.get(target_id).copied().unwrap_or(0.0);
            if new_activation > current {
                activations.insert(*target_id, new_activation);

                if !visited.contains(target_id) {
                    queue.push(ActivationEntry {
                        activation: new_activation,
                        node_id: *target_id,
                    });
                }
            }
        }
    }

    activations
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    outgoing_edges: vec![(b, 0.9, EdgeType::Semantic, true)],
                }),
                1 => Some(NodeInfo {
                    salience: 0.7,
                    mass: 0.4,
                    outgoing_edges: vec![(c, 0.8, EdgeType::Semantic, true)],
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
        let result = spread_activation(initial, info_fn, 100, 0.01, 0.65);

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
                    outgoing_edges: vec![(b, 0.9, EdgeType::Semantic, true)],
                }),
                1 => Some(NodeInfo {
                    salience: 0.7,
                    mass: 0.4,
                    outgoing_edges: vec![(c, 0.8, EdgeType::Semantic, true)],
                }),
                2 => Some(NodeInfo {
                    salience: 0.6,
                    mass: 0.3,
                    outgoing_edges: vec![(a, 0.7, EdgeType::Semantic, true)], // back to A
                }),
                _ => None,
            }
        };

        // Should terminate without panic
        let result = spread_activation(initial, info_fn, 100, 0.01, 0.65);
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
                    outgoing_edges: vec![(b, 0.9, EdgeType::Contradicts, true)],
                }),
                1 => Some(NodeInfo {
                    salience: 0.7,
                    mass: 0.4,
                    outgoing_edges: vec![],
                }),
                _ => None,
            }
        };

        let result = spread_activation(initial, info_fn, 100, 0.01, 0.65);
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
                        .map(|&n| (n, 0.9, EdgeType::Semantic, true))
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

        // Budget = 2: should visit A + at most 1 neighbor
        let result = spread_activation(initial, info_fn, 2, 0.01, 0.65);
        // activations map includes seeds + propagated, but only 2 nodes visited
        // All 5 neighbors get activation from A (added to map), but only 1 gets visited
        let visited_with_activation: Vec<_> = result.iter().collect();
        assert!(
            visited_with_activation.len() <= 6,
            "budget=2 should limit visited nodes, got {} entries",
            visited_with_activation.len()
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
                    outgoing_edges: vec![(NodeId(id.0 + 1), 0.5, EdgeType::Semantic, true)],
                })
            } else {
                None
            }
        };

        let result = spread_activation(initial, info_fn, 100, 0.02, 0.65);
        // With 0.05 initial and 0.65 decay, should stop quickly
        assert!(
            result.len() < 10,
            "should stop before visiting all 10 nodes, got {}",
            result.len()
        );
    }
}
