//! Gravity mechanics: centrality-based importance

use std::collections::HashMap;

/// Gravity scoring for node importance/centrality
pub struct Gravity;

impl Gravity {
    /// Compute PageRank-like centrality scores
    /// Returns a map of node IDs to centrality scores
    pub fn compute_centrality(
        nodes: &[u64],
        edges: &[(u64, u64, f64)],
        iterations: usize,
        damping: f64,
    ) -> HashMap<u64, f64> {
        let mut scores: HashMap<u64, f64> = nodes
            .iter()
            .map(|&id| (id, 1.0 / nodes.len() as f64))
            .collect();

        let mut outgoing: HashMap<u64, Vec<(u64, f64)>> = HashMap::new();
        for &node_id in nodes {
            outgoing.insert(node_id, Vec::new());
        }

        for &(source, target, weight) in edges {
            if let Some(targets) = outgoing.get_mut(&source) {
                targets.push((target, weight));
            }
        }

        for _ in 0..iterations {
            let mut new_scores = HashMap::new();

            for &node_id in nodes {
                let mut score = (1.0 - damping) / nodes.len() as f64;

                // Sum contributions from incoming edges
                for &(source, target, weight) in edges {
                    if target == node_id {
                        if let Some(source_score) = scores.get(&source) {
                            let out_count = outgoing.get(&source).map(|v| v.len()).unwrap_or(1);
                            score += damping * source_score * weight / out_count as f64;
                        }
                    }
                }

                new_scores.insert(node_id, score);
            }

            scores = new_scores;
        }

        scores
    }

    /// Compute in-degree centrality
    pub fn in_degree(node_id: u64, edges: &[(u64, u64, f64)]) -> usize {
        edges
            .iter()
            .filter(|(_, target, _)| *target == node_id)
            .count()
    }

    /// Compute out-degree centrality
    pub fn out_degree(node_id: u64, edges: &[(u64, u64, f64)]) -> usize {
        edges
            .iter()
            .filter(|(source, _, _)| *source == node_id)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_degree() {
        let edges = vec![(1, 2, 1.0), (1, 3, 1.0), (2, 3, 1.0)];
        assert_eq!(Gravity::in_degree(2, &edges), 1);
        assert_eq!(Gravity::in_degree(3, &edges), 2);
    }

    #[test]
    fn test_out_degree() {
        let edges = vec![(1, 2, 1.0), (1, 3, 1.0), (2, 3, 1.0)];
        assert_eq!(Gravity::out_degree(1, &edges), 2);
        assert_eq!(Gravity::out_degree(2, &edges), 1);
    }
}
