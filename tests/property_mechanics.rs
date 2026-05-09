//! Property-based tests for pure mechanics functions.
//!
//! Tests invariants for pure functions in the mechanics and query modules
//! using proptest with 256 cases per test.

use anamnesis::graph::KnowledgeType;
use anamnesis::mechanics::attraction::{attraction_score, cosine_similarity, strengthen_edge};
use anamnesis::mechanics::forgetting::{
    base_level_to_salience, decay_salience, floor_for_type, lambda_for_type, reinforce_salience,
};
use anamnesis::mechanics::gravity::{compute_mass, gravity_boost, normalize_access_count};
use anamnesis::query::activation::{initial_activation, propagation_strength, salience_gate};
use proptest::prelude::*;

// ── Strategy for generating KnowledgeType variants ──────────────────────────

fn knowledge_type_strategy() -> impl Strategy<Value = KnowledgeType> {
    prop_oneof![
        Just(KnowledgeType::IdentityCore),
        Just(KnowledgeType::IdentityLearned),
        Just(KnowledgeType::IdentityState),
        Just(KnowledgeType::Semantic),
        Just(KnowledgeType::Procedural),
        Just(KnowledgeType::Entity),
        Just(KnowledgeType::Convention),
        Just(KnowledgeType::Decision),
        Just(KnowledgeType::Gotcha),
        Just(KnowledgeType::Hypothesis),
        Just(KnowledgeType::Evidence),
        Just(KnowledgeType::DebugSession),
        Just(KnowledgeType::Episodic),
        Just(KnowledgeType::Event),
    ]
}

// ── Property tests for forgetting mechanics ──────────────────────────────────

proptest! {
    /// decay_salience: result ≤ current, result ≥ 0.0
    /// For decaying types (lambda > 0): result ≥ floor_for_type(kt) when current >= floor
    /// For inert types (lambda = 0): result == current
    #[test]
    fn prop_decay_salience_bounds(
        current in 0.0f64..=1.0,
        dt_days in 0.0f64..=365.0,
        kt in knowledge_type_strategy(),
    ) {
        let result = decay_salience(current, dt_days, &kt);
        let lambda = lambda_for_type(&kt);
        let floor = floor_for_type(&kt);

        // Result must be non-negative and not exceed current
        prop_assert!(result >= 0.0, "decay result negative: {result}");
        prop_assert!(result <= current + 1e-10, "decay increased salience: {result} > {current}");

        // For inert types (lambda = 0), result must equal current
        if lambda == 0.0 {
            prop_assert!(
                (result - current).abs() < 1e-10,
                "inert type should not decay: {result} != {current}"
            );
        } else {
            // For decaying types, if current >= floor, result must be >= floor
            if current >= floor {
                prop_assert!(
                    result >= floor - 1e-10,
                    "decay below floor: {result} < {floor}"
                );
            } else {
                // If already below floor, result should equal current (unchanged)
                prop_assert!(
                    (result - current).abs() < 1e-10,
                    "below-floor input should be unchanged: {result} != {current}"
                );
            }
        }
    }

    /// reinforce_salience: result ≥ current, result ≤ 1.0
    /// When current == 1.0, result == 1.0
    #[test]
    fn prop_reinforce_salience_bounds(current in 0.0f64..=1.0) {
        let result = reinforce_salience(current);

        // Result must be >= current and <= 1.0
        prop_assert!(result >= current - 1e-10, "reinforce decreased salience: {result} < {current}");
        prop_assert!(result <= 1.0 + 1e-10, "reinforce exceeded 1.0: {result}");

        // At saturation, no further boost
        if (current - 1.0).abs() < 1e-10 {
            prop_assert!((result - 1.0).abs() < 1e-10, "at 1.0, reinforce should stay 1.0");
        }
    }

    /// lambda_for_type: result ≥ 0 for all variants
    #[test]
    fn prop_lambda_for_type_nonnegative(kt in knowledge_type_strategy()) {
        let lambda = lambda_for_type(&kt);
        prop_assert!(lambda >= 0.0, "lambda negative: {lambda}");
    }

    /// floor_for_type: result ∈ [0, 1]
    /// (1.0 sentinel for inert types is valid)
    #[test]
    fn prop_floor_for_type_in_range(kt in knowledge_type_strategy()) {
        let floor = floor_for_type(&kt);
        prop_assert!(floor >= 0.0, "floor negative: {floor}");
        prop_assert!(floor <= 1.0 + 1e-10, "floor > 1.0: {floor}");
    }

    /// base_level_to_salience: result ∈ [0, 1]
    #[test]
    fn prop_base_level_to_salience_in_range(b in -100.0f64..=100.0) {
        let result = base_level_to_salience(b);
        prop_assert!(result >= 0.0, "base_level_to_salience negative: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "base_level_to_salience > 1.0: {result}");
    }
}

// ── Property tests for attraction mechanics ──────────────────────────────────

proptest! {
    /// cosine_similarity: result ∈ [-1, 1] for non-zero vectors
    /// (clamped to [0, 1] by implementation)
    #[test]
    fn prop_cosine_similarity_in_bounds(
        a0 in -10.0f64..=10.0,
        a1 in -10.0f64..=10.0,
        b0 in -10.0f64..=10.0,
        b1 in -10.0f64..=10.0,
    ) {
        let result = cosine_similarity(&[a0, a1], &[b0, b1]);
        prop_assert!(result >= 0.0, "cosine_similarity negative: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "cosine_similarity > 1.0: {result}");
    }

    /// attraction_score: result ≥ 0
    #[test]
    fn prop_attraction_score_nonnegative(
        sim in 0.0f64..=1.0,
        tau in 0.0f64..=2.0,
        mass in 0.0f64..=1.0,
    ) {
        let result = attraction_score(sim, tau, mass);
        prop_assert!(result >= 0.0, "attraction_score negative: {result}");
    }

    /// strengthen_edge: result ≥ current, result ≤ 1.0
    #[test]
    fn prop_strengthen_edge_bounds(
        current in 0.0f64..=1.0,
        attraction in 0.0f64..=2.0,
    ) {
        let result = strengthen_edge(current, attraction);
        prop_assert!(result >= current - 1e-10, "strengthen decreased weight: {result} < {current}");
        prop_assert!(result <= 1.0 + 1e-10, "strengthen exceeded 1.0: {result}");
    }
}

// ── Property tests for gravity mechanics ──────────────────────────────────────

proptest! {
    /// compute_mass: result ∈ [0, 1]
    #[test]
    fn prop_compute_mass_in_bounds(
        salience in 0.0f64..=1.0,
        access_count in 0u32..=10000,
        kt in knowledge_type_strategy(),
    ) {
        let result = compute_mass(salience, access_count, &kt);
        prop_assert!(result >= 0.0, "compute_mass negative: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "compute_mass > 1.0: {result}");
    }

    /// gravity_boost: result ≥ 1.0 (boost is additive)
    #[test]
    fn prop_gravity_boost_at_least_one(mass in 0.0f64..=1.0) {
        let result = gravity_boost(mass);
        prop_assert!(result >= 1.0 - 1e-10, "gravity_boost < 1.0: {result}");
    }

    /// normalize_access_count: result ∈ [0, 1]
    #[test]
    fn prop_normalize_access_count_in_range(count in 0u32..=100000) {
        let result = normalize_access_count(count);
        prop_assert!(result >= 0.0, "normalize_access_count negative: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "normalize_access_count > 1.0: {result}");
    }
}

// ── Property tests for activation mechanics ──────────────────────────────────

proptest! {
    /// initial_activation: result ∈ [0, 1]
    #[test]
    fn prop_initial_activation_in_bounds(
        is_seed in any::<bool>(),
        vector_sim in 0.0f64..=1.0,
        identity_prior in 0.0f64..=1.0,
    ) {
        let result = initial_activation(is_seed, vector_sim, identity_prior);
        prop_assert!(result >= 0.0, "initial_activation negative: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "initial_activation > 1.0: {result}");
    }

    /// salience_gate: result ∈ [0.2, 1.0]
    #[test]
    fn prop_salience_gate_in_range(salience in 0.0f64..=1.0) {
        let result = salience_gate(salience);
        prop_assert!(result >= 0.2 - 1e-10, "salience_gate < 0.2: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "salience_gate > 1.0: {result}");
    }

    /// propagation_strength: result ≥ 0 when all inputs ≥ 0
    #[test]
    fn prop_propagation_strength_nonnegative(
        source_activation in 0.0f64..=1.0,
        edge_weight in 0.0f64..=1.0,
        kappa in 0.0f64..=2.0,
        hop_decay in 0.0f64..=1.0,
        target_salience_gate in 0.2f64..=1.0,
        target_gravity_boost in 1.0f64..=1.2,
    ) {
        let result = propagation_strength(
            source_activation,
            edge_weight,
            kappa,
            hop_decay,
            target_salience_gate,
            target_gravity_boost,
        );
        prop_assert!(result >= 0.0, "propagation_strength negative: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "propagation_strength > 1.0: {result}");
    }
}
