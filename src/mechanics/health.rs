//! Graph health diagnostics — read-only structural analysis.
//!
//! Computes the nine `GraphHealth` metrics named in
//! [observability.md](../../docs/07-quality-gates/observability.md) without
//! mutating any state. Every metric is derived from the authoritative reservoirs
//! and their projections; nothing here writes salience/weight (the standing
//! cross-phase invariant, ADR-0002).

use crate::graph::{EdgeType, Timestamp};
use crate::mechanics::topology::degree;
use crate::storage::StorageAdapter;

/// A site is "stale" when it has not been accessed within this many days.
///
/// CALIBRATED PRIOR — the window past which an unread site counts toward
/// `stale_ratio`. Declared here (not in `priors.rs`) because it is an
/// observability reporting threshold, not a dynamics constant.
const STALE_WINDOW_DAYS: f64 = 30.0;

/// Milliseconds per day, for the elapsed-time computation in `stale_ratio`.
const DAY_MS: f64 = 86_400_000.0;

/// Diagnostic summary of the cognitive graph's structural health.
///
/// The nine metrics are exactly those named in
/// [observability.md](../../docs/07-quality-gates/observability.md). All fields
/// are computed read-only from the current graph state — useful for monitoring
/// growth, detecting fragmentation, and identifying structural anomalies. They
/// are derived views over the reservoirs; computing them never mutates state.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphHealth {
    /// Total number of live sites (nodes).
    pub node_count: usize,
    /// Total number of live edges.
    pub edge_count: usize,
    /// Fraction of sites with zero degree (no incoming or outgoing edges), `[0, 1]`.
    pub orphan_ratio: f64,
    /// Fraction of edges whose type is `Contradicts` (tension edges), `[0, 1]`.
    pub contradiction_ratio: f64,
    /// Shannon entropy (bits) of the salience-projection distribution across 4
    /// buckets `[0, 0.1)`, `[0.1, 0.4)`, `[0.4, 0.8)`, `[0.8, 1.0]`. Zero when
    /// all sites fall in a single bucket. Diversity of salience projections.
    pub salience_entropy: f64,
    /// Shannon entropy (bits) of the conductance-projection (`project_weight(C)`)
    /// distribution across the same 4 buckets. Zero when all edges fall in a
    /// single bucket or there are no edges. Diversity of conductance projections.
    pub conductance_entropy: f64,
    /// Mean graph degree `2 * edge_count / node_count` (each edge contributes to
    /// the degree of both endpoints). Zero for an empty graph.
    pub average_degree: f64,
    /// Site count by origin scope (`scope.as_str()` → count). Universal scope is
    /// keyed as `"universal"`. The distribution of sites across scopes.
    pub scope_distribution: std::collections::BTreeMap<String, usize>,
    /// Fraction of sites not accessed within the stale window, `[0, 1]`.
    pub stale_ratio: f64,
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

/// Compute the nine-metric `GraphHealth` diagnostics for the given storage.
///
/// This is a pure read-only operation — no graph state is mutated. The `now`
/// timestamp is the reference for the `stale_ratio` window.
pub fn compute_health<S: StorageAdapter>(storage: &S, now: Timestamp) -> GraphHealth {
    let node_ids = storage.all_node_ids();
    let edge_ids = storage.all_edge_ids();
    let node_count = node_ids.len();
    let edge_count = edge_ids.len();

    // --- Orphan ratio ---
    let orphan_count = node_ids
        .iter()
        .filter(|&&id| degree(storage, id) == 0)
        .count();
    let orphan_ratio = ratio(orphan_count, node_count);

    // --- Contradiction ratio + conductance entropy (single edge scan) ---
    let mut contradiction_count = 0usize;
    let mut conductance_buckets = [0usize; 4];
    for &eid in &edge_ids {
        if let Ok(edge) = storage.get_edge(eid) {
            if matches!(edge.edge_type, EdgeType::Contradicts) {
                contradiction_count += 1;
            }
            let weight = crate::mechanics::priors::project_weight(
                storage.get_conductance(eid).unwrap_or(edge.conductance),
            );
            conductance_buckets[projection_bucket(weight)] += 1;
        }
    }
    let contradiction_ratio = ratio(contradiction_count, edge_count);
    let conductance_entropy = bucket_entropy(&conductance_buckets, edge_count);

    // --- Salience entropy + scope distribution + stale ratio (single node scan) ---
    let mut salience_buckets = [0usize; 4];
    let mut scope_distribution: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut stale_count = 0usize;
    let stale_window_ms = STALE_WINDOW_DAYS * DAY_MS;

    for &id in &node_ids {
        let salience = storage.get_salience(id).unwrap_or(0.0);
        salience_buckets[projection_bucket(salience)] += 1;

        if let Ok(node) = storage.get_node(id) {
            let key = if node.origin.scope.is_universal() {
                "universal".to_string()
            } else {
                node.origin.scope.as_str().to_string()
            };
            *scope_distribution.entry(key).or_insert(0) += 1;
        }

        let accessed = storage.get_accessed_at(id).unwrap_or(Timestamp(0)).0;
        let elapsed = now.0.saturating_sub(accessed) as f64;
        if elapsed > stale_window_ms {
            stale_count += 1;
        }
    }
    let salience_entropy = bucket_entropy(&salience_buckets, node_count);
    let stale_ratio = ratio(stale_count, node_count);

    // --- Average degree ---
    let average_degree = if node_count > 0 {
        2.0 * edge_count as f64 / node_count as f64
    } else {
        0.0
    };

    GraphHealth {
        node_count,
        edge_count,
        orphan_ratio,
        contradiction_ratio,
        salience_entropy,
        conductance_entropy,
        average_degree,
        scope_distribution,
        stale_ratio,
    }
}

/// Fraction `numerator / denominator`, or `0.0` when the denominator is zero.
#[inline]
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator > 0 {
        numerator as f64 / denominator as f64
    } else {
        0.0
    }
}

/// Map a projection value in `[0, 1]` to its bucket index.
///
/// Buckets: `[0, 0.1)` -> 0, `[0.1, 0.4)` -> 1, `[0.4, 0.8)` -> 2, `[0.8, 1.0]` -> 3.
fn projection_bucket(value: f64) -> usize {
    if value < 0.1 {
        0
    } else if value < 0.4 {
        1
    } else if value < 0.8 {
        2
    } else {
        3
    }
}

/// Shannon entropy of a four-bucket count histogram over `total` items.
fn bucket_entropy(buckets: &[usize; 4], total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let total = total as f64;
    let probs: Vec<f64> = buckets
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| count as f64 / total)
        .collect();
    shannon_entropy(&probs)
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
    fn projection_bucket_boundaries() {
        assert_eq!(projection_bucket(0.0), 0);
        assert_eq!(projection_bucket(0.05), 0);
        assert_eq!(projection_bucket(0.099), 0);
        assert_eq!(projection_bucket(0.1), 1);
        assert_eq!(projection_bucket(0.39), 1);
        assert_eq!(projection_bucket(0.4), 2);
        assert_eq!(projection_bucket(0.79), 2);
        assert_eq!(projection_bucket(0.8), 3);
        assert_eq!(projection_bucket(1.0), 3);
    }

    #[test]
    fn bucket_entropy_empty_is_zero() {
        assert_eq!(bucket_entropy(&[0, 0, 0, 0], 0), 0.0);
    }

    #[test]
    fn bucket_entropy_single_bucket_is_zero() {
        assert_eq!(bucket_entropy(&[5, 0, 0, 0], 5), 0.0);
    }

    #[test]
    fn bucket_entropy_uniform_four_is_two_bits() {
        let h = bucket_entropy(&[1, 1, 1, 1], 4);
        assert!((h - 2.0).abs() < 1e-10, "expected 2.0, got {h}");
    }
}
