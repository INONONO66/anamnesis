//! Forgetting mechanics — salience decay and reinforcement.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equations
//! - (4) Decay: s(t+dt) = b + (s(t) - b) * exp(-lambda * dt_days)
//! - (5) Reinforcement: s <- clamp(s + 0.20 * (1 - s), 0, 1)

use crate::graph::KnowledgeType;
use crate::graph::Timestamp;
use std::collections::VecDeque;

/// Returns the per-day decay rate (lambda) for a knowledge type.
///
/// Higher lambda = faster decay. IdentityCore never decays (lambda = 0).
pub fn lambda_for_type(kt: &KnowledgeType) -> f64 {
    match kt {
        KnowledgeType::IdentityCore => 0.0,
        KnowledgeType::IdentityLearned => 0.005,
        KnowledgeType::IdentityState => 0.030,
        KnowledgeType::Convention | KnowledgeType::Decision => 0.015,
        KnowledgeType::Semantic | KnowledgeType::Procedural | KnowledgeType::Entity => 0.020,
        KnowledgeType::Episodic => 0.050,
        KnowledgeType::Event => 0.030,
        KnowledgeType::Gotcha => 0.020,
        KnowledgeType::Hypothesis | KnowledgeType::Evidence | KnowledgeType::DebugSession => 0.0,
        KnowledgeType::Custom(_) => 0.020,
    }
}

/// Adjusts a base decay rate using local graph topology signals.
///
/// Isolated nodes decay faster, while bridge nodes receive decay protection:
/// `lambda_eff = lambda_base * (1 + isolation_factor * is_orphan) * (1 - bridge_factor * bridge_score)`.
pub fn effective_lambda(
    lambda_base: f64,
    is_orphan: bool,
    bridge_score: f64,
    isolation_factor: f64,
    bridge_factor: f64,
) -> f64 {
    let orphan_indicator = if is_orphan { 1.0 } else { 0.0 };
    let isolation_multiplier = 1.0 + isolation_factor * orphan_indicator;
    let bridge_protection = 1.0 - bridge_factor * bridge_score;

    lambda_base * isolation_multiplier * bridge_protection
}

/// Returns the salience floor (minimum value) for a knowledge type.
///
/// Salience never decays below this floor. IdentityCore floor equals
/// the current salience (it never changes).
pub fn floor_for_type(kt: &KnowledgeType) -> f64 {
    match kt {
        KnowledgeType::IdentityCore => 1.0, // sentinel: handled specially in decay_salience
        KnowledgeType::IdentityLearned => 0.30,
        KnowledgeType::IdentityState => 0.10,
        KnowledgeType::Convention | KnowledgeType::Decision => 0.10,
        KnowledgeType::Semantic | KnowledgeType::Procedural | KnowledgeType::Entity => 0.02,
        KnowledgeType::Episodic => 0.00,
        KnowledgeType::Event => 0.02,
        KnowledgeType::Gotcha => 0.02,
        KnowledgeType::Hypothesis | KnowledgeType::Evidence | KnowledgeType::DebugSession => 1.0,
        KnowledgeType::Custom(_) => 0.02,
    }
}

/// Applies exponential decay to a salience value.
///
/// Equation (4): s(t+dt) = b + (s(t) - b) * exp(-lambda * dt_days)
///
/// - `current`: current salience [0, 1]
/// - `dt_days`: elapsed time in days (must be >= 0)
/// - `kt`: knowledge type (determines lambda and floor)
///
/// Returns the new salience, clamped to [floor, current].
/// IdentityCore nodes are never decayed (returns `current` unchanged).
pub fn decay_salience(current: f64, dt_days: f64, kt: &KnowledgeType) -> f64 {
    decay_salience_with_lambda(current, dt_days, kt, lambda_for_type(kt))
}

/// Applies exponential decay using an explicit decay rate.
///
/// This keeps [`decay_salience`] backwards-compatible while allowing callers to
/// supply topology-adjusted decay rates.
pub(crate) fn decay_salience_with_lambda(
    current: f64,
    dt_days: f64,
    kt: &KnowledgeType,
    lambda: f64,
) -> f64 {
    // IdentityCore never decays
    if matches!(kt, KnowledgeType::IdentityCore) {
        return current;
    }

    let floor = floor_for_type(kt);

    // No decay if lambda is zero or no time has passed
    if lambda == 0.0 || dt_days <= 0.0 {
        return current;
    }

    // If already at or below floor, no further decay possible
    if current <= floor {
        return current;
    }

    let decayed = floor + (current - floor) * (-lambda * dt_days).exp();
    // Clamp to [floor, current] — decay never increases salience
    decayed.clamp(floor, current)
}

/// Applies reinforcement boost to a salience value.
///
/// Equation (5): s <- clamp(s + 0.20 * (1 - s), 0, 1)
///
/// The `(1 - s)` factor creates diminishing returns: the closer to 1.0,
/// the smaller the boost. At s=1.0, the boost is 0.
pub fn reinforce_salience(current: f64) -> f64 {
    (current + 0.20 * (1.0 - current)).clamp(0.0, 1.0)
}

/// ACT-R base-level activation (Anderson & Schooler 1991).
///
/// B = ln(Σⱼ tⱼ⁻ᵈ) where d is the decay parameter (typically 0.5).
///
/// Each tⱼ is the elapsed time in milliseconds since the j-th access.
/// Returns negative infinity when access_history is empty (no activation).
/// Result is not clamped — can be any real number including negative.
pub fn compute_base_level(
    access_history: &VecDeque<Timestamp>,
    now: Timestamp,
    decay_d: f64,
) -> f64 {
    if access_history.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = access_history
        .iter()
        .map(|&t| {
            let dt = now.0.saturating_sub(t.0).max(1) as f64;
            dt.powf(-decay_d)
        })
        .sum();
    sum.ln()
}

/// Map ACT-R base-level activation to salience in [0, 1].
///
/// Uses sigmoid: σ(b) = 1 / (1 + exp(-b)).
/// - B = −∞ → 0.0  (no activation)
/// - B = 0  → 0.5  (neutral)
/// - B → +∞ → 1.0  (fully active)
pub fn base_level_to_salience(b: f64) -> f64 {
    if b.is_infinite() && b < 0.0 {
        return 0.0;
    }
    1.0 / (1.0 + (-b).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Deterministic tests ──────────────────────────────────────────────────

    #[test]
    fn identity_core_never_decays() {
        let s = 0.8;
        assert_eq!(decay_salience(s, 365.0, &KnowledgeType::IdentityCore), s);
        assert_eq!(decay_salience(s, 0.0, &KnowledgeType::IdentityCore), s);
    }

    #[test]
    fn decay_with_zero_dt_returns_current() {
        for kt in [
            KnowledgeType::Semantic,
            KnowledgeType::Episodic,
            KnowledgeType::IdentityLearned,
        ] {
            assert_eq!(decay_salience(0.8, 0.0, &kt), 0.8);
        }
    }

    #[test]
    fn episodic_decays_faster_than_semantic() {
        let s = 1.0;
        let dt = 14.0; // 2 weeks
        let episodic = decay_salience(s, dt, &KnowledgeType::Episodic);
        let semantic = decay_salience(s, dt, &KnowledgeType::Semantic);
        assert!(
            episodic < semantic,
            "episodic={episodic}, semantic={semantic}"
        );
    }

    #[test]
    fn decay_approaches_floor_over_time() {
        let s = 1.0;
        let floor = floor_for_type(&KnowledgeType::Episodic);
        let decayed = decay_salience(s, 365.0, &KnowledgeType::Episodic);
        // After a year, should be very close to floor
        assert!(decayed < 0.05, "expected near floor, got {decayed}");
        assert!(decayed >= floor);
    }

    #[test]
    fn identity_learned_floor_is_respected() {
        let floor = floor_for_type(&KnowledgeType::IdentityLearned);
        let decayed = decay_salience(floor + 0.01, 10000.0, &KnowledgeType::IdentityLearned);
        assert!(decayed >= floor, "decayed below floor: {decayed} < {floor}");
    }

    #[test]
    fn reinforce_at_zero_gives_point_two() {
        let result = reinforce_salience(0.0);
        assert!((result - 0.20).abs() < 1e-10);
    }

    #[test]
    fn reinforce_at_one_returns_one() {
        assert_eq!(reinforce_salience(1.0), 1.0);
    }

    #[test]
    fn reinforce_at_half_gives_point_six() {
        let result = reinforce_salience(0.5);
        assert!((result - 0.60).abs() < 1e-10);
    }

    #[test]
    fn reinforce_is_diminishing() {
        // Boost at s=0.1 should be larger than boost at s=0.9
        let boost_low = reinforce_salience(0.1) - 0.1;
        let boost_high = reinforce_salience(0.9) - 0.9;
        assert!(
            boost_low > boost_high,
            "boost_low={boost_low}, boost_high={boost_high}"
        );
    }

    #[test]
    fn lambda_values_match_architecture() {
        assert_eq!(lambda_for_type(&KnowledgeType::IdentityCore), 0.0);
        assert_eq!(lambda_for_type(&KnowledgeType::IdentityLearned), 0.005);
        assert_eq!(lambda_for_type(&KnowledgeType::Episodic), 0.050);
        assert_eq!(lambda_for_type(&KnowledgeType::Semantic), 0.020);
        assert_eq!(lambda_for_type(&KnowledgeType::Convention), 0.015);
    }

    // ── Property tests ───────────────────────────────────────────────────────

    proptest! {
        #[test]
        fn decay_output_in_bounds(
            s in 0.0f64..=1.0,
            dt in 0.0f64..=365.0,
        ) {
            let kt = KnowledgeType::Semantic;
            let result = decay_salience(s, dt, &kt);
            let floor = floor_for_type(&kt);
            if s >= floor {
                prop_assert!(result >= floor, "result {result} below floor {floor}");
            } else {
                prop_assert!(result == s, "below-floor input should be unchanged: {result} != {s}");
            }
            prop_assert!(result <= s + 1e-10, "result {result} exceeds input {s}");
        }

        #[test]
        fn decay_never_increases(
            s in 0.0f64..=1.0,
            dt in 0.001f64..=365.0,
        ) {
            let result = decay_salience(s, dt, &KnowledgeType::Episodic);
            prop_assert!(result <= s + 1e-10, "decay increased salience: {result} > {s}");
        }

        #[test]
        fn reinforce_output_in_bounds(s in 0.0f64..=1.0) {
            let result = reinforce_salience(s);
            prop_assert!(result >= s - 1e-10, "reinforce decreased salience: {result} < {s}");
            prop_assert!(result <= 1.0 + 1e-10, "reinforce exceeded 1.0: {result}");
        }

        #[test]
        fn reinforce_never_decreases(s in 0.0f64..=1.0) {
            let result = reinforce_salience(s);
            prop_assert!(result >= s - 1e-10);
        }
    }
}
