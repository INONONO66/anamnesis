//! ACT-R base-level activation kernel with activation-dependent per-trace decay
//! (Anderson & Schooler 1991; Pavlik & Anderson 2005).
//!
//! This is the multi-trace base-level kernel `B_i = ln(Σⱼ (now − atⱼ)^(−dⱼ))`. Per
//! [ADR-0008](../../docs/adr/0008-powerlaw-dissipation.md) persistent node strength
//! decomposes as `A_i = B_i + P_i`: the base level `B_i` owns forgetting and
//! use-driven reinforcement and is the LIVE node strength term, while `P_i` (the
//! stored, decay-exempt `evidence_prior`) holds encoding surprise, feedback, and
//! peer trust. `B_i` is recomputed on demand from the node's access-trace history
//! (a creation trace plus each committed access, bounded to 32 traces); there is no
//! scalar reservoir that maintenance decays. Salience is the logistic projection of
//! the composite sum, `s_i = logistic(B_i + P_i)`.
//!
//! Each trace carries its OWN decay rate `dⱼ` ([`crate::graph::AccessTrace`]),
//! computed ONCE at the moment the trace is laid down from the activation `mⱼ` of
//! the EXISTING traces: `dⱼ = m_type·(c·e^{mⱼ} + α)` (Pavlik & Anderson 2005,
//! [`compute_trace_decay`]). A trace laid down on a strongly active node decays
//! faster, which is what produces the genuine spacing effect.
//!
//! Aging is intrinsic: `B_i` ages every trace to `now` whenever it is read, so a
//! committed access appends a fresh trace inside the same sum that ages the prior
//! ones (decay-first by construction). See
//! [interactions.md](../../docs/04-cognitive-dynamics/interactions.md).

use crate::graph::{AccessTrace, Timestamp};
use std::collections::VecDeque;

/// Multi-trace ACT-R base-level activation with per-trace decay (Anderson &
/// Schooler 1991; Pavlik & Anderson 2005).
///
/// `B = ln(Σⱼ (now − atⱼ)^(−dⱼ))` where each `dⱼ` is the trace's own stored decay
/// rate. Each `(now − atⱼ)` is the elapsed time in milliseconds since the j-th
/// access, floored to `max(1)` ms.
///
/// Returns negative infinity when `access_history` is empty (no activation).
/// Result is not clamped — can be any real number including negative.
pub fn compute_base_level(access_history: &VecDeque<AccessTrace>, now: Timestamp) -> f64 {
    if access_history.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = access_history
        .iter()
        .map(|trace| {
            let dt = now.0.saturating_sub(trace.at.0).max(1) as f64;
            dt.powf(-trace.decay)
        })
        .sum();
    sum.ln()
}

/// Activation-dependent per-trace decay `d_j` (Pavlik & Anderson 2005).
///
/// Computed ONCE at the moment a trace is laid down (at time `now`) from the
/// activation `m` of the EXISTING traces:
/// - empty history (the creation / first trace) ⇒ `m = −∞`, `e^m = 0`, so the
///   floor `d_j = m_type · intercept_alpha` applies;
/// - otherwise `m = ln(Σ_existing (now − at_k)^(−d_k))` and
///   `d_j = m_type · (scale_c · e^m + intercept_alpha)`.
///
/// `m_type` ([`crate::mechanics::priors::decay_multiplier_for_type`]) is the OUTER
/// multiplier, so a decay-exempt type (`m_type = 0`) yields `d_j = 0` (permanent).
/// `scale_c` and `intercept_alpha` are the locked
/// [`crate::mechanics::priors::DECAY_SCALE`] / [`crate::mechanics::priors::DECAY_INTERCEPT`].
pub fn compute_trace_decay(
    existing: &VecDeque<AccessTrace>,
    now: Timestamp,
    m_type: f64,
    scale_c: f64,
    intercept_alpha: f64,
) -> f64 {
    if existing.is_empty() {
        // Creation / first-trace floor: e^{−∞} = 0 ⇒ d_j = m_type·α.
        return m_type * intercept_alpha;
    }
    let activation_sum: f64 = existing
        .iter()
        .map(|trace| {
            let dt = now.0.saturating_sub(trace.at.0).max(1) as f64;
            dt.powf(-trace.decay)
        })
        .sum();
    let m = activation_sum.ln();
    m_type * (scale_c * m.exp() + intercept_alpha)
}

/// Map ACT-R base-level activation to a bounded value in [0, 1].
///
/// Uses sigmoid: σ(b) = 1 / (1 + exp(-b)). This is the same logistic form used by
/// `project_salience`, applied to the base-level activation `B` directly.
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
    use crate::mechanics::priors::{DECAY_INTERCEPT, DECAY_SCALE};

    /// Helper: a single trace at `at` with decay rate `d`.
    fn trace(at: u64, d: f64) -> AccessTrace {
        AccessTrace {
            at: Timestamp(at),
            decay: d,
        }
    }

    #[test]
    fn empty_history_is_neg_infinity() {
        let h: VecDeque<AccessTrace> = VecDeque::new();
        let b = compute_base_level(&h, Timestamp(1000));
        assert!(b.is_infinite() && b < 0.0);
    }

    #[test]
    fn single_access_act_r_exact() {
        let mut h = VecDeque::new();
        h.push_back(trace(0, 0.5));
        let now = Timestamp(7 * 24 * 3600 * 1000);
        let dt = now.0 as f64;
        let expected = dt.powf(-0.5).ln();
        let actual = compute_base_level(&h, now);
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn more_recent_access_raises_base_level() {
        let mut old = VecDeque::new();
        old.push_back(trace(0, 0.5));
        let mut recent = VecDeque::new();
        recent.push_back(trace(900_000, 0.5));
        let now = Timestamp(1_000_000);
        assert!(compute_base_level(&recent, now) > compute_base_level(&old, now));
    }

    #[test]
    fn base_level_to_salience_in_unit_range() {
        assert_eq!(base_level_to_salience(f64::NEG_INFINITY), 0.0);
        assert!((base_level_to_salience(0.0) - 0.5).abs() < 1e-9);
        assert!(base_level_to_salience(20.0) > 0.99);
        assert!(base_level_to_salience(-20.0) < 0.01);
    }

    #[test]
    fn creation_trace_makes_fresh_node_finite() {
        // A freshly created node seeds a creation trace at `created_at`, so its
        // base level is finite (not NEG_INFINITY) even before any access. With the
        // creation trace stamped at `now` itself, dt floors to 1ms and B ≈ ln(1) = 0.
        let mut h = VecDeque::new();
        let created = 5_000;
        h.push_back(trace(created, 0.5 * DECAY_INTERCEPT));
        let b = compute_base_level(&h, Timestamp(created));
        assert!(b.is_finite(), "fresh node B must be finite, got {b}");
        assert!(b.abs() < 1e-9, "fresh-at-now B should be ≈ 0, got {b}");
    }

    #[test]
    fn composite_salience_combines_base_level_and_prior() {
        // salience = logistic(B_i + P_i). A high decay-exempt prior keeps a node
        // salient at birth even though its base level is ≈ 0.
        let mut h = VecDeque::new();
        let now = Timestamp(1_000);
        h.push_back(trace(now.0, 0.4 * DECAY_INTERCEPT));
        let b = compute_base_level(&h, now);
        let p = 13.8; // surprise-ceiling evidence prior
        let s = base_level_to_salience(b + p);
        assert!(
            s > 0.999,
            "high prior should land salience near 1.0, got {s}"
        );
    }

    // ── Activation-dependent per-trace decay (Pavlik & Anderson 2005) ─────────

    #[test]
    fn empty_history_trace_decay_is_floor() {
        // The creation / first trace has no existing activation: e^{−∞} = 0 ⇒
        // d_j = m_type·α exactly (the locked floor).
        let h: VecDeque<AccessTrace> = VecDeque::new();
        let m_type = 0.40; // Semantic
        let d = compute_trace_decay(&h, Timestamp(0), m_type, DECAY_SCALE, DECAY_INTERCEPT);
        assert!(
            (d - m_type * DECAY_INTERCEPT).abs() < 1e-12,
            "creation decay must be m_type·α = {}, got {d}",
            m_type * DECAY_INTERCEPT
        );
    }

    #[test]
    fn core_type_trace_decay_is_zero() {
        // m_type = 0 ⇒ d_j = 0 for every trace (permanent, no decay).
        let mut h = VecDeque::new();
        h.push_back(trace(0, 0.0));
        let d_empty = compute_trace_decay(
            &VecDeque::new(),
            Timestamp(1000),
            0.0,
            DECAY_SCALE,
            DECAY_INTERCEPT,
        );
        let d_nonempty =
            compute_trace_decay(&h, Timestamp(1000), 0.0, DECAY_SCALE, DECAY_INTERCEPT);
        assert_eq!(d_empty, 0.0);
        assert_eq!(d_nonempty, 0.0);
    }

    #[test]
    fn core_type_base_level_constant_in_time() {
        // With every trace decay = 0, B = ln(Σ_j dt^0) = ln(n) regardless of `now`.
        let mut h = VecDeque::new();
        h.push_back(trace(0, 0.0));
        h.push_back(trace(1_000, 0.0));
        h.push_back(trace(2_000, 0.0));
        let n = h.len() as f64;
        for now in [
            Timestamp(3_000),
            Timestamp(10_000_000),
            Timestamp(u64::MAX / 2),
        ] {
            let b = compute_base_level(&h, now);
            assert!(
                (b - n.ln()).abs() < 1e-12,
                "Core B must equal ln(n)={} at now={now:?}, got {b}",
                n.ln()
            );
        }
    }

    #[test]
    fn single_trace_semantic_slope_is_minus_m_type_alpha() {
        // A single Semantic creation trace (decay = m_type·α) gives a perfectly
        // log-linear forgetting curve B = −d·ln(dt), so the slope of B against
        // ln(dt) is exactly −d = −m_type·α = −0.16.
        let m_type = 0.40_f64; // Semantic
        let d = m_type * DECAY_INTERCEPT; // 0.16
        let mut h = VecDeque::new();
        h.push_back(trace(0, d));
        // Two probe times; slope = ΔB / Δln(dt).
        let dt1 = 10_000.0_f64;
        let dt2 = 1_000_000.0_f64;
        let b1 = compute_base_level(&h, Timestamp(dt1 as u64));
        let b2 = compute_base_level(&h, Timestamp(dt2 as u64));
        let slope = (b2 - b1) / (dt2.ln() - dt1.ln());
        assert!(
            (slope + 0.16).abs() < 1e-9,
            "single-trace Semantic slope must be −0.16, got {slope}"
        );
    }

    #[test]
    fn second_trace_decays_faster_than_creation_floor() {
        // A second trace laid down while the first is still active sees m > −∞, so
        // its decay exceeds the m_type·α floor (activation-dependent acceleration).
        let m_type = 0.40_f64;
        let floor = m_type * DECAY_INTERCEPT;
        let mut h = VecDeque::new();
        h.push_back(trace(0, floor));
        let now = Timestamp(1_000); // first trace still active
        let d2 = compute_trace_decay(&h, now, m_type, DECAY_SCALE, DECAY_INTERCEPT);
        assert!(
            d2 > floor,
            "active-node trace decay {d2} must exceed floor {floor}"
        );
    }
}
