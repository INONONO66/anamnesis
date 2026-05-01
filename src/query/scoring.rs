//! Final scoring and scope weighting for query results.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equation
//! (13) R_i = (0.50 * X'_i + 0.20 * q_i + 0.15 * s_i + 0.15 * m_i) * scope_w(i)

use crate::graph::ScopePath;
use crate::graph::scope::ScopeRelation;
use crate::mechanics::forces::{
    ActivationForce, Force, MassForce, NodeContext, QueryContext, SalienceForce, SimilarityForce,
    weighted_contribution,
};

static ACTIVATION_FORCE: ActivationForce = ActivationForce;
static SIMILARITY_FORCE: SimilarityForce = SimilarityForce;
static SALIENCE_FORCE: SalienceForce = SalienceForce;
static MASS_FORCE: MassForce = MassForce;

/// Maximum shared-entity bonus added to the Unrelated base weight.
const UNRELATED_BONUS_CAP: f64 = 0.20;

/// Computes the scope weight for a node relative to the query context.
///
/// Hierarchical weighting based on `ScopeRelation` (locked):
/// - Exact: 1.0
/// - Universal: 0.95
/// - Ancestor / Descendant: 0.85
/// - Sibling: 0.50
/// - Unrelated: 0.05 + shared-entity bonus capped at +0.20
pub fn scope_weight(
    query_scope: &ScopePath,
    node_scope: &ScopePath,
    shared_entity_count: usize,
) -> f64 {
    match query_scope.relation_to(node_scope) {
        ScopeRelation::Exact => 1.0,
        ScopeRelation::Universal => 0.95,
        ScopeRelation::Ancestor | ScopeRelation::Descendant => 0.85,
        ScopeRelation::Sibling => 0.50,
        ScopeRelation::Unrelated => {
            let bonus = match shared_entity_count {
                0 => 0.0,
                1 => 0.10,
                _ => UNRELATED_BONUS_CAP,
            };
            0.05 + bonus.min(UNRELATED_BONUS_CAP)
        }
    }
}

/// Computes the final relevance score for a node.
///
/// Equation (13): R_i = (0.50 * X'_i + 0.20 * q_i + 0.15 * s_i + 0.15 * m_i) * scope_w
pub fn final_score(
    activation: f64,
    vector_sim: f64,
    salience: f64,
    mass: f64,
    scope_w: f64,
) -> f64 {
    let node = NodeContext::scoring(activation, vector_sim, salience, mass);
    let query = QueryContext {
        scope_weight: scope_w,
    };
    let forces = all_forces();

    compute_with_forces(&node, &query, &forces)
}

/// Return the default final-score force set.
///
/// This intentionally includes only the four force components from equation (13).
/// Repulsion and identity forces remain available for explicit ablation, but are not
/// part of the default final score because query routing already applies them in
/// earlier stages.
pub fn all_forces() -> [&'static dyn Force; 4] {
    [
        &ACTIVATION_FORCE,
        &SIMILARITY_FORCE,
        &SALIENCE_FORCE,
        &MASS_FORCE,
    ]
}

/// Compose a final relevance score from an explicit force list.
///
/// The weighted sum is multiplied by `query.scope_weight` and clamped to `[0, 1]`,
/// matching `final_score` for `all_forces()`.
pub fn compute_with_forces(node: &NodeContext, query: &QueryContext, forces: &[&dyn Force]) -> f64 {
    let mut raw = 0.0;

    for force in forces {
        raw += weighted_contribution(*force, node, query);
    }

    (raw * query.scope_weight).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn proj(s: &str) -> ScopePath {
        ScopePath::new(s).expect("valid scope")
    }

    #[test]
    fn same_project_full_weight() {
        let w = scope_weight(&proj("proj-a"), &proj("proj-a"), 0);
        assert_eq!(w, 1.0);
    }

    #[test]
    fn universal_node_weight() {
        let w = scope_weight(&proj("proj-a"), &ScopePath::universal(), 0);
        assert_eq!(w, 0.95);
    }

    #[test]
    fn universal_query_weight() {
        let w = scope_weight(&ScopePath::universal(), &proj("proj-b"), 0);
        assert_eq!(w, 0.95);
    }

    #[test]
    fn ancestor_weight() {
        let w = scope_weight(&proj("proj-a"), &proj("proj-a/feature"), 0);
        assert_eq!(w, 0.85);
    }

    #[test]
    fn descendant_weight() {
        let w = scope_weight(&proj("proj-a/feature"), &proj("proj-a"), 0);
        assert_eq!(w, 0.85);
    }

    #[test]
    fn sibling_weight() {
        let w = scope_weight(&proj("proj-a/x"), &proj("proj-a/y"), 0);
        assert_eq!(w, 0.50);
    }

    #[test]
    fn unrelated_base_weight() {
        let w = scope_weight(&proj("proj-a"), &proj("proj-b"), 0);
        assert_eq!(w, 0.05);
    }

    #[test]
    fn entity_overlap_bonus_one() {
        let w = scope_weight(&proj("proj-a"), &proj("proj-b"), 1);
        assert!((w - 0.15).abs() < 1e-10, "expected 0.15, got {w}");
    }

    #[test]
    fn entity_overlap_bonus_two_plus() {
        let w = scope_weight(&proj("proj-a"), &proj("proj-b"), 2);
        assert!((w - 0.25).abs() < 1e-10, "expected 0.25, got {w}");
    }

    #[test]
    fn unrelated_bonus_capped_at_twenty_percent() {
        let w = scope_weight(&proj("proj-a"), &proj("proj-b"), 100);
        assert!((w - 0.25).abs() < 1e-10, "expected 0.25 (cap), got {w}");
    }

    #[test]
    fn entity_bonus_not_applied_to_same_project() {
        let w = scope_weight(&proj("proj-a"), &proj("proj-a"), 3);
        assert_eq!(w, 1.0);
    }

    #[test]
    fn entity_bonus_not_applied_to_universal() {
        let w = scope_weight(&proj("proj-a"), &ScopePath::universal(), 3);
        assert_eq!(w, 0.95);
    }

    #[test]
    fn all_ones_gives_one() {
        let score = final_score(1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((score - 1.0).abs() < 1e-10);
    }

    #[test]
    fn all_zeros_gives_zero() {
        let score = final_score(0.0, 0.0, 0.0, 0.0, 1.0);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn scope_zero_gives_zero() {
        let score = final_score(1.0, 1.0, 1.0, 1.0, 0.0);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn activation_dominates() {
        let high_activation = final_score(1.0, 0.0, 0.0, 0.0, 1.0);
        let high_vector = final_score(0.0, 1.0, 0.0, 0.0, 1.0);
        assert!(
            high_activation > high_vector,
            "activation ({high_activation}) should dominate vector_sim ({high_vector})"
        );
    }

    proptest! {
        #[test]
        fn final_score_in_bounds(
            activation in 0.0f64..=1.0,
            vector_sim in 0.0f64..=1.0,
            salience in 0.0f64..=1.0,
            mass in 0.0f64..=1.0,
            scope_w in 0.0f64..=1.0,
        ) {
            let score = final_score(activation, vector_sim, salience, mass, scope_w);
            prop_assert!(score >= 0.0, "score negative: {score}");
            prop_assert!(score <= 1.0 + 1e-10, "score > 1: {score}");
        }
    }
}
