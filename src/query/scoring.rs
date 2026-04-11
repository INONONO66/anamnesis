//! Final scoring and scope weighting for query results.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equation
//! (13) R_i = (0.50 * X'_i + 0.20 * q_i + 0.15 * s_i + 0.15 * m_i) * scope_w(i)

/// Computes the scope weight for a node relative to the query context.
///
/// Nodes in the same project get full weight. Universal nodes get 0.85.
/// Different-project nodes get 0.30, with entity overlap bonus up to 0.25.
pub fn scope_weight(
    query_project: &Option<String>,
    node_project: &Option<String>,
    shared_entity_count: usize,
) -> f64 {
    match (query_project, node_project) {
        (Some(q), Some(n)) if q == n => 1.0,
        (_, None) | (None, Some(_)) => 0.85,
        _ => {
            let bonus = match shared_entity_count {
                0 => 0.0,
                1 => 0.15,
                _ => 0.25,
            };
            f64::min(0.30 + bonus, 1.0)
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
    let raw = 0.50 * activation + 0.20 * vector_sim + 0.15 * salience + 0.15 * mass;
    (raw * scope_w).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn same_project_full_weight() {
        let w = scope_weight(&Some("proj-a".to_string()), &Some("proj-a".to_string()), 0);
        assert_eq!(w, 1.0);
    }

    #[test]
    fn universal_node_weight() {
        let w = scope_weight(&Some("proj-a".to_string()), &None, 0);
        assert_eq!(w, 0.85);
    }

    #[test]
    fn universal_query_weight() {
        let w = scope_weight(&None, &Some("proj-b".to_string()), 0);
        assert_eq!(w, 0.85);
    }

    #[test]
    fn different_project_base_weight() {
        let w = scope_weight(&Some("proj-a".to_string()), &Some("proj-b".to_string()), 0);
        assert_eq!(w, 0.30);
    }

    #[test]
    fn entity_overlap_bonus_one() {
        let w = scope_weight(&Some("proj-a".to_string()), &Some("proj-b".to_string()), 1);
        assert!((w - 0.45).abs() < 1e-10, "expected 0.45, got {w}");
    }

    #[test]
    fn entity_overlap_bonus_two_plus() {
        let w = scope_weight(&Some("proj-a".to_string()), &Some("proj-b".to_string()), 2);
        assert!((w - 0.55).abs() < 1e-10, "expected 0.55, got {w}");
    }

    #[test]
    fn scope_weight_capped_at_one() {
        let w = scope_weight(&Some("proj-a".to_string()), &Some("proj-a".to_string()), 5);
        assert_eq!(w, 1.0);
    }

    #[test]
    fn entity_bonus_not_applied_to_same_project() {
        let w = scope_weight(&Some("proj-a".to_string()), &Some("proj-a".to_string()), 3);
        assert_eq!(w, 1.0);
    }

    #[test]
    fn entity_bonus_not_applied_to_universal() {
        let w = scope_weight(&Some("proj-a".to_string()), &None, 3);
        assert_eq!(w, 0.85);
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
