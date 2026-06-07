//! Property-based tests for pure mechanics functions.
//!
//! Tests invariants for pure functions in the mechanics and query modules
//! using proptest with 256 cases per test.

use anamnesis::graph::{AccessTrace, KnowledgeType, Timestamp};
use anamnesis::mechanics::attraction::{attraction_score, cosine_similarity};
use anamnesis::mechanics::forgetting::{base_level_to_salience, compute_base_level};
use anamnesis::mechanics::interactions::{hebbian_oja, rescorla_wagner};
use anamnesis::mechanics::priors::{
    DECAY_INTERCEPT, TARGET_COACTIVATION_N, decay_multiplier_for_type, learning_rate,
    project_weight,
};
use anamnesis::query::field::{FieldSignals, potential_bias};
use anamnesis::query::scoring::{ReadoutInputs, readout_score};
use proptest::prelude::*;
use std::collections::VecDeque;

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

// ── Property tests for the base-level forgetting kernel (B_i, ADR-0008) ──────

proptest! {
    /// compute_base_level (B_i): for a fixed access trace, the base level is
    /// monotone NON-INCREASING in elapsed time (forgetting), stays finite for a
    /// non-empty history, and is independent of elapsed time when the decay
    /// exponent is 0 (protected / inert types).
    #[test]
    fn prop_base_level_decays_with_time(
        created_ms in 0u64..=1_000_000,
        later in 1u64..=10_000_000,
        more in 1u64..=10_000_000,
        kt in knowledge_type_strategy(),
    ) {
        // A single creation trace carries the floor per-trace decay d_j = m_type·α
        // (Pavlik & Anderson 2005); m_type = 0 ⇒ d_j = 0 (protected / inert types).
        let decay_d = decay_multiplier_for_type(&kt) * DECAY_INTERCEPT;
        let mut history = VecDeque::new();
        history.push_back(AccessTrace {
            at: Timestamp(created_ms),
            decay: decay_d,
        });
        let now1 = Timestamp(created_ms + later);
        let now2 = Timestamp(created_ms + later + more);
        let b1 = compute_base_level(&history, now1);
        let b2 = compute_base_level(&history, now2);
        prop_assert!(b1.is_finite() && b2.is_finite());
        if decay_d == 0.0 {
            // Exponent 0 → dt^0 = 1 for every trace; B_i is time-invariant.
            prop_assert!((b1 - b2).abs() < 1e-12, "inert type changed: {b1} vs {b2}");
        } else {
            // More elapsed time never raises the base level (forgetting).
            prop_assert!(b2 <= b1 + 1e-9, "older read raised B_i: {b2} > {b1}");
        }
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
