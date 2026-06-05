//! Calibrated priors — the single home for the engine's numeric constants.
//!
//! Per [ADR-0010](../../docs/adr/0010-calibrated-priors-not-laws.md) these are
//! *calibrated priors*, not physical laws: documented starting values fit to
//! observed behavior, refit-able as graph statistics change. Each constant
//! declares whether it is **DERIVED** (forced by a behavioral specification) or
//! a **CALIBRATED PRIOR** (a fitted or declared default).
//!
//! This module is the only place a magic number may live. Constants migrate in
//! as each migration phase touches them; nothing here may be presented as a law.
//!
//! ## Reserved for later phases
//!
//! - Reservoir↔projection functions (`project_salience`, `project_weight`,
//!   `project_conductance`, and their backfill inverses) are added in **Phase 1**.
//!   Locked conventions (see migration design Decision 5):
//!   `A_i` = log need-odds (unbounded) and `salience = project_salience(A) = logistic(A)`;
//!   `C_ij` = log likelihood ratio (unbounded) and `weight = project_weight(C) = logistic(C)`;
//!   backfill inverses are `logit(clamp(x, EPS, 1-EPS))` using [`LOGIT_BACKFILL_EPS`].
//!   The doc Hebbian `(1 - C_ij)` bound is realized in Phase 3 as `(1 - project_weight(C))`.
//! - Learning rates (`eta_path`, `eta_pair`, `eta_leak`), surprise gain `k`, and
//!   `theta_sep` arrive with their phases (2–4). Per-edge-type `kappa` is deferred.

/// ACT-R base-level power-law decay exponent `d`.
///
/// CALIBRATED PRIOR — ACT-R canonical default `d ≈ 0.5`
/// ([ADR-0008](../../docs/adr/0008-powerlaw-dissipation.md)).
pub const DECAY_EXPONENT_D: f64 = 0.5;

/// Random-walk-with-restart restart probability `alpha`, default.
///
/// CALIBRATED PRIOR — replaced in Phase 3 by a reach-derived
/// `alpha = 1 - f^(1/h_half)` ([ADR-0005](../../docs/adr/0005-additive-activation-flow.md)).
pub const RWR_RESTART_PROBABILITY: f64 = 0.15;

/// Epsilon for the clamped-logit projection backfill, so that
/// `logit(clamp(x, EPS, 1 - EPS))` stays finite at the `0`/`1` boundaries.
///
/// DERIVED — bounds the inverse projection away from `±inf`. Reserved here as the
/// single home for the Phase 1 reservoir migration (migration design Decision 5).
pub const LOGIT_BACKFILL_EPS: f64 = 1e-6;
