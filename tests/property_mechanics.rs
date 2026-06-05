//! Property-based tests for pure mechanics functions.
//!
//! Tests invariants for pure functions in the mechanics and query modules
//! using proptest with 256 cases per test.

use anamnesis::graph::KnowledgeType;
use anamnesis::mechanics::attraction::{attraction_score, cosine_similarity, strengthen_edge};
use anamnesis::mechanics::forgetting::base_level_to_salience;
use anamnesis::mechanics::gravity::{compute_mass, gravity_boost, normalize_access_count};
use anamnesis::mechanics::interactions::{
    decay_default, hebbian_oja, reinforce_access, rescorla_wagner,
};
use anamnesis::mechanics::priors::{
    decay_multiplier_for_type, learning_rate, project_salience, project_weight,
    TARGET_COACTIVATION_N,
};
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

// ── Property tests for reservoir dynamics (Phase 2 substrate) ────────────────

proptest! {
    /// decay (power-law, log-odds): never increases A_i, stays finite, and is an
    /// identity for protected (zero-multiplier) types. There is NO [0,1] floor —
    /// decay operates on the unbounded retained-action reservoir.
    #[test]
    fn prop_decay_never_increases_action(
        action in -20.0f64..=20.0,
        dt_days in 0.0f64..=3650.0,
        kt in knowledge_type_strategy(),
    ) {
        let result = decay_default(action, dt_days, &kt);
        prop_assert!(result.is_finite(), "decay produced non-finite: {result}");
        prop_assert!(result <= action + 1e-9, "decay increased A: {result} > {action}");

        if decay_multiplier_for_type(&kt) == 0.0 {
            prop_assert!(
                (result - action).abs() < 1e-12,
                "protected type must not decay: {result} != {action}"
            );
        }
    }

    /// access gain: bounded saturating reinforcement — never lowers A, stays finite,
    /// and its projection never exceeds 1.0 (the Oja-style ceiling).
    #[test]
    fn prop_access_gain_bounded(action in -20.0f64..=20.0, work in 0.0f64..=2.0) {
        let eta = learning_rate(TARGET_COACTIVATION_N);
        let result = reinforce_access(action, work, eta);
        prop_assert!(result.is_finite());
        prop_assert!(result >= action - 1e-9, "access gain lowered A: {result} < {action}");
        prop_assert!(project_salience(result) <= 1.0 + 1e-12);
    }

    /// Rescorla-Wagner: always moves toward lambda (or stays), never overshoots,
    /// and stays finite.
    #[test]
    fn prop_rescorla_wagner_toward_target(
        action in -20.0f64..=20.0,
        lambda in -10.0f64..=10.0,
    ) {
        let eta = learning_rate(TARGET_COACTIVATION_N);
        let result = rescorla_wagner(action, lambda, eta);
        prop_assert!(result.is_finite());
        // The move is a fraction eta in (0,1) toward lambda: result is between
        // action and lambda (inclusive).
        let lo = action.min(lambda);
        let hi = action.max(lambda);
        prop_assert!(result >= lo - 1e-9 && result <= hi + 1e-9, "overshoot: {result}");
    }

    /// Hebbian-Oja: positive flux never lowers C, the projection stays below 1.0
    /// (no runaway), and the value stays finite.
    #[test]
    fn prop_hebbian_oja_bounded(conductance in -20.0f64..=20.0, flux in 0.0f64..=2.0) {
        let eta = learning_rate(TARGET_COACTIVATION_N);
        let result = hebbian_oja(conductance, flux, eta);
        prop_assert!(result.is_finite());
        prop_assert!(result >= conductance - 1e-9, "positive flux lowered C: {result}");
        prop_assert!(project_weight(result) < 1.0 + 1e-12);
    }

    /// decay_multiplier_for_type: result ∈ [0, 1] for all variants.
    #[test]
    fn prop_decay_multiplier_in_range(kt in knowledge_type_strategy()) {
        let m = decay_multiplier_for_type(&kt);
        prop_assert!((0.0..=1.0 + 1e-12).contains(&m), "multiplier out of range: {m}");
    }

    /// base_level_to_salience: result ∈ [0, 1].
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
