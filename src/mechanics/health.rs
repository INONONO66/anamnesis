//! Graph health diagnostics — read-only structural analysis.
//!
//! Computes summary statistics about graph structure, connectivity,
//! and distribution characteristics without mutating any state.

use crate::graph::{EdgeType, NodeId};
use crate::mechanics::topology::{bridge_score, degree, is_orphan};
use crate::storage::StorageAdapter;

/// Default bridge score threshold for counting bridge candidates.
const BRIDGE_THRESHOLD: f64 = 0.5;

/// Diagnostic summary of the cognitive graph's structural health.
///
/// All fields are computed read-only from the current graph state.
/// Useful for monitoring graph growth, detecting fragmentation,
/// and identifying structural anomalies.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphHealth {
    /// Total number of live nodes in the graph.
    pub node_count: usize,
    /// Total number of live edges in the graph.
    pub edge_count: usize,
    /// Number of nodes with zero degree (no incoming or outgoing edges).
    pub orphan_count: usize,
    /// Number of connected components (treating all edges as undirected).
    pub component_count: usize,
    /// Number of edges with type `Contradicts`.
    pub contradiction_count: usize,
    /// Number of edges with type `Supersedes`.
    pub supersede_count: usize,
    /// Shannon entropy of the salience distribution across 4 buckets:
    /// `[0, 0.1)`, `[0.1, 0.4)`, `[0.4, 0.8)`, `[0.8, 1.0]`.
    /// Zero when all nodes fall in a single bucket.
    pub salience_entropy: f64,
    /// Shannon entropy of the knowledge type distribution.
    /// Zero when all nodes share the same type.
    pub type_entropy: f64,
    /// Shannon entropy of the edge type distribution.
    /// Zero when all edges share the same type.
    pub edge_type_entropy: f64,
    /// Number of nodes whose bridge score exceeds the threshold (0.5).
    pub bridge_candidate_count: usize,
}

/// Compute the Shannon entropy H(P) = -Σ p_i * log2(p_i) for a probability distribution.
///
/// Returns 0.0 for empty distributions or single-element distributions.
/// Skips zero-probability entries (0 * log2(0) is defined as 0).
pub fn shannon_entropy(probabilities: &[f64]) -> f64 {
    let mut h = 0.0_f64;
    for &p in probabilities {
        if p > 0.0 {
            h -= p * p.log2();
        }
    }
    if h.is_finite() { h } else { 0.0 }
}

/// Compute `GraphHealth` diagnostics for the given storage.
///
/// This is a pure read-only operation — no graph state is mutated.
pub fn compute_health<S: StorageAdapter>(storage: &S) -> GraphHealth {
    let node_ids = storage.all_node_ids();
    let edge_ids = storage.all_edge_ids();
    let node_count = node_ids.len();
    let edge_count = edge_ids.len();

    // --- Orphan count ---
    let orphan_count = node_ids
        .iter()
        .filter(|&&id| is_orphan(storage, id))
        .count();

    // --- Connected components via union-find ---
    let component_count = count_components(storage, &node_ids);

    // --- Edge type counts ---
    let mut contradiction_count = 0usize;
    let mut supersede_count = 0usize;
    let mut edge_type_counts: std::collections::HashMap<EdgeTypeKey, usize> =
        std::collections::HashMap::new();

    for &eid in &edge_ids {
        if let Ok(edge) = storage.get_edge(eid) {
            match &edge.edge_type {
                EdgeType::Contradicts => contradiction_count += 1,
                EdgeType::Supersedes => supersede_count += 1,
                _ => {}
            }
            *edge_type_counts
                .entry(EdgeTypeKey::from(&edge.edge_type))
                .or_insert(0) += 1;
        }
    }

    // --- Salience entropy (4 buckets) ---
    let salience_entropy = compute_salience_entropy(storage, &node_ids);

    // --- Type entropy ---
    let type_entropy = compute_type_entropy(storage, &node_ids);

    // --- Edge type entropy ---
    let edge_type_entropy = if edge_count > 0 {
        let probs: Vec<f64> = edge_type_counts
            .values()
            .map(|&count| count as f64 / edge_count as f64)
            .collect();
        shannon_entropy(&probs)
    } else {
        0.0
    };

    // --- Bridge candidates ---
    let bridge_candidate_count = count_bridge_candidates(storage, &node_ids);

    GraphHealth {
        node_count,
        edge_count,
        orphan_count,
        component_count,
        contradiction_count,
        supersede_count,
        salience_entropy,
        type_entropy,
        edge_type_entropy,
        bridge_candidate_count,
    }
}

/// Count connected components using union-find (undirected edges).
fn count_components<S: StorageAdapter>(storage: &S, node_ids: &[NodeId]) -> usize {
    if node_ids.is_empty() {
        return 0;
    }

    // Map NodeId -> index for union-find
    let id_to_index: std::collections::HashMap<NodeId, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();

    let n = node_ids.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank: Vec<u8> = vec![0; n];

    // Process all edges as undirected
    let edge_ids = storage.all_edge_ids();
    for &eid in &edge_ids {
        if let Ok(edge) = storage.get_edge(eid) {
            if let (Some(&idx_a), Some(&idx_b)) =
                (id_to_index.get(&edge.source), id_to_index.get(&edge.target))
            {
                union(&mut parent, &mut rank, idx_a, idx_b);
            }
        }
    }

    // Count distinct roots
    let mut roots = std::collections::HashSet::new();
    for i in 0..n {
        roots.insert(find(&mut parent, i));
    }
    roots.len()
}

/// Find with path compression.
fn find(parent: &mut [usize], x: usize) -> usize {
    if parent[x] != x {
        parent[x] = find(parent, parent[x]);
    }
    parent[x]
}

/// Union by rank.
fn union(parent: &mut [usize], rank: &mut [u8], x: usize, y: usize) {
    let rx = find(parent, x);
    let ry = find(parent, y);
    if rx == ry {
        return;
    }
    match rank[rx].cmp(&rank[ry]) {
        std::cmp::Ordering::Less => parent[rx] = ry,
        std::cmp::Ordering::Greater => parent[ry] = rx,
        std::cmp::Ordering::Equal => {
            parent[ry] = rx;
            rank[rx] = rank[rx].saturating_add(1);
        }
    }
}

/// Compute salience entropy using 4 fixed buckets:
/// [0, 0.1), [0.1, 0.4), [0.4, 0.8), [0.8, 1.0]
fn compute_salience_entropy<S: StorageAdapter>(storage: &S, node_ids: &[NodeId]) -> f64 {
    if node_ids.is_empty() {
        return 0.0;
    }

    let mut buckets = [0usize; 4];
    for &id in node_ids {
        let salience = storage.get_salience(id).unwrap_or(0.0);
        let bucket = salience_bucket(salience);
        buckets[bucket] += 1;
    }

    let total = node_ids.len() as f64;
    let probs: Vec<f64> = buckets
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| count as f64 / total)
        .collect();

    shannon_entropy(&probs)
}

/// Map a salience value to its bucket index.
/// Buckets: [0, 0.1) -> 0, [0.1, 0.4) -> 1, [0.4, 0.8) -> 2, [0.8, 1.0] -> 3
fn salience_bucket(salience: f64) -> usize {
    if salience < 0.1 {
        0
    } else if salience < 0.4 {
        1
    } else if salience < 0.8 {
        2
    } else {
        3
    }
}

/// Compute Shannon entropy of the knowledge type distribution.
fn compute_type_entropy<S: StorageAdapter>(storage: &S, node_ids: &[NodeId]) -> f64 {
    if node_ids.is_empty() {
        return 0.0;
    }

    let mut type_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for &id in node_ids {
        if let Ok(kt) = storage.get_node_type(id) {
            let key = format!("{:?}", kt);
            *type_counts.entry(key).or_insert(0) += 1;
        }
    }

    let total = node_ids.len() as f64;
    let probs: Vec<f64> = type_counts.values().map(|&c| c as f64 / total).collect();
    shannon_entropy(&probs)
}

/// Count nodes whose bridge score exceeds the threshold.
fn count_bridge_candidates<S: StorageAdapter>(storage: &S, node_ids: &[NodeId]) -> usize {
    if node_ids.is_empty() {
        return 0;
    }

    // Compute average degree as d_ref
    let total_degree: usize = node_ids.iter().map(|&id| degree(storage, id)).sum();
    let d_ref = if node_ids.is_empty() {
        1
    } else {
        (total_degree / node_ids.len()).max(1)
    };

    node_ids
        .iter()
        .filter(|&&id| {
            bridge_score(storage, id, d_ref)
                .unwrap_or(0.0)
                .gt(&BRIDGE_THRESHOLD)
        })
        .count()
}

/// Key type for edge type counting — needed because EdgeType doesn't impl Hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EdgeTypeKey {
    Semantic,
    Causal,
    Temporal,
    Reason,
    ReinforcedBy,
    ConsolidatedFrom,
    ExtractedFrom,
    Entity,
    Supersedes,
    RejectedAlternative,
    Supports,
    Refutes,
    BelongsTo,
    Contradicts,
    Custom(String),
}

impl From<&EdgeType> for EdgeTypeKey {
    fn from(et: &EdgeType) -> Self {
        match et {
            EdgeType::Semantic => EdgeTypeKey::Semantic,
            EdgeType::Causal => EdgeTypeKey::Causal,
            EdgeType::Temporal => EdgeTypeKey::Temporal,
            EdgeType::Reason => EdgeTypeKey::Reason,
            EdgeType::ReinforcedBy => EdgeTypeKey::ReinforcedBy,
            EdgeType::ConsolidatedFrom => EdgeTypeKey::ConsolidatedFrom,
            EdgeType::ExtractedFrom => EdgeTypeKey::ExtractedFrom,
            EdgeType::Entity => EdgeTypeKey::Entity,
            EdgeType::Supersedes => EdgeTypeKey::Supersedes,
            EdgeType::RejectedAlternative => EdgeTypeKey::RejectedAlternative,
            EdgeType::Supports => EdgeTypeKey::Supports,
            EdgeType::Refutes => EdgeTypeKey::Refutes,
            EdgeType::BelongsTo => EdgeTypeKey::BelongsTo,
            EdgeType::Contradicts => EdgeTypeKey::Contradicts,
            EdgeType::Custom(s) => EdgeTypeKey::Custom(s.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_empty_distribution() {
        assert_eq!(shannon_entropy(&[]), 0.0);
    }

    #[test]
    fn entropy_single_element() {
        assert_eq!(shannon_entropy(&[1.0]), 0.0);
    }

    #[test]
    fn entropy_uniform_two() {
        let h = shannon_entropy(&[0.5, 0.5]);
        assert!((h - 1.0).abs() < 1e-10, "expected 1.0, got {h}");
    }

    #[test]
    fn entropy_uniform_four() {
        let h = shannon_entropy(&[0.25, 0.25, 0.25, 0.25]);
        assert!((h - 2.0).abs() < 1e-10, "expected 2.0, got {h}");
    }

    #[test]
    fn salience_bucket_boundaries() {
        assert_eq!(salience_bucket(0.0), 0);
        assert_eq!(salience_bucket(0.05), 0);
        assert_eq!(salience_bucket(0.099), 0);
        assert_eq!(salience_bucket(0.1), 1);
        assert_eq!(salience_bucket(0.39), 1);
        assert_eq!(salience_bucket(0.4), 2);
        assert_eq!(salience_bucket(0.79), 2);
        assert_eq!(salience_bucket(0.8), 3);
        assert_eq!(salience_bucket(1.0), 3);
    }

    #[test]
    fn union_find_single_component() {
        let mut parent = vec![0, 1, 2, 3, 4];
        let mut rank = vec![0u8; 5];
        union(&mut parent, &mut rank, 0, 1);
        union(&mut parent, &mut rank, 1, 2);
        union(&mut parent, &mut rank, 3, 4);
        union(&mut parent, &mut rank, 2, 3);

        let root = find(&mut parent, 0);
        for i in 1..5 {
            assert_eq!(find(&mut parent, i), root);
        }
    }

    #[test]
    fn union_find_disconnected() {
        let mut parent = vec![0, 1, 2, 3];
        let mut rank = vec![0u8; 4];
        union(&mut parent, &mut rank, 0, 1);
        union(&mut parent, &mut rank, 2, 3);

        assert_eq!(find(&mut parent, 0), find(&mut parent, 1));
        assert_eq!(find(&mut parent, 2), find(&mut parent, 3));
        assert_ne!(find(&mut parent, 0), find(&mut parent, 2));
    }
}
