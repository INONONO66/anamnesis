//! Invariant checks and operational warnings — read-only diagnostics.
//!
//! Implements the structural [`InvariantCheck`] suite and the
//! [`OperationalWarning`] set named in
//! [observability.md](../../docs/07-quality-gates/observability.md). These are
//! debugging/evaluation surfaces: they observe the graph and never mutate it.
//!
//! The eight doc invariants are split by what they can see:
//!
//! - **storage-local** (computed here, in [`check_storage_invariants`]):
//!   `projection_range`, `adjacency_consistency`, `missing_origins`,
//!   `invalid_validity_intervals`, `dangling_edges`, `private_scope_leakage`,
//!   `non_finite_hot_fields`, plus the standing reservoir-finite invariant.
//! - **engine-local** (added by [`crate::api::Engine::check_invariants`]):
//!   `snapshot_restore_consistency` (clone → restore → re-read must match
//!   field-for-field: every `Node`/`Edge` record plus the authoritative SoA
//!   reservoir/hot fields — `retained_action`, `conductance`, `accessed_at`,
//!   `decay_checkpoint` — so a clone that silently drops a column is caught even
//!   when no aggregate health metric shifts) and `determinism` (same graph +
//!   query twice → identical `ContextPackage`), which need a clone and a query
//!   runner the storage layer does not have.
//!
//! A reservoir is always finite (ADR-0002 / ADR-0003); a projection is always in
//! `[0, 1]`; flow never crosses disjoint private scopes. A violation is surfaced,
//! not silently repaired.

use crate::graph::scope::ScopeRelation;
use crate::graph::{EdgeType, NodeId};
use crate::storage::StorageAdapter;

/// One invariant in the [`InvariantCheck`] suite.
///
/// Each variant names a property the engine guarantees. The ordering is the doc
/// ordering ([observability.md](../../docs/07-quality-gates/observability.md))
/// with the standing reservoir-finite and the engine-level determinism checks
/// appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InvariantCheck {
    /// Projection values (`salience`, `weight`) lie inside `[0, 1]`.
    ProjectionRange,
    /// Adjacency index agrees with stored edge endpoints (consistency).
    AdjacencyConsistency,
    /// Every node carries an origin (no missing origins).
    MissingOrigins,
    /// Every validity interval is well-formed (`valid_from < valid_until`).
    InvalidValidityIntervals,
    /// No edge references a non-existent endpoint (no dangling edges).
    DanglingEdges,
    /// No propagating edge bridges two disjoint private scopes (leakage).
    PrivateScopeLeakage,
    /// No hot field (`salience`, `weight`) is NaN/Inf (non-finite hot fields).
    NonFiniteHotFields,
    /// Every reservoir (`retained_action` `A_i`, `conductance` `C_ij`) is finite.
    ReservoirFinite,
    /// A clone → restore round-trip reproduces identical state.
    SnapshotRestoreConsistency,
    /// The same graph + query, run twice, yields an identical `ContextPackage`.
    Determinism,
}

impl InvariantCheck {
    /// All invariants in the suite, in canonical (doc) order.
    pub const ALL: [InvariantCheck; 10] = [
        InvariantCheck::ProjectionRange,
        InvariantCheck::AdjacencyConsistency,
        InvariantCheck::MissingOrigins,
        InvariantCheck::InvalidValidityIntervals,
        InvariantCheck::DanglingEdges,
        InvariantCheck::PrivateScopeLeakage,
        InvariantCheck::NonFiniteHotFields,
        InvariantCheck::ReservoirFinite,
        InvariantCheck::SnapshotRestoreConsistency,
        InvariantCheck::Determinism,
    ];
}

/// Outcome of one invariant check.
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantResult {
    /// Which invariant this result reports on.
    pub check: InvariantCheck,
    /// Whether the invariant held.
    pub passed: bool,
    /// Number of distinct violations found (`0` when `passed`).
    pub violation_count: usize,
    /// Human-readable detail of the first few violations (for debugging).
    pub detail: Option<String>,
}

impl InvariantResult {
    /// A clean (passing) result for `check`.
    pub fn ok(check: InvariantCheck) -> Self {
        InvariantResult {
            check,
            passed: true,
            violation_count: 0,
            detail: None,
        }
    }

    /// A failing result for `check` with `count` violations and an optional detail.
    pub fn failed(check: InvariantCheck, count: usize, detail: impl Into<String>) -> Self {
        InvariantResult {
            check,
            passed: false,
            violation_count: count,
            detail: Some(detail.into()),
        }
    }
}

/// The full result of an invariant sweep over the graph.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InvariantReport {
    /// One result per invariant checked, in canonical order.
    pub results: Vec<InvariantResult>,
}

impl InvariantReport {
    /// Whether every checked invariant held.
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }

    /// The results for invariants that were violated.
    pub fn violations(&self) -> impl Iterator<Item = &InvariantResult> {
        self.results.iter().filter(|r| !r.passed)
    }

    /// Look up the result for a specific check, if present.
    pub fn get(&self, check: InvariantCheck) -> Option<&InvariantResult> {
        self.results.iter().find(|r| r.check == check)
    }
}

/// How many violation examples to record in a result's `detail` string.
const MAX_DETAIL_EXAMPLES: usize = 8;

/// Run the storage-local invariant checks over `storage`.
///
/// Covers every doc invariant that is decidable from a single graph snapshot:
/// `projection_range`, `adjacency_consistency`, `missing_origins`,
/// `invalid_validity_intervals`, `dangling_edges`, `private_scope_leakage`,
/// `non_finite_hot_fields`, and the standing reservoir-finite invariant. The
/// engine-level `snapshot_restore_consistency` and `determinism` checks are
/// appended by [`crate::api::Engine::check_invariants`].
pub fn check_storage_invariants<S: StorageAdapter>(storage: &S) -> Vec<InvariantResult> {
    let node_ids = storage.all_node_ids();
    let edge_ids = storage.all_edge_ids();
    let live_nodes: std::collections::HashSet<NodeId> = node_ids.iter().copied().collect();

    let mut projection_range = Vec::new();
    let mut missing_origins = Vec::new();
    let mut invalid_intervals = Vec::new();
    let mut non_finite_hot = Vec::new();
    let mut reservoir_non_finite = Vec::new();

    for &id in &node_ids {
        let salience = storage.get_salience(id).unwrap_or(f64::NAN);
        if !salience.is_finite() {
            non_finite_hot.push(format!("node {id:?} salience = {salience}"));
        } else if !(0.0..=1.0).contains(&salience) {
            projection_range.push(format!("node {id:?} salience = {salience}"));
        }

        // The persistent decay-exempt evidence prior P_i must be finite (ADR-0008).
        // The composite cache A_i = B_i + P_i is also checked: B_i is recomputed from
        // access_history and is finite as long as the creation trace keeps the
        // history non-empty, so a non-finite A_i flags either a non-finite P_i or an
        // empty trace window.
        let prior = storage.get_evidence_prior(id).unwrap_or(f64::NAN);
        if !prior.is_finite() {
            reservoir_non_finite.push(format!("node {id:?} evidence_prior = {prior}"));
        }
        let action = storage.get_retained_action(id).unwrap_or(f64::NAN);
        if !action.is_finite() {
            reservoir_non_finite.push(format!("node {id:?} retained_action = {action}"));
        }

        if let Ok(node) = storage.get_node(id) {
            // A default-constructed origin still carries a peer_id; "missing origin"
            // here means an empty session with a non-finite confidence — a record
            // that lost its provenance. We treat a non-finite confidence as missing.
            if !node.origin.confidence.is_finite() {
                missing_origins.push(format!("node {id:?} has non-finite origin confidence"));
            }
            // Well-formed validity interval: from < until (half-open, non-empty).
            if !interval_well_formed(node.valid_from, node.valid_until) {
                let from = node.valid_from.map(|t| t.0).unwrap_or(0);
                let until = node.valid_until.map(|t| t.0).unwrap_or(0);
                invalid_intervals.push(format!(
                    "node {id:?} interval [{from}, {until}) is empty/inverted"
                ));
            }
        }
    }

    let mut adjacency = Vec::new();
    let mut dangling = Vec::new();
    let mut scope_leakage = Vec::new();

    for &eid in &edge_ids {
        if let Ok(edge) = storage.get_edge(eid) {
            let weight = edge.weight;
            if !weight.is_finite() {
                non_finite_hot.push(format!("edge {eid:?} weight = {weight}"));
            } else if !(0.0..=1.0).contains(&weight) {
                projection_range.push(format!("edge {eid:?} weight = {weight}"));
            }

            let conductance = storage.get_conductance(eid).unwrap_or(edge.conductance);
            if !conductance.is_finite() {
                reservoir_non_finite.push(format!("edge {eid:?} conductance = {conductance}"));
            }

            // Dangling: endpoint not live.
            let source_live = live_nodes.contains(&edge.source);
            let target_live = live_nodes.contains(&edge.target);
            if !source_live || !target_live {
                dangling.push(format!(
                    "edge {eid:?} ({:?} -> {:?}) references a non-live endpoint",
                    edge.source, edge.target
                ));
                continue;
            }

            // Adjacency consistency: the adjacency index must list this edge for
            // both endpoints.
            if !storage.edges_from(edge.source).contains(&eid) {
                adjacency.push(format!(
                    "edge {eid:?} missing from edges_from({:?})",
                    edge.source
                ));
            }
            if !storage.edges_to(edge.target).contains(&eid) {
                adjacency.push(format!(
                    "edge {eid:?} missing from edges_to({:?})",
                    edge.target
                ));
            }

            // Private-scope leakage: a *propagating* edge (one that carries flow —
            // i.e. not `Contradicts`, which is excluded from propagation) must not
            // bridge two sites whose origin scopes are mutually disjoint
            // (`Disjoint`) when neither side is universal. Such an edge would let
            // private knowledge in one scope light up a node a query in the other,
            // disjoint scope can reach.
            if !matches!(edge.edge_type, EdgeType::Contradicts) {
                if let (Ok(src), Ok(tgt)) =
                    (storage.get_node(edge.source), storage.get_node(edge.target))
                {
                    let src_scope = &src.origin.scope;
                    let tgt_scope = &tgt.origin.scope;
                    if !src_scope.is_universal()
                        && !tgt_scope.is_universal()
                        && src_scope.relation_to(tgt_scope) == ScopeRelation::Disjoint
                    {
                        scope_leakage.push(format!(
                            "edge {eid:?} bridges disjoint scopes {} <-> {}",
                            src_scope, tgt_scope
                        ));
                    }
                }
            }
        }
    }

    vec![
        result(InvariantCheck::ProjectionRange, projection_range),
        result(InvariantCheck::AdjacencyConsistency, adjacency),
        result(InvariantCheck::MissingOrigins, missing_origins),
        result(InvariantCheck::InvalidValidityIntervals, invalid_intervals),
        result(InvariantCheck::DanglingEdges, dangling),
        result(InvariantCheck::PrivateScopeLeakage, scope_leakage),
        result(InvariantCheck::NonFiniteHotFields, non_finite_hot),
        result(InvariantCheck::ReservoirFinite, reservoir_non_finite),
    ]
}

/// Build a passing/failing [`InvariantResult`] from a list of violation strings.
fn result(check: InvariantCheck, violations: Vec<String>) -> InvariantResult {
    if violations.is_empty() {
        InvariantResult::ok(check)
    } else {
        let count = violations.len();
        let detail = violations
            .into_iter()
            .take(MAX_DETAIL_EXAMPLES)
            .collect::<Vec<_>>()
            .join("; ");
        InvariantResult::failed(check, count, detail)
    }
}

/// Whether a bitemporal validity interval is well-formed: a present pair must be
/// non-empty under the half-open `[from, until)` convention shared with
/// [`crate::graph::valid_at`] (an interval with `from >= until` can never be valid at any
/// `as_of`). The validity-interval invariant uses this predicate.
#[inline]
pub fn interval_well_formed(
    from: Option<crate::graph::Timestamp>,
    until: Option<crate::graph::Timestamp>,
) -> bool {
    match (from, until) {
        (Some(f), Some(u)) => f.0 < u.0,
        _ => true,
    }
}

// ── Operational warnings (observability.md) ────────────────────────────────

/// A heuristic operational warning derived from [`crate::mechanics::health::GraphHealth`].
///
/// These are the five rows of the observability.md "Operational Warnings" table.
/// Each names a likely cause and a recommended action; they are advisory, never
/// errors. Thresholds are calibrated priors.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OperationalWarning {
    /// `high orphan ratio` — conductance threshold too strict.
    HighOrphanRatio,
    /// `high contradiction ratio` — over-linking entities or stale facts.
    HighContradictionRatio,
    /// `low entropy` — salience projections collapsed.
    LowSalienceEntropy,
    /// `dense graph` — excess edge proposal.
    DenseGraph,
    /// `stale core` — important identity not accessed.
    StaleCore,
}

impl OperationalWarning {
    /// The likely cause, per the observability.md table.
    pub fn likely_cause(&self) -> &'static str {
        match self {
            OperationalWarning::HighOrphanRatio => "Conductance threshold too strict",
            OperationalWarning::HighContradictionRatio => "Over-linking entities or stale facts",
            OperationalWarning::LowSalienceEntropy => "Salience projections collapsed",
            OperationalWarning::DenseGraph => "Excess edge proposal",
            OperationalWarning::StaleCore => "Important identity not accessed",
        }
    }

    /// The recommended action, per the observability.md table.
    pub fn action(&self) -> &'static str {
        match self {
            OperationalWarning::HighOrphanRatio => "Recalibrate threshold or candidate generation",
            OperationalWarning::HighContradictionRatio => "Review tension handling",
            OperationalWarning::LowSalienceEntropy => "Inspect dissipation and reinforcement",
            OperationalWarning::DenseGraph => "Apply edge budget / leakage",
            OperationalWarning::StaleCore => "Inspect packaging policy",
        }
    }
}

/// Orphan-ratio warning threshold (CALIBRATED PRIOR).
const WARN_ORPHAN_RATIO: f64 = 0.30;
/// Contradiction-ratio warning threshold (CALIBRATED PRIOR).
const WARN_CONTRADICTION_RATIO: f64 = 0.15;
/// Salience-entropy warning floor in bits (CALIBRATED PRIOR): below this on a
/// non-trivial graph the projections have collapsed onto a single bucket.
const WARN_LOW_ENTROPY: f64 = 0.10;
/// Average-degree warning threshold (CALIBRATED PRIOR): a denser mean degree
/// signals over-proposal of edges.
const WARN_DENSE_DEGREE: f64 = 20.0;
/// Minimum node count before entropy/degree warnings are meaningful.
const WARN_MIN_NODES: usize = 8;

/// Derive the operational warnings implied by a [`crate::mechanics::health::GraphHealth`] summary.
///
/// `stale_core` is reported when the graph is stale overall (high `stale_ratio`)
/// — a proxy for "an important identity has not been accessed"; the engine layer
/// refines this with tier knowledge. Returned in canonical order.
pub fn derive_warnings(health: &super::health::GraphHealth) -> Vec<OperationalWarning> {
    let mut warnings = Vec::new();

    if health.orphan_ratio > WARN_ORPHAN_RATIO {
        warnings.push(OperationalWarning::HighOrphanRatio);
    }
    if health.contradiction_ratio > WARN_CONTRADICTION_RATIO {
        warnings.push(OperationalWarning::HighContradictionRatio);
    }
    if health.node_count >= WARN_MIN_NODES && health.salience_entropy < WARN_LOW_ENTROPY {
        warnings.push(OperationalWarning::LowSalienceEntropy);
    }
    if health.node_count >= WARN_MIN_NODES && health.average_degree > WARN_DENSE_DEGREE {
        warnings.push(OperationalWarning::DenseGraph);
    }
    if health.stale_ratio > 0.5 {
        warnings.push(OperationalWarning::StaleCore);
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mechanics::health::GraphHealth;

    fn health_template() -> GraphHealth {
        GraphHealth {
            node_count: 100,
            edge_count: 100,
            orphan_ratio: 0.0,
            contradiction_ratio: 0.0,
            salience_entropy: 1.0,
            conductance_entropy: 1.0,
            average_degree: 2.0,
            scope_distribution: std::collections::BTreeMap::new(),
            stale_ratio: 0.0,
        }
    }

    #[test]
    fn no_warnings_on_healthy_graph() {
        assert!(derive_warnings(&health_template()).is_empty());
    }

    #[test]
    fn high_orphan_ratio_warns() {
        let mut h = health_template();
        h.orphan_ratio = 0.5;
        assert!(derive_warnings(&h).contains(&OperationalWarning::HighOrphanRatio));
    }

    #[test]
    fn high_contradiction_ratio_warns() {
        let mut h = health_template();
        h.contradiction_ratio = 0.2;
        assert!(derive_warnings(&h).contains(&OperationalWarning::HighContradictionRatio));
    }

    #[test]
    fn low_entropy_warns() {
        let mut h = health_template();
        h.salience_entropy = 0.0;
        assert!(derive_warnings(&h).contains(&OperationalWarning::LowSalienceEntropy));
    }

    #[test]
    fn dense_graph_warns() {
        let mut h = health_template();
        h.average_degree = 50.0;
        assert!(derive_warnings(&h).contains(&OperationalWarning::DenseGraph));
    }

    #[test]
    fn stale_core_warns() {
        let mut h = health_template();
        h.stale_ratio = 0.9;
        assert!(derive_warnings(&h).contains(&OperationalWarning::StaleCore));
    }

    #[test]
    fn invariant_check_all_has_ten() {
        assert_eq!(InvariantCheck::ALL.len(), 10);
    }
}
