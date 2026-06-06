//! ACT-R base-level activation kernel (Anderson & Schooler 1991).
//!
//! This is the pure power-law base-level kernel `B_i = ln(Σⱼ (now − tⱼ)⁻ᵈ)`. Per
//! [ADR-0008](../../docs/adr/0008-powerlaw-dissipation.md) persistent node strength
//! decomposes as `A_i = B_i + P_i`: the base level `B_i` owns forgetting and
//! use-driven reinforcement and is the LIVE node strength term, while `P_i` (the
//! stored, decay-exempt `evidence_prior`) holds encoding surprise, feedback, and
//! peer trust. `B_i` is recomputed on demand from the node's access-trace history
//! (a creation trace plus each committed access, bounded to 32 traces); there is no
//! scalar reservoir that maintenance decays. Salience is the logistic projection of
//! the composite sum, `s_i = logistic(B_i + P_i)`.
//!
//! Aging is intrinsic: `B_i` ages every trace to `now` whenever it is read, so a
//! committed access appends a fresh trace inside the same sum that ages the prior
//! ones (decay-first by construction). See
//! [interactions.md](../../docs/04-cognitive-dynamics/interactions.md).

use crate::graph::Timestamp;
use std::collections::VecDeque;

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

    #[test]
    fn empty_history_is_neg_infinity() {
        let h: VecDeque<Timestamp> = VecDeque::new();
        let b = compute_base_level(&h, Timestamp(1000), 0.5);
        assert!(b.is_infinite() && b < 0.0);
    }

    #[test]
    fn single_access_act_r_exact() {
        let mut h = VecDeque::new();
        h.push_back(Timestamp(0));
        let now = Timestamp(7 * 24 * 3600 * 1000);
        let dt = now.0 as f64;
        let expected = dt.powf(-0.5).ln();
        let actual = compute_base_level(&h, now, 0.5);
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn more_recent_access_raises_base_level() {
        let mut old = VecDeque::new();
        old.push_back(Timestamp(0));
        let mut recent = VecDeque::new();
        recent.push_back(Timestamp(900_000));
        let now = Timestamp(1_000_000);
        assert!(compute_base_level(&recent, now, 0.5) > compute_base_level(&old, now, 0.5));
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
        let created = Timestamp(5_000);
        h.push_back(created);
        let b = compute_base_level(&h, created, 0.5);
        assert!(b.is_finite(), "fresh node B must be finite, got {b}");
        assert!(b.abs() < 1e-9, "fresh-at-now B should be ≈ 0, got {b}");
    }

    #[test]
    fn composite_salience_combines_base_level_and_prior() {
        // salience = logistic(B_i + P_i). A high decay-exempt prior keeps a node
        // salient at birth even though its base level is ≈ 0.
        let mut h = VecDeque::new();
        let now = Timestamp(1_000);
        h.push_back(now);
        let b = compute_base_level(&h, now, 0.5);
        let p = 13.8; // surprise-ceiling evidence prior
        let s = base_level_to_salience(b + p);
        assert!(
            s > 0.999,
            "high prior should land salience near 1.0, got {s}"
        );
    }
}
