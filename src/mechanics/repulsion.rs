//! Repulsion mechanics — contradiction-based activation damping.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equations
//! - (7) Repulsion: H_i = sum_{j: Contradicts(i,j)} w_ij_neg * rho_j * X_j
//! - (8) Damping: X'_i = X_i * exp(-1.5 * H_i)

use crate::graph::KnowledgeType;

/// Returns the rigidity (rho) of a knowledge type.
///
/// Higher rigidity = stronger repulsion effect when this node contradicts another.
/// IdentityCore is maximally rigid — contradictions against core identity are strongly suppressed.
pub fn rigidity(kt: &KnowledgeType) -> f64 {
    match kt {
        KnowledgeType::IdentityCore => 1.00,
        KnowledgeType::Convention | KnowledgeType::Decision => 0.75,
        KnowledgeType::IdentityLearned | KnowledgeType::IdentityState => 0.50,
        KnowledgeType::Semantic | KnowledgeType::Procedural | KnowledgeType::Entity => 0.25,
        KnowledgeType::Gotcha => 0.25,
        KnowledgeType::Episodic | KnowledgeType::Event => 0.10,
        KnowledgeType::Custom(_) => 0.25,
    }
}

/// Computes the repulsion accumulation for a node.
///
/// Equation (7): H_i = sum_{j: Contradicts(i,j)} w_ij_neg * rho_j * X_j
///
/// `contradicts_edges`: slice of (edge_weight, rigidity_j, activation_j) tuples
/// for all Contradicts edges pointing at node i.
///
/// Returns H_i >= 0. Higher H_i = more repulsion.
pub fn compute_repulsion(contradicts_edges: &[(f64, f64, f64)]) -> f64 {
    contradicts_edges
        .iter()
        .map(|(w, rho, x)| w * rho * x)
        .sum::<f64>()
        .max(0.0)
}

/// Applies repulsion damping to an activation value.
///
/// Equation (8): X'_i = X_i * exp(-1.5 * H_i)
///
/// - `activation`: current activation [0, 1]
/// - `repulsion`: accumulated repulsion H_i from `compute_repulsion()`
///
/// Returns the damped activation, clamped to [0, 1].
pub fn apply_damping(activation: f64, repulsion: f64) -> f64 {
    (activation * (-1.5 * repulsion).exp()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn identity_core_max_rigidity() {
        assert_eq!(rigidity(&KnowledgeType::IdentityCore), 1.00);
    }

    #[test]
    fn episodic_min_rigidity() {
        assert_eq!(rigidity(&KnowledgeType::Episodic), 0.10);
    }

    #[test]
    fn no_contradicts_zero_repulsion() {
        assert_eq!(compute_repulsion(&[]), 0.0);
    }

    #[test]
    fn single_contradiction_repulsion() {
        // w=1.0, rho=1.0, X=1.0 → H = 1.0
        let h = compute_repulsion(&[(1.0, 1.0, 1.0)]);
        assert!((h - 1.0).abs() < 1e-10);
    }

    #[test]
    fn no_repulsion_no_damping() {
        // H=0 → X' = X * exp(0) = X
        assert!((apply_damping(0.8, 0.0) - 0.8).abs() < 1e-10);
    }

    #[test]
    fn strong_repulsion_heavy_damping() {
        // H=1.0 → X' = 0.8 * exp(-1.5) ≈ 0.179
        let result = apply_damping(0.8, 1.0);
        let expected = 0.8 * (-1.5_f64).exp();
        assert!(
            (result - expected).abs() < 1e-6,
            "expected {expected}, got {result}"
        );
    }

    #[test]
    fn damping_reduces_activation() {
        let original = 0.8;
        let damped = apply_damping(original, 0.5);
        assert!(damped < original, "damping should reduce activation");
    }

    #[test]
    fn multiple_contradictions_accumulate() {
        let h = compute_repulsion(&[(0.5, 1.0, 0.8), (0.3, 0.75, 0.6)]);
        let expected = 0.5 * 1.0 * 0.8 + 0.3 * 0.75 * 0.6;
        assert!((h - expected).abs() < 1e-10);
    }

    proptest! {
        #[test]
        fn damping_output_in_bounds(
            activation in 0.0f64..=1.0,
            repulsion in 0.0f64..=5.0,
        ) {
            let result = apply_damping(activation, repulsion);
            prop_assert!(result >= 0.0, "damping negative: {result}");
            prop_assert!(result <= activation + 1e-10, "damping increased activation");
        }

        #[test]
        fn repulsion_non_negative(
            w in 0.0f64..=1.0,
            rho in 0.0f64..=1.0,
            x in 0.0f64..=1.0,
        ) {
            let h = compute_repulsion(&[(w, rho, x)]);
            prop_assert!(h >= 0.0, "repulsion negative: {h}");
        }
    }
}
