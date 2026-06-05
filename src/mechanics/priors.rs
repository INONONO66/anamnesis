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
//! ## Reservoir ↔ projection (Phase 1, below)
//!
//! Locked conventions (see migration design Decision 5):
//! `A_i` = log need-odds (unbounded) and `salience = project_salience(A) = logistic(A)`;
//! `C_ij` = log likelihood ratio (unbounded) and `weight = project_weight(C) = logistic(C)`;
//! backfill inverses are `logit(clamp(x, EPS, 1-EPS))` using [`LOGIT_BACKFILL_EPS`].
//! The doc Hebbian `(1 - C_ij)` bound is realized in **Phase 3** as `(1 - project_weight(C))`.
//!
//! ## Reserved for later phases
//!
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

/// Numerical-safety bound for the log-odds reservoirs (`A_i`, `C_ij`).
///
/// DERIVED — keeps reservoir arithmetic finite. `logistic(±40)` is `1.0`/`0.0`
/// within `f64` precision, so this is far beyond any value the clamped-logit
/// backfill (`±logit(1 - EPS) ≈ ±13.8`) produces; it exists only to trap blowups.
pub const LOG_ODDS_CLAMP: f64 = 40.0;

// --- Reservoir ↔ projection (ADR-0002) -------------------------------------
//
// Reservoirs are the authoritative log-odds state; projections are bounded
// derived views in (0, 1). These functions are pure and deterministic. Per the
// ADR-0002 standing invariant, only `commit`/`tick` may *store* a projection;
// these functions just compute it.

/// Logistic squashing `1 / (1 + e^-x)`, mapping log-odds to a probability in (0, 1).
#[inline]
fn logistic(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// Clamped logit `ln(p / (1 - p))` with `p` pinned to `[EPS, 1 - EPS]` so the
/// result is always finite (the inverse of [`logistic`] on the safe interior).
#[inline]
fn clamped_logit(p: f64) -> f64 {
    let p = p.clamp(LOGIT_BACKFILL_EPS, 1.0 - LOGIT_BACKFILL_EPS);
    (p / (1.0 - p)).ln()
}

/// `salience = project_salience(A) = logistic(A)` — the bounded public view of
/// retained action `A_i` (log need-odds). ADR-0002.
#[inline]
pub fn project_salience(retained_action: f64) -> f64 {
    logistic(retained_action)
}

/// Backfill inverse `A = logit(clamp(salience))`: recovers retained action from a
/// legacy bounded salience during the v2→v3 migration. Finite at the 0/1 ends.
#[inline]
pub fn salience_to_action(salience: f64) -> f64 {
    clamped_logit(salience)
}

/// `weight = project_weight(C) = logistic(C)` — the bounded public view of
/// conductance `C_ij` (log likelihood ratio). ADR-0002.
#[inline]
pub fn project_weight(conductance: f64) -> f64 {
    logistic(conductance)
}

/// Backfill inverse `C = logit(clamp(weight))`: recovers conductance from a legacy
/// bounded edge weight during the v2→v3 migration. Finite at the 0/1 ends.
#[inline]
pub fn weight_to_conductance(weight: f64) -> f64 {
    clamped_logit(weight)
}

/// Finite-guard for the conductance reservoir: clamps to `[-LOG_ODDS_CLAMP,
/// LOG_ODDS_CLAMP]`. This is NOT a `[0, 1]` bound — `C_ij` is unbounded log-LR.
///
/// The doc's Hebbian Oja bound `dC = η·flux·(1 - C_ij)` (conductance.md) is
/// realized in Phase 3 as saturation via the *projection* `(1 - project_weight(C))`,
/// keeping `C` in log-LR units (migration design Decision 5).
#[inline]
pub fn project_conductance(conductance: f64) -> f64 {
    conductance.clamp(-LOG_ODDS_CLAMP, LOG_ODDS_CLAMP)
}
