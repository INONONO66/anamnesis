//! Frustration mechanics — query-local contradiction stress.
//!
//! Per [frustration.md](../../docs/04-cognitive-dynamics/frustration.md) and
//! [ADR-0006](../../docs/adr/0006-frustration-not-deletion.md), contradiction is
//! **surfaced as stress, never suppressed and never deleted**. There is no
//! exponential activation damping and no rigidity term: a `Contradicts` pair keeps
//! its full activation and is simply *reported* as a tension whose stress raises the
//! readout energy (the `-w_stress` term), encouraging conflicting bundles to
//! separate without judging either side true.
//!
//! Stress is a pure multiplicative product of gates:
//!
//! ```text
//! sigma_ij = contradiction_weight_ij * min(a_i, a_j) * scope_overlap * temporal_overlap
//! ```
//!
//! Each factor is a gate. If either endpoint is inactive, the scopes do not overlap,
//! or the facts are not valid together, the corresponding gate is `0` and so is the
//! stress. All functions are pure: no side effects, no storage access.

use crate::graph::Timestamp;
use crate::graph::scope::ScopePath;

/// Scope-overlap gate `scope_overlap` for a contradiction pair (frustration.md).
///
/// Two claims can only frustrate each other when their scopes actually overlap.
/// Scopes are flat opaque paths (the hierarchy was removed — all production nodes
/// are universal), so the gate is a two-branch **safety** gate in `{0.0, 1.0}`:
/// identical or universal scopes overlap fully (`1.0`); two different concrete
/// scopes do not overlap at all (`0.0`), so a private contradiction cannot leak
/// across unauthorized scopes (the frustration.md safety rule). This is the
/// conservative closed-gate choice: unlike the readout `scope_weight` (a ranking
/// weight that only *attenuates*), this gate must *hide* cross-scope tensions, not
/// merely down-weight them. In production the compared scopes are always
/// universal, so this always yields `1.0` — bit-identical to the previous
/// hierarchical table's `Equal`/`Universal` rows for that case.
pub fn scope_overlap(a: &ScopePath, b: &ScopePath) -> f64 {
    if a == b || a.is_universal() || b.is_universal() {
        1.0
    } else {
        0.0
    }
}

/// Temporal-overlap gate `temporal_overlap` for a contradiction pair (frustration.md).
///
/// Two facts can only frustrate each other when they are valid *together* at the
/// query time. The gate is `1.0` when both endpoints' half-open validity intervals
/// `[valid_from, valid_until)` contain `as_of`, and `0.0` otherwise — a
/// time-filtered query returns only stress valid at that time. This routes through
/// the single canonical [`crate::graph::valid_at`] predicate so the temporal
/// semantics never diverge.
pub fn temporal_overlap(
    a_valid_from: Option<Timestamp>,
    a_valid_until: Option<Timestamp>,
    b_valid_from: Option<Timestamp>,
    b_valid_until: Option<Timestamp>,
    as_of: Timestamp,
) -> f64 {
    let a_valid = crate::graph::valid_at(a_valid_from, a_valid_until, as_of);
    let b_valid = crate::graph::valid_at(b_valid_from, b_valid_until, as_of);
    if a_valid && b_valid { 1.0 } else { 0.0 }
}

/// Query-local frustration stress `sigma_ij` between an active contradiction pair.
///
/// ```text
/// sigma_ij = contradiction_weight * min(a_i, a_j) * scope_overlap * temporal_overlap
/// ```
///
/// This is a product of gates: if either endpoint is inactive (`a_i` or `a_j` is
/// `0`), if the scopes do not overlap, or if the facts are not valid together, the
/// stress is exactly `0`. Stress is non-negative; it never reduces or deletes
/// activation — the conflict is surfaced, not suppressed (ADR-0006). Non-finite
/// inputs collapse the corresponding gate to `0`.
pub fn stress(
    contradiction_weight: f64,
    activation_i: f64,
    activation_j: f64,
    scope_overlap: f64,
    temporal_overlap: f64,
) -> f64 {
    let cw = gate(contradiction_weight);
    // Guard each activation before `min`: `NaN.min(x)` returns `x` in Rust, which
    // would silently leak a non-finite endpoint past the gate.
    let min_active = gate(activation_i).min(gate(activation_j));
    let scope = gate(scope_overlap);
    let temporal = gate(temporal_overlap);
    let sigma = cw * min_active * scope * temporal;
    if sigma.is_finite() {
        sigma.max(0.0)
    } else {
        0.0
    }
}

/// `S_frustration(i, j) = tension_presented_ij * sigma_ij` — the commit-time stress
/// flux recorded when a caller actually presents/uses a surfaced tension
/// (frustration.md, `TensionActivated`). Read-only retrieval records nothing; this
/// only scales the query-local stress by how strongly the tension was presented.
pub fn tension_flux(tension_presented: f64, sigma: f64) -> f64 {
    let presented = gate(tension_presented);
    let sigma = gate(sigma);
    let flux = presented * sigma;
    if flux.is_finite() { flux.max(0.0) } else { 0.0 }
}

/// Clamps a gate factor to a finite, non-negative value (`NaN`/`Inf` → `0`).
#[inline]
fn gate(v: f64) -> f64 {
    if v.is_finite() { v.max(0.0) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn scope(s: &str) -> ScopePath {
        ScopePath::new(s).expect("valid scope")
    }

    // ── scope overlap gate ────────────────────────────────────────────────────

    #[test]
    fn same_scope_full_overlap() {
        assert_eq!(scope_overlap(&scope("proj-a"), &scope("proj-a")), 1.0);
    }

    #[test]
    fn universal_full_overlap() {
        assert_eq!(
            scope_overlap(&scope("proj-a"), &ScopePath::universal()),
            1.0
        );
    }

    #[test]
    fn different_concrete_scopes_no_overlap() {
        // Two different concrete scopes (neither universal) do not overlap — the
        // safety gate is fully closed so private contradictions never leak,
        // regardless of any former hierarchical relationship (the hierarchy is gone).
        assert_eq!(scope_overlap(&scope("proj-a"), &scope("proj-b")), 0.0);
        assert_eq!(
            scope_overlap(&scope("proj-a"), &scope("proj-a/feature")),
            0.0
        );
    }

    // ── temporal overlap gate ─────────────────────────────────────────────────

    #[test]
    fn both_valid_full_temporal_overlap() {
        assert_eq!(
            temporal_overlap(None, None, None, None, Timestamp(100)),
            1.0
        );
    }

    #[test]
    fn one_expired_no_temporal_overlap() {
        // b expired before as_of → no overlap.
        let g = temporal_overlap(None, None, None, Some(Timestamp(50)), Timestamp(100));
        assert_eq!(g, 0.0);
    }

    #[test]
    fn one_not_yet_valid_no_temporal_overlap() {
        let g = temporal_overlap(None, None, Some(Timestamp(200)), None, Timestamp(100));
        assert_eq!(g, 0.0);
    }

    // ── stress (multiplicative gates) ─────────────────────────────────────────

    #[test]
    fn stress_full_when_all_gates_open() {
        // cw=1, min(a)=0.6, scope=1, temporal=1 → 0.6
        let s = stress(1.0, 0.8, 0.6, 1.0, 1.0);
        assert!((s - 0.6).abs() < 1e-12, "got {s}");
    }

    #[test]
    fn stress_uses_min_of_activations() {
        let s1 = stress(1.0, 0.9, 0.2, 1.0, 1.0);
        let s2 = stress(1.0, 0.2, 0.9, 1.0, 1.0);
        assert!((s1 - 0.2).abs() < 1e-12);
        assert!((s1 - s2).abs() < 1e-12, "stress must be symmetric in min");
    }

    #[test]
    fn one_inactive_endpoint_zero_stress() {
        // Failure condition: one active endpoint must not create stress.
        assert_eq!(stress(1.0, 0.9, 0.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn scope_gate_zero_zero_stress() {
        assert_eq!(stress(1.0, 0.9, 0.9, 0.0, 1.0), 0.0);
    }

    #[test]
    fn temporal_gate_zero_zero_stress() {
        assert_eq!(stress(1.0, 0.9, 0.9, 1.0, 0.0), 0.0);
    }

    #[test]
    fn contradiction_weight_gate_zero_zero_stress() {
        assert_eq!(stress(0.0, 0.9, 0.9, 1.0, 1.0), 0.0);
    }

    #[test]
    fn stress_is_non_negative() {
        // Even with a negative spurious input, stress never goes below 0.
        assert!(stress(1.0, -0.5, 0.9, 1.0, 1.0) >= 0.0);
    }

    #[test]
    fn nan_input_collapses_gate() {
        assert_eq!(stress(1.0, f64::NAN, 0.9, 1.0, 1.0), 0.0);
        assert_eq!(stress(f64::INFINITY, 0.9, 0.9, 1.0, 1.0), 0.0);
    }

    // ── tension flux (commit-time) ────────────────────────────────────────────

    #[test]
    fn tension_flux_scales_by_presentation() {
        assert!((tension_flux(0.5, 0.4) - 0.2).abs() < 1e-12);
        assert_eq!(tension_flux(0.0, 0.4), 0.0);
    }

    proptest! {
        #[test]
        fn stress_zero_if_any_gate_zero(
            cw in 0.0f64..=2.0,
            ai in 0.0f64..=1.0,
            aj in 0.0f64..=1.0,
            scope in 0.0f64..=1.0,
            temporal in 0.0f64..=1.0,
        ) {
            let s = stress(cw, ai, aj, scope, temporal);
            prop_assert!(s.is_finite() && s >= 0.0);
            if cw == 0.0 || ai == 0.0 || aj == 0.0 || scope == 0.0 || temporal == 0.0 {
                prop_assert_eq!(s, 0.0, "any zero gate must zero the stress");
            }
        }
    }
}
