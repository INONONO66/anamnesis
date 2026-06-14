//! Query-local readout energy `E(S | Q)` — an interpretive objective, not a reservoir.
//!
//! Energy is the scalar objective used to *explain and stabilize* readout over an
//! active subsystem `S` ([energy.md](../../docs/04-cognitive-dynamics/energy.md),
//! [ADR-0007](../../docs/adr/0007-energy-objective-symmetric-caveat.md)). It is
//! **query-local and never stored**: it is computed from the settled query response
//! and the active selection, and is discarded with the query.
//!
//! ## Objective shape
//!
//! ```text
//! E(S | Q) =
//!     - field_alignment(S, Q)
//!     - conductive_support(S)
//!     + impedance_regularization(S)
//!     + frustration_penalty(S)
//! ```
//!
//! The four leading `+`/`-` coefficients are **structural descent-direction signs
//! (`+/-1`), not tunable magnitudes** (energy.md "Objective Shape"). They encode that
//! alignment and conductive support *lower* energy while impedance and frustration
//! *raise* it; there are no per-term weights to fit. This module therefore exposes
//! the four terms as non-negative magnitudes and composes them with the fixed signs.
//!
//! ## The strict symmetric-coupling caveat (ADR-0007)
//!
//! Energy minimization is mathematically exact **only under symmetric coupling**
//! (`C_ij = C_ji`), where the graph energy is a true Dirichlet/Lyapunov form
//! ([`dirichlet_energy`]). Real activation flow is **directed and driven-dissipative**:
//! once query injection and dissipation exist no conservation law applies, and the
//! **true fixed point is the RWR stationary activation vector `a*`**, computed by
//! [`crate::query::rwr::additive_rwr`]. The stationary vector is primary; this energy
//! merely explains and ranks the readout *around* it. Nothing here drives the
//! dynamics — it is read-only interpretation of an already-settled response.
//!
//! All functions are pure: no side effects, no storage access, no mutation.

use crate::mechanics::priors::project_weight;

/// Per-site contribution to the readout energy over an active subsystem `S`.
///
/// One [`SiteEnergy`] is built for each site selected into the active subsystem
/// from the settled query response. All quantities are query-local and transient.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SiteEnergy {
    /// Settled query-local activation response `a_i` (probability-like, in `[0, 1]`).
    pub activation: f64,
    /// Potential bias `phi_i` from the query field — how well the site aligns with
    /// the query (the `field_alignment` driver).
    pub phi: f64,
    /// Effective impedance `Z_i` — how expensive it is to light the site
    /// (the `impedance_regularization` cost).
    pub impedance: f64,
}

/// A conductive bond between two active sites in the subsystem `S`.
///
/// Carries the *projected* conductance `project_weight(C_ij)` in `[0, 1]` (the
/// bounded view of the log-LR reservoir) together with the two endpoints'
/// activations. Conductive support measures how strongly co-active sites support
/// each other through their conductance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SiteBond {
    /// Conductance reservoir `C_ij` (log likelihood ratio) of the bond.
    pub conductance: f64,
    /// Settled activation `a_i` of the first endpoint.
    pub activation_i: f64,
    /// Settled activation `a_j` of the second endpoint.
    pub activation_j: f64,
}

/// The four energy-term magnitudes plus the composed total.
///
/// The terms are reported as **non-negative magnitudes**; [`EnergyTerms::total`]
/// applies the fixed structural signs (`- alignment - support + impedance +
/// frustration`). Exposing the decomposition lets a trace explain *why* a bundle was
/// selected (energy.md "Role").
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EnergyTerms {
    /// `field_alignment(S, Q)` magnitude — lowers energy (enters with `-1`).
    pub field_alignment: f64,
    /// `conductive_support(S)` magnitude — lowers energy (enters with `-1`).
    pub conductive_support: f64,
    /// `impedance_regularization(S)` magnitude — raises energy (enters with `+1`).
    pub impedance_regularization: f64,
    /// `frustration_penalty(S)` magnitude — raises energy (enters with `+1`).
    pub frustration_penalty: f64,
}

impl EnergyTerms {
    /// The composed scalar energy `E(S | Q)` with the fixed structural signs.
    ///
    /// ```text
    /// E = - field_alignment - conductive_support + impedance_regularization + frustration_penalty
    /// ```
    ///
    /// The coefficients are `+/-1` descent-direction signs, never fitted magnitudes.
    pub fn total(&self) -> f64 {
        -self.field_alignment - self.conductive_support
            + self.impedance_regularization
            + self.frustration_penalty
    }
}

/// `field_alignment(S, Q)` — how well the selected sites align with the query field.
///
/// Each active site contributes its activation-weighted potential bias `a_i * phi_i`:
/// a site that the query lit strongly *and* that aligns with the query field
/// contributes the most alignment. This is a non-negative *magnitude* (clamped at
/// `0`); it enters `E` with the structural `-1` sign, so stronger alignment lowers
/// energy.
pub fn field_alignment(sites: &[SiteEnergy]) -> f64 {
    let mut sum = 0.0;
    for s in sites {
        let a = gate(s.activation);
        let phi = finite(s.phi);
        sum += a * phi;
    }
    if sum.is_finite() { sum.max(0.0) } else { 0.0 }
}

/// `conductive_support(S)` — how strongly the selected sites support each other
/// through conductance.
///
/// Each bond between two co-active selected sites contributes
/// `project_weight(C_ij) * min(a_i, a_j)`: the projected (bounded) conductance gated
/// by the weaker endpoint's activation, so a bond only supports the bundle when both
/// endpoints are actually active. Non-negative; enters `E` with the structural `-1`
/// sign, so stronger mutual support lowers energy.
pub fn conductive_support(bonds: &[SiteBond]) -> f64 {
    let mut sum = 0.0;
    for b in bonds {
        let w = gate(project_weight(b.conductance));
        let min_active = gate(b.activation_i).min(gate(b.activation_j));
        sum += w * min_active;
    }
    if sum.is_finite() { sum.max(0.0) } else { 0.0 }
}

/// `impedance_regularization(S)` — the cost of lighting isolated or cold sites.
///
/// Each active site contributes its activation-weighted impedance `a_i * Z_i`:
/// lighting a high-impedance site (isolated/cold, hard to reach) is expensive, and
/// the cost is scaled by how much it was lit. Non-negative; enters `E` with the
/// structural `+1` sign, so lighting expensive sites raises energy.
pub fn impedance_regularization(sites: &[SiteEnergy]) -> f64 {
    let mut sum = 0.0;
    for s in sites {
        let a = gate(s.activation);
        let z = finite(s.impedance).max(0.0);
        sum += a * z;
    }
    if sum.is_finite() { sum.max(0.0) } else { 0.0 }
}

/// `frustration_penalty(S, Sigma)` — stress from contradictory sites active together.
///
/// The sum of the query-local stresses `sigma_ij` over the contradiction pairs whose
/// endpoints are both active in `S` (frustration.md / [`crate::mechanics::frustration`]).
/// Non-negative; enters `E` with the structural `+1` sign, so co-activating a
/// contradiction raises energy — the positive sign is what encourages conflicting
/// bundles to *separate* without deleting either side (ADR-0006).
pub fn frustration_penalty(stresses: &[f64]) -> f64 {
    let mut sum = 0.0;
    for &sigma in stresses {
        sum += gate(sigma);
    }
    if sum.is_finite() { sum.max(0.0) } else { 0.0 }
}

/// Compose the full readout energy decomposition `E(S | Q)` over an active subsystem.
///
/// Builds the four [`EnergyTerms`] magnitudes from the active sites, their conductive
/// bonds, and the active contradiction stresses. The scalar energy is
/// [`EnergyTerms::total`]. Query-local and never stored (energy.md).
pub fn energy(sites: &[SiteEnergy], bonds: &[SiteBond], stresses: &[f64]) -> EnergyTerms {
    EnergyTerms {
        field_alignment: field_alignment(sites),
        conductive_support: conductive_support(bonds),
        impedance_regularization: impedance_regularization(sites),
        frustration_penalty: frustration_penalty(stresses),
    }
}

/// The symmetric-coupling Dirichlet/Lyapunov energy — a **true** energy, exact only
/// when `C_ij = C_ji` (energy.md "Symmetric Coupling", ADR-0007).
///
/// ```text
/// a^T L a = 1/2 * sum_{i,j} C_ij * (a_i - a_j)^2
/// ```
///
/// This measures how much the activation pattern violates the conductive structure:
/// a bond with high conductance whose endpoints carry very different activation
/// contributes a large penalty. It is computed over *projected* conductance
/// `project_weight(C_ij)` (the bounded, non-negative bond weight), so the form is a
/// valid quadratic Dirichlet energy. This is the **only** case in which "energy
/// descent" is literally exact; under directed RWR the true fixed point is the
/// stationary vector `a*` and [`energy`] above is the interpretive objective instead.
///
/// `bonds` must be supplied as the *symmetric* coupling (each undirected bond once);
/// the `1/2` factor in the Dirichlet form is folded in by summing each bond once.
pub fn dirichlet_energy(bonds: &[SiteBond]) -> f64 {
    let mut sum = 0.0;
    for b in bonds {
        let c = gate(project_weight(b.conductance));
        let ai = finite(b.activation_i);
        let aj = finite(b.activation_j);
        let diff = ai - aj;
        sum += c * diff * diff;
    }
    if sum.is_finite() { sum.max(0.0) } else { 0.0 }
}

/// Clamps a value to a finite, non-negative quantity (`NaN`/`Inf` → `0`).
#[inline]
fn gate(v: f64) -> f64 {
    if v.is_finite() { v.max(0.0) } else { 0.0 }
}

/// Returns `v` if finite, else `0.0` (allows negative inputs through, unlike [`gate`]).
#[inline]
fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn site(activation: f64, phi: f64, impedance: f64) -> SiteEnergy {
        SiteEnergy {
            activation,
            phi,
            impedance,
        }
    }

    fn bond(conductance: f64, ai: f64, aj: f64) -> SiteBond {
        SiteBond {
            conductance,
            activation_i: ai,
            activation_j: aj,
        }
    }

    // ── structural signs (the load-bearing property) ──────────────────────────

    #[test]
    fn signs_are_structural_plus_minus_one() {
        // alignment and support lower energy; impedance and frustration raise it.
        let terms = EnergyTerms {
            field_alignment: 1.0,
            conductive_support: 2.0,
            impedance_regularization: 4.0,
            frustration_penalty: 8.0,
        };
        // E = -1 - 2 + 4 + 8 = 9
        assert!((terms.total() - 9.0).abs() < 1e-12, "got {}", terms.total());
    }

    #[test]
    fn more_alignment_lowers_energy() {
        let weak = energy(&[site(0.5, 0.2, 0.0)], &[], &[]);
        let strong = energy(&[site(0.5, 2.0, 0.0)], &[], &[]);
        assert!(
            strong.total() < weak.total(),
            "stronger field alignment must lower energy: {} !< {}",
            strong.total(),
            weak.total()
        );
    }

    #[test]
    fn more_conductive_support_lowers_energy() {
        let weak = energy(&[], &[bond(-2.0, 0.5, 0.5)], &[]);
        let strong = energy(&[], &[bond(3.0, 0.5, 0.5)], &[]);
        assert!(
            strong.total() < weak.total(),
            "stronger conductive support must lower energy"
        );
    }

    #[test]
    fn more_impedance_raises_energy() {
        let cheap = energy(&[site(0.5, 0.0, 0.0)], &[], &[]);
        let costly = energy(&[site(0.5, 0.0, 5.0)], &[], &[]);
        assert!(
            costly.total() > cheap.total(),
            "lighting a high-impedance site must raise energy"
        );
    }

    #[test]
    fn more_frustration_raises_energy() {
        let calm = energy(&[], &[], &[]);
        let stressed = energy(&[], &[], &[0.4, 0.6]);
        assert!(
            stressed.total() > calm.total(),
            "co-active contradictions must raise energy (the positive sign, ADR-0006)"
        );
        // The penalty is exactly the summed stress.
        assert!((stressed.frustration_penalty - 1.0).abs() < 1e-12);
    }

    // ── individual term shapes ─────────────────────────────────────────────────

    #[test]
    fn field_alignment_is_activation_weighted_phi() {
        // a*phi = 0.5*2 + 0.25*4 = 1 + 1 = 2
        let sites = [site(0.5, 2.0, 0.0), site(0.25, 4.0, 0.0)];
        assert!((field_alignment(&sites) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn conductive_support_uses_min_activation_and_projected_weight() {
        // project_weight(0) = 0.5; min(0.8, 0.2) = 0.2 -> 0.1
        let bonds = [bond(0.0, 0.8, 0.2)];
        assert!((conductive_support(&bonds) - 0.1).abs() < 1e-12);
    }

    #[test]
    fn inactive_endpoint_gives_no_support() {
        let bonds = [bond(5.0, 0.9, 0.0)];
        assert_eq!(conductive_support(&bonds), 0.0);
    }

    #[test]
    fn impedance_regularization_is_activation_weighted() {
        // a*Z = 0.5*3 + 0.5*1 = 2
        let sites = [site(0.5, 0.0, 3.0), site(0.5, 0.0, 1.0)];
        assert!((impedance_regularization(&sites) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn frustration_penalty_sums_stress() {
        assert!((frustration_penalty(&[0.1, 0.2, 0.3]) - 0.6).abs() < 1e-12);
        assert_eq!(frustration_penalty(&[]), 0.0);
    }

    // ── symmetric Dirichlet form (true Lyapunov, ADR-0007) ─────────────────────

    #[test]
    fn dirichlet_zero_when_activations_equal() {
        // Equal activations across the bond perfectly satisfy the conductive
        // structure: zero Dirichlet energy regardless of conductance.
        assert_eq!(dirichlet_energy(&[bond(5.0, 0.7, 0.7)]), 0.0);
    }

    #[test]
    fn dirichlet_grows_with_activation_disagreement() {
        let agree = dirichlet_energy(&[bond(2.0, 0.6, 0.5)]);
        let disagree = dirichlet_energy(&[bond(2.0, 0.9, 0.1)]);
        assert!(
            disagree > agree,
            "a high-conductance bond whose endpoints disagree must cost more"
        );
    }

    #[test]
    fn dirichlet_scales_with_conductance() {
        // project_weight is monotonic, so a higher-conductance bond with the same
        // activation gap carries more Dirichlet penalty.
        let weak = dirichlet_energy(&[bond(0.0, 0.9, 0.1)]);
        let strong = dirichlet_energy(&[bond(4.0, 0.9, 0.1)]);
        assert!(strong > weak);
    }

    // ── non-finite safety ──────────────────────────────────────────────────────

    #[test]
    fn non_finite_inputs_are_trapped() {
        let sites = [site(f64::NAN, f64::INFINITY, f64::NAN)];
        let bonds = [bond(f64::INFINITY, f64::NAN, 0.5)];
        let terms = energy(&sites, &bonds, &[f64::NAN]);
        assert!(terms.total().is_finite());
        assert!(dirichlet_energy(&bonds).is_finite());
    }

    proptest! {
        #[test]
        fn energy_is_finite_for_bounded_inputs(
            a in 0.0f64..=1.0,
            phi in -10.0f64..=10.0,
            z in 0.0f64..=40.0,
            c in -40.0f64..=40.0,
            sigma in 0.0f64..=5.0,
        ) {
            let terms = energy(
                &[site(a, phi, z)],
                &[bond(c, a, a)],
                &[sigma],
            );
            prop_assert!(terms.total().is_finite());
            prop_assert!(terms.field_alignment >= 0.0);
            prop_assert!(terms.conductive_support >= 0.0);
            prop_assert!(terms.impedance_regularization >= 0.0);
            prop_assert!(terms.frustration_penalty >= 0.0);
            prop_assert!(dirichlet_energy(&[bond(c, a, a)]).is_finite());
        }
    }
}
