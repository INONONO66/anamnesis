//! Property-based tests for pure mechanics functions.
//!
//! Tests invariants for pure functions in the mechanics and query modules
//! using proptest with 256 cases per test.

use anamnesis::graph::KnowledgeType;
use anamnesis::mechanics::attraction::{attraction_score, cosine_similarity};
use anamnesis::mechanics::forgetting::base_level_to_salience;
use anamnesis::mechanics::interactions::{
    decay_default, hebbian_oja, reinforce_access, rescorla_wagner,
};
use anamnesis::mechanics::priors::{
    TARGET_COACTIVATION_N, decay_multiplier_for_type, learning_rate, project_salience,
    project_weight,
};
use anamnesis::query::field::{FieldSignals, potential_bias};
use anamnesis::query::scoring::{ReadoutInputs, readout_score};
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

    /// attraction_score: result ≥ 0. It is the candidate-selection affinity
    /// `sigma_ij * tau` only — no mass / gravity term (importance is emergent,
    /// overview.md / conductance.md).
    #[test]
    fn prop_attraction_score_nonnegative(
        sim in 0.0f64..=1.0,
        tau in 0.0f64..=2.0,
    ) {
        let result = attraction_score(sim, tau);
        prop_assert!(result >= 0.0, "attraction_score negative: {result}");
    }
}

// ── Property tests for the additive-RWR readout / potential field ───────────

proptest! {
    /// potential_bias: finite for finite inputs; A_i enters with unit coefficient.
    #[test]
    fn prop_potential_bias_finite(
        text in -5.0f64..=5.0,
        embed in -5.0f64..=5.0,
        retained_action in -20.0f64..=20.0,
    ) {
        let phi = potential_bias(&FieldSignals {
            text_score: text,
            embedding_score: embed,
            retained_action,
            ..Default::default()
        });
        prop_assert!(phi.is_finite(), "phi not finite: {phi}");
    }

    /// readout_score: finite for bounded inputs (log-odds additive form).
    #[test]
    fn prop_readout_score_finite(
        activation in 0.0f64..=1.0,
        phi in -10.0f64..=10.0,
        salience in 0.0f64..=1.0,
        impedance in 0.0f64..=40.0,
        stress in 0.0f64..=10.0,
    ) {
        let score = readout_score(&ReadoutInputs {
            activation, phi, salience, impedance,
            scope_weight: 1.0, trust_weight: 0.0, stress,
        });
        prop_assert!(score.is_finite(), "readout_score not finite: {score}");
    }
}
