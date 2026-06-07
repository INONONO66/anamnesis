//! Perception mechanics — the two-stage observation gate.
//!
//! Per [perception.md](../../docs/04-cognitive-dynamics/perception.md) and
//! [ADR-0009](../../docs/adr/0009-surprise-gated-perception.md), perception decides
//! what an external observation is allowed to change when it enters the graph.
//! Two principles define the gate:
//!
//! 1. **Familiarity is not rejection.** Repeated knowledge *routes* to an existing
//!    site and reinforces it. The gate blocks untrusted input and unacceptable cost,
//!    not similarity by itself.
//! 2. **Initial charge comes from surprise.** A newly allocated site's retained
//!    action is proportional to precision-weighted prediction error (`dA = k*eps`),
//!    so duplicates and noise no longer enter as strong as belief-changing inputs.
//!
//! The gate has two stages:
//!
//! - **Stage 1 (reject only):** low confidence rejects; budget rejects *only* when
//!   the node budget is full **and** the input is not novel.
//! - **Stage 2 (route survivors):** novelty `> theta_sep` ⇒ `Allocate` a new site
//!   with surprise-gated charge; novelty `<= theta_sep` ⇒ `Route` to and reinforce
//!   the nearest site. Stage 2 never rejects.
//!
//! All functions are pure: no side effects, no storage access.

use crate::mechanics::priors;

/// Outcome of the two-stage perception gate (perception.md decision table).
#[derive(Debug, Clone, PartialEq)]
pub enum PerceptionDecision {
    /// Stage 1 rejected the observation. State change: none.
    Reject(RejectReason),
    /// Stage 2: novelty `> theta_sep`. Allocate a new site whose initial retained
    /// action receives the surprise-gated charge `dA = k * eps` (log-odds units).
    Allocate {
        /// Novelty `1 - max_similarity` that crossed `theta_sep`.
        novelty: f64,
        /// Surprise-gated initial retained-action charge `dA_i = k * eps`.
        surprise_charge: f64,
    },
    /// Stage 2: novelty `<= theta_sep`. Route to and reinforce the nearest site;
    /// no new site is created. Carries the novelty for diagnostics.
    Route { novelty: f64 },
}

/// Stage-1 rejection reasons (perception.md rejection trace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Origin confidence failed stage 1.
    LowConfidence,
    /// Node budget is full and the input is not novel.
    BudgetExceeded,
    /// Required fields are missing / inputs are non-finite.
    MalformedObservation,
}

impl RejectReason {
    /// Stable machine-readable tag (perception.md rejection trace).
    pub fn as_str(&self) -> &'static str {
        match self {
            RejectReason::LowConfidence => "low_confidence",
            RejectReason::BudgetExceeded => "budget_exceeded",
            RejectReason::MalformedObservation => "malformed_observation",
        }
    }
}

/// Runs the two-stage perception gate (perception.md).
///
/// # Parameters
/// - `confidence`: origin confidence `[0, 1]`.
/// - `confidence_threshold`: minimum required confidence (stage 1).
/// - `current_node_count` / `max_nodes`: node budget (stage 1).
/// - `max_similarity`: highest cosine similarity to any candidate site (`0.0` if no
///   candidates). Novelty is `1 - max_similarity`.
/// - `theta_sep`: the separation boundary, derived from the encoder distinct-pair
///   q95 via [`priors::theta_sep`].
/// - `surprise_charge`: the precomputed `dA = k * eps` for the allocate branch
///   (see [`surprise_charge`]).
///
/// Stage 1 is the only place rejection happens. Similarity alone is never a
/// rejection reason — familiar input routes and reinforces.
pub fn gate(
    confidence: f64,
    confidence_threshold: f64,
    current_node_count: usize,
    max_nodes: usize,
    max_similarity: f64,
    theta_sep: f64,
    surprise_charge: f64,
) -> PerceptionDecision {
    if !confidence.is_finite()
        || !confidence_threshold.is_finite()
        || !max_similarity.is_finite()
        || !theta_sep.is_finite()
    {
        return PerceptionDecision::Reject(RejectReason::MalformedObservation);
    }

    let novelty = (1.0 - max_similarity).clamp(0.0, 1.0);
    let is_novel = novelty > theta_sep;

    // ── Stage 1: rejection ────────────────────────────────────────────────────
    if confidence < confidence_threshold {
        return PerceptionDecision::Reject(RejectReason::LowConfidence);
    }
    // Budget rejects only when FULL and NOT novel — a novel input may still enter
    // (perception.md: "guard only when full and not novel").
    if current_node_count >= max_nodes && !is_novel {
        return PerceptionDecision::Reject(RejectReason::BudgetExceeded);
    }

    // ── Stage 2: routing (never rejects) ───────────────────────────────────────
    if is_novel {
        let charge = if surprise_charge.is_finite() {
            surprise_charge
        } else {
            0.0
        };
        PerceptionDecision::Allocate {
            novelty,
            surprise_charge: charge,
        }
    } else {
        PerceptionDecision::Route { novelty }
    }
}

/// Bayesian-surprise proxy `eps` for an allocate decision (perception.md, ADR-0009).
///
/// ```text
/// eps = (obs - pred)^T Sigma^-1 (obs - pred)
/// ```
///
/// `eps` is the precision-weighted (Mahalanobis) embedding prediction error between
/// the observation and the graph's nearest prediction. Anamnesis has no explicit
/// generative model, so literal KL is approximated by this precision-weighted error.
///
/// When no precision matrix `Sigma` is available (the common case), this falls back
/// to the **isotropic** estimate `Sigma = I`, i.e. the squared Euclidean distance
/// `||obs - pred||^2`. For unit-norm embeddings this equals `2 * (1 - cosine)`, so
/// the prediction here is the nearest candidate site's embedding. The result is
/// clamped to a finite, non-negative value.
///
/// `precision_diag` is an optional per-dimension precision vector (the diagonal of
/// `Sigma^-1`); `None` selects the isotropic fallback.
pub fn bayesian_surprise(
    observed: &[f64],
    predicted: &[f64],
    precision_diag: Option<&[f64]>,
) -> f64 {
    if observed.is_empty() || observed.len() != predicted.len() {
        return 0.0;
    }
    let eps = match precision_diag {
        Some(prec) if prec.len() == observed.len() => observed
            .iter()
            .zip(predicted.iter())
            .zip(prec.iter())
            .map(|((o, p), w)| {
                let d = o - p;
                gate_finite(*w) * d * d
            })
            .sum::<f64>(),
        // Isotropic fallback: Sigma = I → squared Euclidean distance.
        _ => observed
            .iter()
            .zip(predicted.iter())
            .map(|(o, p)| {
                let d = o - p;
                d * d
            })
            .sum::<f64>(),
    };
    if eps.is_finite() { eps.max(0.0) } else { 0.0 }
}

/// Surprise-gated initial charge `dA_i = k * eps` (perception.md, ADR-0009).
///
/// `k` is the single calibrated surprise gain ([`priors::SURPRISE_GAIN_K`]) that
/// converts the surprise proxy `eps` into initial retained action (log need-odds).
/// This avoids the white-snow paradox: high Shannon information that does not change
/// belief receives little charge; inputs far from prior expectation receive more.
/// The result is finite-clamped to the reservoir bound.
pub fn surprise_charge(eps: f64) -> f64 {
    let eps = if eps.is_finite() { eps.max(0.0) } else { 0.0 };
    (priors::SURPRISE_GAIN_K * eps).clamp(0.0, priors::LOG_ODDS_CLAMP)
}

#[inline]
fn gate_finite(v: f64) -> f64 {
    if v.is_finite() { v.max(0.0) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theta() -> f64 {
        priors::theta_sep(priors::ENCODER_DISTINCT_PAIR_Q95)
    }

    // ── stage 1: rejection ─────────────────────────────────────────────────────

    #[test]
    fn low_confidence_rejected() {
        let d = gate(0.3, 0.5, 10, 100, 0.0, theta(), 1.0);
        assert_eq!(d, PerceptionDecision::Reject(RejectReason::LowConfidence));
    }

    #[test]
    fn budget_full_and_not_novel_rejected() {
        // max_similarity 0.99 → novelty 0.01 < theta_sep → not novel; budget full.
        let d = gate(0.9, 0.5, 100, 100, 0.99, theta(), 1.0);
        assert_eq!(d, PerceptionDecision::Reject(RejectReason::BudgetExceeded));
    }

    #[test]
    fn budget_full_but_novel_is_not_rejected() {
        // Novel input may still enter even when the budget is full.
        let d = gate(0.9, 0.5, 100, 100, 0.0, theta(), 1.0);
        assert!(matches!(d, PerceptionDecision::Allocate { .. }));
    }

    #[test]
    fn nan_inputs_malformed() {
        assert_eq!(
            gate(f64::NAN, 0.5, 0, 100, 0.0, theta(), 1.0),
            PerceptionDecision::Reject(RejectReason::MalformedObservation)
        );
    }

    // ── stage 2: routing ───────────────────────────────────────────────────────

    #[test]
    fn high_novelty_allocates() {
        // No similar site → novelty 1.0 > theta_sep → allocate.
        let d = gate(0.9, 0.5, 0, 100, 0.0, theta(), 5.0);
        match d {
            PerceptionDecision::Allocate {
                novelty,
                surprise_charge,
            } => {
                assert!((novelty - 1.0).abs() < 1e-12);
                assert_eq!(surprise_charge, 5.0);
            }
            other => panic!("expected Allocate, got {other:?}"),
        }
    }

    #[test]
    fn low_novelty_routes_not_rejects() {
        // The old "low novelty means reject" rule is removed: familiar input routes.
        // max_similarity 0.95 → novelty 0.05 <= theta_sep (0.30) → route.
        let d = gate(0.9, 0.5, 10, 100, 0.95, theta(), 1.0);
        assert!(matches!(d, PerceptionDecision::Route { .. }));
    }

    #[test]
    fn similarity_alone_never_rejects() {
        // Even at extreme similarity with budget available, the result is a Route,
        // never a Reject.
        let d = gate(0.9, 0.5, 10, 100, 1.0, theta(), 1.0);
        assert!(matches!(d, PerceptionDecision::Route { .. }));
    }

    // ── surprise ────────────────────────────────────────────────────────────────

    #[test]
    fn isotropic_surprise_is_squared_distance() {
        let eps = bayesian_surprise(&[1.0, 0.0], &[0.0, 0.0], None);
        assert!((eps - 1.0).abs() < 1e-12);
        let eps2 = bayesian_surprise(&[1.0, 1.0], &[0.0, 0.0], None);
        assert!((eps2 - 2.0).abs() < 1e-12);
    }

    #[test]
    fn precision_weighting_scales_surprise() {
        let iso = bayesian_surprise(&[1.0, 0.0], &[0.0, 0.0], None);
        let weighted = bayesian_surprise(&[1.0, 0.0], &[0.0, 0.0], Some(&[4.0, 1.0]));
        assert!((weighted - 4.0 * iso).abs() < 1e-12);
    }

    #[test]
    fn zero_surprise_for_identical() {
        assert_eq!(bayesian_surprise(&[1.0, 2.0], &[1.0, 2.0], None), 0.0);
    }

    #[test]
    fn more_surprising_input_gets_higher_charge() {
        let near = surprise_charge(bayesian_surprise(&[0.1, 0.0], &[0.0, 0.0], None));
        let far = surprise_charge(bayesian_surprise(&[1.0, 0.0], &[0.0, 0.0], None));
        assert!(far > near, "far={far} should exceed near={near}");
    }

    #[test]
    fn surprise_charge_is_bounded_and_finite() {
        let c = surprise_charge(1e300);
        assert!(c.is_finite() && c <= priors::LOG_ODDS_CLAMP);
        assert_eq!(surprise_charge(f64::NAN), 0.0);
    }

    #[test]
    fn mismatched_lengths_zero_surprise() {
        assert_eq!(bayesian_surprise(&[1.0, 2.0], &[1.0], None), 0.0);
        assert_eq!(bayesian_surprise(&[], &[], None), 0.0);
    }
}
