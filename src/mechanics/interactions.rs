//! Interaction dynamics on the authoritative reservoirs `A_i` and `C_ij`.
//!
//! Per [interactions.md](../../docs/04-cognitive-dynamics/interactions.md) and
//! [ADR-0002](../../docs/adr/0002-reservoir-projection-state.md), interactions are
//! the *only* path that mutates persistent cognitive quantities. Every function
//! here operates on the unbounded log-odds / log-LR reservoirs — never on the
//! bounded `salience`/`weight` projections. The projection is recomputed *after*
//! the reservoir moves (the caller does that via `project_salience`/`project_weight`).
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Reservoir units
//!
//! - `A_i` retained action = log need-odds (unbounded). `salience = project_salience(A_i)`.
//! - `C_ij` conductance = log likelihood ratio (unbounded). `weight = project_weight(C_ij)`.
//!
//! ## The single learning rate
//!
//! Reinforcement uses one core rate `eta = 1 - 0.5^(1/N)` derived from the target
//! co-activation count `N` ([`crate::mechanics::priors::learning_rate`]). The same
//! `eta` drives Rescorla-Wagner feedback, Hebbian-Oja conductance, and the
//! saturating `access_gain` (conductance.md / interactions.md).

use crate::graph::KnowledgeType;
use crate::mechanics::priors::{
    self, DECAY_EXPONENT_D, LOG_ODDS_CLAMP, decay_multiplier_for_type, project_weight,
};

/// Finite-guard for a reservoir value: clamps to `[-LOG_ODDS_CLAMP, LOG_ODDS_CLAMP]`.
///
/// This is the numerical-safety trap (not a `[0, 1]` bound — the reservoirs are
/// unbounded log-odds). Non-finite inputs are rejected at the engine boundary, so
/// this only contains finite blowups well inside `f64` range.
#[inline]
fn clamp_reservoir(value: f64) -> f64 {
    value.clamp(-LOG_ODDS_CLAMP, LOG_ODDS_CLAMP)
}

/// `TimeElapsed` — power-law dissipation of retained action in log-odds space.
///
/// Implements `A_i' = decay(A_i, delta_days, node_type, d)` (dissipation.md,
/// [ADR-0008](../../docs/adr/0008-powerlaw-dissipation.md)). The ACT-R base-level
/// power-law form `t^-d` is re-targeted onto the reservoir: aging by `delta_days`
/// shifts the log need-odds by the power-law term
///
/// ```text
/// A_i' = A_i - (d * type_mult) * ln(1 + delta_days)
/// ```
///
/// where `ln(1 + delta_days)` is the log-odds image of the ACT-R `t^-d` kernel
/// (`ln((1 + delta)^-d) = -d * ln(1 + delta)`) and `type_mult` is the per-`node_type`
/// policy multiplier on the single free decay prior `d`. There is **no `[0, 1]`
/// floor** on this reservoir path — flooring belongs to the projection, never the
/// reservoir. Decay only ever lowers `A_i` (monotonic non-increasing); the protected
/// case (`type_mult == 0`) returns `A_i` unchanged. Result is finite-clamped.
///
/// `delta_days <= 0` (no elapsed time) returns `A_i` unchanged.
pub fn decay(retained_action: f64, delta_days: f64, node_type: &KnowledgeType, d: f64) -> f64 {
    let type_mult = decay_multiplier_for_type(node_type);
    let effective_d = d * type_mult;
    if effective_d <= 0.0 || delta_days <= 0.0 {
        return retained_action;
    }
    let shift = effective_d * (1.0 + delta_days).ln();
    clamp_reservoir(retained_action - shift)
}

/// `decay` with the canonical decay prior `d` ([`DECAY_EXPONENT_D`]).
#[inline]
pub fn decay_default(retained_action: f64, delta_days: f64, node_type: &KnowledgeType) -> f64 {
    decay(retained_action, delta_days, node_type, DECAY_EXPONENT_D)
}

/// `Accessed` — bounded saturating access gain on retained action.
///
/// `A_next = A_after_decay + access_gain(readout_work)` (interactions.md). The gain
/// is bounded and saturating in the projection so repeated access cannot drive
/// retained action past its ceiling: it is the same Oja-bounded family as the
/// Hebbian update, applied to `A_i`'s own projection.
///
/// ```text
/// access_gain = eta * readout_work * (1 - project_salience(A))
/// ```
///
/// `readout_work` is the (non-negative) work delivered by the committed access,
/// normally in `[0, 1]`. As `project_salience(A) -> 1` the gain -> 0, so the
/// reservoir saturates. Returns the *new* reservoir (decay must already be applied).
pub fn reinforce_access(retained_action: f64, readout_work: f64, eta: f64) -> f64 {
    let work = readout_work.max(0.0);
    let headroom = 1.0 - priors::project_salience(retained_action);
    clamp_reservoir(retained_action + eta * work * headroom)
}

/// `FeedbackReceived` — Rescorla-Wagner prediction-error update on retained action.
///
/// `dA_i = eta * (lambda - A_i)` (interactions.md). `lambda` is the reward target
/// in the same log-odds units as `A_i`; already-well-predicted sites (those near
/// `lambda`) move less. Negative feedback (`lambda < A_i`) lowers retained action.
/// Returns `A_i + dA_i`, finite-clamped.
pub fn rescorla_wagner(retained_action: f64, lambda: f64, eta: f64) -> f64 {
    clamp_reservoir(retained_action + eta * (lambda - retained_action))
}

/// Map a consumer [`crate::FeedbackSignal`] to a Rescorla-Wagner reward target `lambda`
/// in log-odds units ([`crate::mechanics::priors::REWARD_LOG_ODDS_SCALE`]).
///
/// CALIBRATED PRIOR mapping — a `Useful` signal of strength `s` sets the target to
/// `+s * scale` (high need-odds); `NotUseful`/`Incorrect` set `-s * scale`. The
/// Rescorla-Wagner step then moves `A_i` a fraction `eta` of the way toward this
/// target, so well-predicted sites move less and provenance/content are untouched.
pub fn lambda_reward(signal: &crate::mechanics::social::FeedbackSignal) -> f64 {
    signal.signed_strength() * priors::REWARD_LOG_ODDS_SCALE
}

/// `CoReadout` / `PathUsed` — bounded Hebbian-Oja conductance update.
///
/// `dC_ij = eta * flux_ij * (1 - project_weight(C_ij))` (conductance.md /
/// interactions.md). The `(1 - project_weight(C))` term is the Oja bound realized
/// on the *projection* (migration design Decision 5): as `project_weight(C) -> 1`
/// the update -> 0, preventing raw Hebbian runaway / hub explosion, while `C`
/// itself stays in unbounded log-LR units. `flux_ij` is committed path current or
/// co-readout activation. Returns the new conductance reservoir, finite-clamped.
pub fn hebbian_oja(conductance: f64, flux: f64, eta: f64) -> f64 {
    let bound = 1.0 - project_weight(conductance);
    clamp_reservoir(conductance + eta * flux * bound)
}

/// Per-edge idle leakage amount `idle_edge_leakage_ij` (conductance.md
/// post-commit plasticity term `- eta_leak * idle_edge_leakage_ij`).
///
/// This is the leak *magnitude* before the `eta_leak` rate is applied. It is the
/// product of two factors:
///
/// ```text
/// idle_edge_leakage(C_ij, idle_days) = project_weight(C_ij) * ln(1 + idle_days)
/// ```
///
/// - `project_weight(C_ij)` is the current bounded coupling strength. Scaling by
///   it realizes the density-control goal "remove unused weak coupling over time"
///   (conductance.md / dissipation.md): the leak is proportional to present
///   coupling, so an idle path's weight drains toward zero rather than crossing
///   into negative log-LR runaway, and a long-saturated path resists more slowly.
/// - `ln(1 + idle_days)` is the same power-law idle kernel used for node decay
///   ([`decay`]): the log-odds image of the ACT-R `t^-d` form, so a freshly used
///   edge (`idle_days <= 0`) leaks **nothing** and leakage grows sub-linearly with
///   idle time.
///
/// Non-positive / non-finite `idle_days` returns `0.0` (no leak). The result is
/// always finite and non-negative.
#[inline]
pub fn idle_edge_leakage(conductance: f64, idle_days: f64) -> f64 {
    if !idle_days.is_finite() || idle_days <= 0.0 {
        return 0.0;
    }
    let coupling = project_weight(conductance).clamp(0.0, 1.0);
    coupling * (1.0 + idle_days).ln()
}

/// `TimeElapsed` — idle-edge conductance leakage (interactions.md
/// `C_ij' = leak_idle_edge(C_ij, idle_days)`; conductance.md
/// `- eta_leak * idle_edge_leakage_ij`).
///
/// Applies the leak to the authoritative conductance reservoir:
///
/// ```text
/// C_ij' = C_ij - eta_leak * idle_edge_leakage(C_ij, idle_days)
/// ```
///
/// This is the conductance analogue of node [`decay`]: time is an interaction and
/// unused reservoirs leak (interactions.md). Leakage only ever lowers `C_ij`
/// (monotonic non-increasing); a freshly used edge (`idle_days <= 0`) or a
/// disabled rate (`eta_leak <= 0`) returns `C_ij` unchanged. The caller re-projects
/// `weight = project_weight(C_ij')`. Result is finite-clamped.
pub fn leak_idle_edge(conductance: f64, idle_days: f64, eta_leak: f64) -> f64 {
    if eta_leak <= 0.0 || !eta_leak.is_finite() {
        return conductance;
    }
    let leak = eta_leak * idle_edge_leakage(conductance, idle_days);
    clamp_reservoir(conductance - leak)
}

/// [`leak_idle_edge`] with the canonical idle-edge leak rate
/// ([`crate::mechanics::priors::ETA_LEAK`]).
#[inline]
pub fn leak_idle_edge_default(conductance: f64, idle_days: f64) -> f64 {
    leak_idle_edge(conductance, idle_days, priors::ETA_LEAK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mechanics::priors::{TARGET_COACTIVATION_N, learning_rate, project_salience};
    use proptest::prelude::*;

    fn eta() -> f64 {
        learning_rate(TARGET_COACTIVATION_N)
    }

    // ── decay (power-law, log-odds, no floor) ─────────────────────────────────

    #[test]
    fn decay_zero_delta_is_identity() {
        for kt in [
            KnowledgeType::Semantic,
            KnowledgeType::Episodic,
            KnowledgeType::IdentityCore,
        ] {
            assert_eq!(decay_default(0.7, 0.0, &kt), 0.7);
        }
    }

    #[test]
    fn decay_protected_types_never_change() {
        for kt in [
            KnowledgeType::IdentityCore,
            KnowledgeType::Hypothesis,
            KnowledgeType::Evidence,
            KnowledgeType::DebugSession,
        ] {
            assert_eq!(decay_default(2.0, 365.0, &kt), 2.0);
        }
    }

    #[test]
    fn decay_is_monotonic_non_increasing_and_unfloored() {
        // A negative reservoir keeps going more negative — no [0,1] floor on the
        // reservoir path (this is the whole point of Phase 2).
        let a0 = -3.0;
        let a1 = decay_default(a0, 30.0, &KnowledgeType::Episodic);
        assert!(a1 < a0, "decay must lower A even below zero: {a1} !< {a0}");
    }

    #[test]
    fn decay_episodic_faster_than_semantic() {
        let a = 1.0;
        let dt = 14.0;
        let episodic = decay_default(a, dt, &KnowledgeType::Episodic);
        let semantic = decay_default(a, dt, &KnowledgeType::Semantic);
        assert!(episodic < semantic, "ep={episodic} sem={semantic}");
    }

    #[test]
    fn decay_power_law_shift_exact() {
        // A' = A - d*mult*ln(1+delta_days) for Episodic (mult = 1.0).
        let a = 0.5_f64;
        let delta = 9.0_f64;
        let expected = a - DECAY_EXPONENT_D * (1.0 + delta).ln();
        let got = decay_default(a, delta, &KnowledgeType::Episodic);
        assert!((got - expected).abs() < 1e-12, "{got} != {expected}");
    }

    // ── access gain (bounded, saturating) ─────────────────────────────────────

    #[test]
    fn access_gain_saturates_and_stays_bounded() {
        // Bounded saturating: each step's gain shrinks as project_salience(A) -> 1
        // (headroom 1 - project_salience(A) -> 0). The projection climbs toward 1
        // but never reaches it, and A stays finite — no runaway past the ceiling.
        let mut a = 0.0;
        let mut prev_gain = f64::INFINITY;
        for _ in 0..1000 {
            let next = reinforce_access(a, 1.0, eta());
            let gain = next - a;
            assert!(gain >= 0.0, "gain must be non-negative");
            assert!(
                gain <= prev_gain + 1e-12,
                "gain must diminish monotonically"
            );
            prev_gain = gain;
            a = next;
        }
        assert!(a.is_finite());
        assert!(project_salience(a) < 1.0, "projection must never reach 1");
        assert!(
            project_salience(a) > 0.9,
            "but should climb high: {}",
            project_salience(a)
        );
    }

    #[test]
    fn access_gain_is_non_negative_movement() {
        let a = -1.0;
        let next = reinforce_access(a, 0.5, eta());
        assert!(next >= a, "access gain should not lower A: {next} < {a}");
    }

    #[test]
    fn access_gain_zero_work_is_identity() {
        assert_eq!(reinforce_access(0.3, 0.0, eta()), 0.3);
    }

    // ── Rescorla-Wagner ───────────────────────────────────────────────────────

    #[test]
    fn rescorla_wagner_moves_toward_lambda() {
        let a = 0.0;
        let lambda = 4.0;
        let next = rescorla_wagner(a, lambda, eta());
        assert!(next > a && next < lambda, "should move partway: {next}");
    }

    #[test]
    fn rescorla_wagner_well_predicted_moves_less() {
        let lambda = 4.0;
        let far = rescorla_wagner(0.0, lambda, eta()) - 0.0;
        let near = rescorla_wagner(3.5, lambda, eta()) - 3.5;
        assert!(
            far > near,
            "well-predicted should move less: {far} vs {near}"
        );
    }

    #[test]
    fn rescorla_wagner_negative_feedback_lowers() {
        let a = 2.0;
        let next = rescorla_wagner(a, -2.0, eta());
        assert!(next < a, "negative feedback should lower A: {next}");
    }

    // ── Hebbian-Oja conductance ───────────────────────────────────────────────

    #[test]
    fn hebbian_oja_saturates_no_runaway() {
        let mut c = 0.0;
        for _ in 0..10_000 {
            c = hebbian_oja(c, 1.0, eta());
        }
        assert!(c.is_finite(), "must not run away: {c}");
        assert!(project_weight(c) < 1.0);
    }

    #[test]
    fn hebbian_oja_zero_flux_is_identity() {
        assert_eq!(hebbian_oja(0.4, 0.0, eta()), 0.4);
    }

    #[test]
    fn hebbian_oja_increases_with_positive_flux() {
        let c = 0.0;
        let next = hebbian_oja(c, 0.5, eta());
        assert!(next > c, "positive flux should raise C: {next}");
    }

    // ── idle-edge leakage (TimeElapsed on conductance) ────────────────────────

    use crate::mechanics::priors::ETA_LEAK;

    #[test]
    fn leak_idle_edge_recently_used_unchanged() {
        // idle_days <= 0 → freshly used edge leaks nothing.
        let c = 1.0;
        assert_eq!(leak_idle_edge(c, 0.0, ETA_LEAK), c);
        assert_eq!(leak_idle_edge(c, -5.0, ETA_LEAK), c);
        assert_eq!(leak_idle_edge_default(c, 0.0), c);
    }

    #[test]
    fn leak_idle_edge_idle_loses_conductance() {
        // An idle edge with positive coupling must lose conductance.
        let c = 1.0;
        let leaked = leak_idle_edge_default(c, 30.0);
        assert!(leaked < c, "idle edge must leak: {leaked} !< {c}");
        // And the weight projection drops too.
        assert!(project_weight(leaked) < project_weight(c));
    }

    #[test]
    fn leak_idle_edge_is_monotonic_non_increasing() {
        // More idle time leaks at least as much (never raises C).
        let c = 1.5;
        let a = leak_idle_edge_default(c, 10.0);
        let b = leak_idle_edge_default(c, 100.0);
        assert!(a <= c && b <= a, "expected b={b} <= a={a} <= c={c}");
    }

    #[test]
    fn leak_idle_edge_disabled_rate_is_identity() {
        let c = 0.8;
        assert_eq!(leak_idle_edge(c, 365.0, 0.0), c);
        assert_eq!(leak_idle_edge(c, 365.0, -1.0), c);
    }

    #[test]
    fn idle_edge_leakage_zero_when_fresh() {
        assert_eq!(idle_edge_leakage(2.0, 0.0), 0.0);
        assert_eq!(idle_edge_leakage(2.0, f64::NAN), 0.0);
    }

    #[test]
    fn idle_edge_leakage_bounded_and_finite() {
        // Leak magnitude is non-negative, finite, and grows with idle time.
        let near = idle_edge_leakage(1.0, 1.0);
        let far = idle_edge_leakage(1.0, 1000.0);
        assert!(near >= 0.0 && near.is_finite());
        assert!(far >= near, "leak should grow with idle time");
        assert!(leak_idle_edge_default(1.0, 100_000.0).is_finite());
    }

    #[test]
    fn leak_weaker_coupling_drains_toward_zero_weight() {
        // A weak edge under sustained idle leaks toward (but not past, in any
        // runaway sense) a low weight; result stays finite.
        let mut c = 0.2; // weak coupling
        for _ in 0..200 {
            c = leak_idle_edge_default(c, 30.0);
        }
        assert!(c.is_finite());
        assert!(project_weight(c) < project_weight(0.2));
    }

    // ── eta derivation ────────────────────────────────────────────────────────

    #[test]
    fn eta_reaches_half_saturation_after_n_full_flux() {
        // After N full-flux Hebbian updates from C=0, project_weight(C) should be
        // near the 0.5 saturation target the eta was derived for.
        let n = TARGET_COACTIVATION_N;
        let e = learning_rate(n);
        let mut c = 0.0;
        for _ in 0..(n as usize) {
            c = hebbian_oja(c, 1.0, e);
        }
        let w = project_weight(c);
        // Oja bound on the projection: w climbs toward 0.5 in N steps.
        assert!(w > 0.4 && w < 0.6, "w after N steps = {w}");
    }

    // ── property: reservoirs stay finite ──────────────────────────────────────

    proptest! {
        #[test]
        fn decay_never_increases_action(
            a in -20.0f64..=20.0,
            dt in 0.0f64..=3650.0,
        ) {
            let result = decay_default(a, dt, &KnowledgeType::Episodic);
            prop_assert!(result <= a + 1e-9, "decay raised A: {result} > {a}");
            prop_assert!(result.is_finite());
        }

        #[test]
        fn all_updates_stay_finite(
            a in -20.0f64..=20.0,
            x in -2.0f64..=2.0,
        ) {
            let e = eta();
            prop_assert!(reinforce_access(a, x.abs(), e).is_finite());
            prop_assert!(rescorla_wagner(a, x, e).is_finite());
            prop_assert!(hebbian_oja(a, x, e).is_finite());
        }

        #[test]
        fn leak_never_increases_conductance(
            c in -20.0f64..=20.0,
            idle in 0.0f64..=3650.0,
        ) {
            let result = leak_idle_edge_default(c, idle);
            prop_assert!(result <= c + 1e-9, "leak raised C: {result} > {c}");
            prop_assert!(result.is_finite());
        }
    }
}
