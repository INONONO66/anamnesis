//! Spreading activation algorithm for graph traversal

use std::collections::{HashMap, VecDeque};

use crate::graph::NodeId;

/// Spreading activation for k-hop traversal
pub struct SpreadingActivation;

impl SpreadingActivation {
    /// Execute spreading activation from seed nodes
    /// Returns activated nodes and their activation scores
    pub fn activate(
        seed_nodes: &[NodeId],
        edges: &[(NodeId, NodeId, f64)],
        decay_per_hop: f64,
        min_activation: f64,
        budget: usize,
    ) -> HashMap<NodeId, f64> {
        let mut activations: HashMap<NodeId, f64> = HashMap::new();
        let mut queue: VecDeque<(NodeId, f64)> = VecDeque::new();

        // Initialize with seed nodes
        for &node_id in seed_nodes {
            activations.insert(node_id, 1.0);
            queue.push_back((node_id, 1.0));
        }

        // BFS with activation decay
        while let Some((current, activation)) = queue.pop_front() {
            if activations.len() >= budget {
                break;
            }

            let next_activation = activation * decay_per_hop;
            if next_activation < min_activation {
                continue;
            }

            // Find outgoing edges
            for &(source, target, weight) in edges {
                if source == current {
                    let weighted_activation = next_activation * weight;

                    if weighted_activation >= min_activation {
                        activations
                            .entry(target)
                            .and_modify(|a| *a = a.max(weighted_activation))
                            .or_insert(weighted_activation);

                        if activations.len() < budget {
                            queue.push_back((target, weighted_activation));
                        }
                    }
                }
            }
        }

        activations
    }

    /// Extract k-hop neighborhood
    pub fn k_hop_neighborhood(
        seed: NodeId,
        edges: &[(NodeId, NodeId, f64)],
        k: usize,
    ) -> Vec<NodeId> {
        let mut visited = std::collections::HashSet::new();
        let mut current_level = vec![seed];
        visited.insert(seed);

        for _ in 0..k {
            let mut next_level = Vec::new();

            for node in &current_level {
                for &(source, target, _) in edges {
                    if source == *node && !visited.contains(&target) {
                        visited.insert(target);
                        next_level.push(target);
                    }
                }
            }

            current_level = next_level;
            if current_level.is_empty() {
                break;
            }
        }

        visited.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spreading_activation() {
        let edges = vec![(1, 2, 1.0), (2, 3, 1.0), (3, 4, 1.0)];
        let activations = SpreadingActivation::activate(&[1], &edges, 0.8, 0.01, 100);

        assert!(activations.contains_key(&1));
        assert!(activations.contains_key(&2));
        assert!(activations.contains_key(&3));
    }

    #[test]
    fn test_k_hop_neighborhood() {
        let edges = vec![(1, 2, 1.0), (2, 3, 1.0), (3, 4, 1.0)];
        let neighborhood = SpreadingActivation::k_hop_neighborhood(1, &edges, 2);

        assert!(neighborhood.contains(&1));
        assert!(neighborhood.contains(&2));
        assert!(neighborhood.contains(&3));
    }
}
