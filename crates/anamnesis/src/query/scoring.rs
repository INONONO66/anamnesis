//! Readout scoring — the authoritative additive log-odds ranking.
//!
//! Readout turns the settled activation response into ranked, budgeted output. The
//! score is the seven-term additive log-odds form of
//! [readout-scoring.md](../../docs/04-cognitive-dynamics/readout-scoring.md):
//!
//! ```text
//! readout_score_i =
//!     w_a     * logit(a_i)
//!   + w_phi   * phi_i
//!   + w_s     * logit(s_i)
//!   - w_z     * Z_i
//!   + w_scope * scope_weight_i
//!   + w_trust * trust_weight_i
//!   - w_stress * stress_i
//! ```
//!
//! It reads as a posterior log-odds (`posterior = prior + sum of evidence`). The
//! seven coefficients are one calibrated re-ranking regression object; the default
//! is unit coefficients, which recovers the plain additive log-odds sum. All inputs
//! are query-local; scoring **never mutates storage** (ADR-0002).

use std::cmp::Ordering;

use crate::graph::ScopePath;
use crate::graph::{NodeId, Timestamp};
use crate::mechanics::priors::{
    READOUT_W_A, READOUT_W_PHI, READOUT_W_S, READOUT_W_SCOPE, READOUT_W_STRESS, READOUT_W_TRUST,
    READOUT_W_Z,
};

/// Computes the scope weight for a node relative to the query context.
///
/// Scopes are flat opaque paths (the hierarchy was removed — all production
/// nodes are universal). The weight is a two-branch visibility gate:
/// - identical scopes, or either scope universal: `1.0` (fully visible);
/// - two different concrete scopes: `0.5` (attenuated).
///
/// In production the query scope is always universal, so this always yields
/// `1.0` — bit-identical to the previous hierarchical table's `Equal`/`Universal`
/// rows for the universal-query case.
pub fn scope_weight(query_scope: &ScopePath, node_scope: &ScopePath) -> f64 {
    if query_scope == node_scope || query_scope.is_universal() || node_scope.is_universal() {
        1.0
    } else {
        0.5
    }
}

/// The per-site inputs to the readout score (readout-scoring.md input signals).
#[derive(Debug, Clone, Copy)]
pub struct ReadoutInputs {
    /// Query-local activation response `a_i` (probability-like, in `[0, 1]`).
    pub activation: f64,
    /// Potential bias `phi_i` from the query field (log-odds units).
    pub phi: f64,
    /// Salience projection `s_i` in `(0, 1)`.
    pub salience: f64,
    /// Effective impedance `Z_i` (access cost; subtracted).
    pub impedance: f64,
    /// Scope compatibility `scope_weight_i`.
    pub scope_weight: f64,
    /// Origin/peer reliability `trust_weight_i`.
    pub trust_weight: f64,
    /// Frustration `stress_i` attached to selected contradictions (subtracted).
    pub stress: f64,
}

impl Default for ReadoutInputs {
    fn default() -> Self {
        ReadoutInputs {
            activation: 0.0,
            phi: 0.0,
            salience: 0.5,
            impedance: 0.0,
            scope_weight: 1.0,
            trust_weight: 0.0,
            stress: 0.0,
        }
    }
}

/// Computes the authoritative seven-term additive log-odds readout score.
///
/// The activation term uses `logit(a_i)`; `s_i` enters
/// as `logit(s_i)`; `phi_i` and the scope/trust terms enter linearly; impedance and
/// stress are subtracted. Inputs are clamped to keep the logits finite.
pub fn readout_score(input: &ReadoutInputs) -> f64 {
    let a_term = READOUT_W_A * logit(clamp_prob(input.activation));
    let phi_term = READOUT_W_PHI * finite(input.phi);
    let s_term = READOUT_W_S * logit(clamp_prob(input.salience));
    let z_term = READOUT_W_Z * finite(input.impedance);
    let scope_term = READOUT_W_SCOPE * finite(input.scope_weight);
    let trust_term = READOUT_W_TRUST * finite(input.trust_weight);
    let stress_term = READOUT_W_STRESS * finite(input.stress);

    a_term + phi_term + s_term - z_term + scope_term + trust_term - stress_term
}

/// Deterministic tie-break key for two candidates with equal readout score
/// (readout-scoring.md ordering stability):
///
/// 1. higher retained action `A_i`,
/// 2. lower impedance `Z_i`,
/// 3. more recent committed access,
/// 4. stable node id.
///
/// Returns the ordering placing the *preferred* candidate first (descending).
pub fn tie_break(a: &TieBreakKey, b: &TieBreakKey) -> Ordering {
    // Higher retained action first.
    cmp_f64_desc(a.retained_action, b.retained_action)
        // Lower impedance first.
        .then_with(|| cmp_f64_asc(a.impedance, b.impedance))
        // More recent access first.
        .then_with(|| b.accessed_at.cmp(&a.accessed_at))
        // Stable node id.
        .then_with(|| a.node_id.0.cmp(&b.node_id.0))
}

/// Deterministic tie-breaker fields for a candidate.
#[derive(Debug, Clone, Copy)]
pub struct TieBreakKey {
    pub node_id: NodeId,
    pub retained_action: f64,
    pub impedance: f64,
    pub accessed_at: Timestamp,
}

/// Orders two candidates by readout score (descending), then by the deterministic
/// tie-breaker chain. The preferred candidate sorts first.
pub fn rank(score_a: f64, key_a: &TieBreakKey, score_b: f64, key_b: &TieBreakKey) -> Ordering {
    cmp_f64_desc(score_a, score_b).then_with(|| tie_break(key_a, key_b))
}

fn logit(p: f64) -> f64 {
    (p / (1.0 - p)).ln()
}

fn clamp_prob(p: f64) -> f64 {
    let eps = crate::mechanics::priors::LOGIT_BACKFILL_EPS;
    if p.is_finite() {
        p.clamp(eps, 1.0 - eps)
    } else {
        0.5
    }
}

fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

fn cmp_f64_desc(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

fn cmp_f64_asc(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn proj(s: &str) -> ScopePath {
        ScopePath::new(s).expect("valid scope")
    }

    // ── scope weight (2-branch: equal-or-universal ⇒ 1.0, else 0.5) ────────────

    #[test]
    fn same_scope_full_weight() {
        assert_eq!(scope_weight(&proj("proj-a"), &proj("proj-a")), 1.0);
    }

    /// PRODUCTION INVARIANT: every production node is universal and the query
    /// scope is always universal, so the weight must be exactly 1.0 — bit-identical
    /// to the pre-shrink hierarchical table (which returned `Equal` ⇒ 1.0 for the
    /// universal-vs-universal case). This pins the shrink's zero-behavior-change
    /// guarantee for the only scope pair that occurs in production.
    #[test]
    fn universal_query_and_universal_node_is_exactly_one() {
        let u = ScopePath::universal();
        assert_eq!(scope_weight(&u, &u), 1.0);
    }

    #[test]
    fn either_universal_full_weight() {
        // Universal on either side ⇒ fully visible (1.0).
        assert_eq!(scope_weight(&proj("proj-a"), &ScopePath::universal()), 1.0);
        assert_eq!(scope_weight(&ScopePath::universal(), &proj("proj-a")), 1.0);
    }

    #[test]
    fn different_concrete_scopes_attenuated() {
        // Two different concrete scopes ⇒ attenuated (0.5), regardless of any
        // former hierarchical relationship (ancestor/sibling/disjoint all collapse).
        assert_eq!(scope_weight(&proj("proj-a"), &proj("proj-b")), 0.5);
        assert_eq!(scope_weight(&proj("proj-a"), &proj("proj-a/feature")), 0.5);
        assert_eq!(scope_weight(&proj("proj-a/x"), &proj("proj-a/y")), 0.5);
    }

    // ── readout score ────────────────────────────────────────────────────────

    #[test]
    fn additive_log_odds_with_calibrated_coefficients() {
        // The score is the weighted additive sum of terms under the current
        // calibrated regression object (calibration-records.md 2026-06-11).
        let input = ReadoutInputs {
            activation: 0.5,
            phi: 1.0,
            salience: 0.5,
            impedance: 0.0,
            scope_weight: 1.0,
            trust_weight: 0.0,
            stress: 0.0,
        };
        // logit(0.5) = 0 twice; score = W_PHI*phi + W_SCOPE*scope.
        let expected = READOUT_W_PHI + READOUT_W_SCOPE;
        assert!((readout_score(&input) - expected).abs() < 1e-9);
    }

    #[test]
    fn higher_activation_increases_score() {
        let low = ReadoutInputs {
            activation: 0.2,
            ..Default::default()
        };
        let high = ReadoutInputs {
            activation: 0.8,
            ..Default::default()
        };
        assert!(readout_score(&high) > readout_score(&low));
    }

    #[test]
    fn impedance_term_calibrated_off_but_form_preserved() {
        // `Z_i = -ln(a_i)` duplicates the activation signal, so calibration
        // set `w_z = 0` (calibration-records.md 2026-06-11). The subtractive
        // term stays in the form; with the current coefficient it is inert.
        let base = ReadoutInputs::default();
        let impeded = ReadoutInputs {
            impedance: 3.0,
            ..Default::default()
        };
        let delta = readout_score(&base) - readout_score(&impeded);
        assert!(
            (delta - READOUT_W_Z * 3.0).abs() < 1e-12,
            "impedance must subtract exactly w_z * Z_i, got delta {delta}"
        );
    }

    #[test]
    fn stress_penalizes() {
        let base = ReadoutInputs::default();
        let stressed = ReadoutInputs {
            stress: 2.0,
            ..Default::default()
        };
        assert!(readout_score(&stressed) < readout_score(&base));
    }

    // ── tie-breakers ──────────────────────────────────────────────────────────

    #[test]
    fn tie_break_prefers_higher_retained_action() {
        let a = TieBreakKey {
            node_id: NodeId(5),
            retained_action: 2.0,
            impedance: 1.0,
            accessed_at: Timestamp(0),
        };
        let b = TieBreakKey {
            node_id: NodeId(1),
            retained_action: 1.0,
            impedance: 1.0,
            accessed_at: Timestamp(0),
        };
        assert_eq!(tie_break(&a, &b), Ordering::Less); // a preferred (sorts first)
    }

    #[test]
    fn tie_break_then_lower_impedance() {
        let a = TieBreakKey {
            node_id: NodeId(5),
            retained_action: 1.0,
            impedance: 0.5,
            accessed_at: Timestamp(0),
        };
        let b = TieBreakKey {
            node_id: NodeId(1),
            retained_action: 1.0,
            impedance: 2.0,
            accessed_at: Timestamp(0),
        };
        assert_eq!(tie_break(&a, &b), Ordering::Less);
    }

    #[test]
    fn tie_break_then_node_id() {
        let a = TieBreakKey {
            node_id: NodeId(1),
            retained_action: 1.0,
            impedance: 1.0,
            accessed_at: Timestamp(10),
        };
        let b = TieBreakKey {
            node_id: NodeId(2),
            retained_action: 1.0,
            impedance: 1.0,
            accessed_at: Timestamp(10),
        };
        assert_eq!(tie_break(&a, &b), Ordering::Less);
    }

    proptest! {
        #[test]
        fn readout_score_finite(
            activation in 0.0f64..=1.0,
            phi in -10.0f64..=10.0,
            salience in 0.0f64..=1.0,
            impedance in 0.0f64..=40.0,
            scope_weight in 0.0f64..=1.0,
            trust_weight in 0.0f64..=1.0,
            stress in 0.0f64..=10.0,
        ) {
            let score = readout_score(&ReadoutInputs {
                activation, phi, salience, impedance, scope_weight, trust_weight, stress,
            });
            prop_assert!(score.is_finite(), "score not finite: {score}");
        }
    }
}
