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
//! - Surprise gain `k` and `theta_sep` arrive with their phases (3–4).
//!   Per-edge-type `kappa` is deferred.

/// ACT-R base-level power-law decay exponent `d`.
///
/// CALIBRATED PRIOR — ACT-R canonical default `d ≈ 0.5`
/// ([ADR-0008](../../docs/adr/0008-powerlaw-dissipation.md)).
pub const DECAY_EXPONENT_D: f64 = 0.5;

/// Target co-activation count `N` — the single behavioral specification from which
/// every learning rate derives ([conductance.md](../../docs/04-cognitive-dynamics/conductance.md),
/// [interactions.md](../../docs/04-cognitive-dynamics/interactions.md)).
///
/// CALIBRATED PRIOR — `N` is the number of full-flux interactions at which a
/// reservoir should reach its saturation target. The single core learning rate is
/// `eta = 1 - 0.5^(1/N)` ([`learning_rate`]); the same `eta` drives feedback
/// (Rescorla-Wagner `dA`), Hebbian-Oja `dC`, and the saturating `access_gain`.
/// Splitting into `eta_path`/`eta_pair` is an optional later refit of one `N`,
/// not a separate base constant.
pub const TARGET_COACTIVATION_N: f64 = 10.0;

/// The single core learning rate `eta = 1 - 0.5^(1/N)`, derived from the target
/// co-activation count `N` ([conductance.md](../../docs/04-cognitive-dynamics/conductance.md)).
///
/// DERIVED — forced by the behavioral specification `N`: after `N` full-flux
/// updates a reservoir's projection reaches the `0.5` Oja/symmetric saturation
/// target. Returns `0.0` for non-positive `N`. There is one core `eta`; per-channel
/// rates are an optional refit of the same `N`, never independent constants.
#[inline]
pub fn learning_rate(n: f64) -> f64 {
    if n <= 0.0 {
        return 0.0;
    }
    1.0 - 0.5_f64.powf(1.0 / n)
}

/// Initial retained action `A_i` for a freshly created node (`SiteInserted`).
///
/// CALIBRATED PRIOR — a new site enters with high need-odds. This is the
/// reservoir-authoritative initial value (ADR-0002): node creation sets
/// `retained_action = INITIAL_RETAINED_ACTION` and derives
/// `salience = project_salience(INITIAL_RETAINED_ACTION) ≈ 1.0`, so the reservoir
/// and its projection agree from the start. `logistic(13.8) ≈ 0.999999`. Phase 4
/// replaces this flat prior with a Bayesian-surprise initial charge.
pub const INITIAL_RETAINED_ACTION: f64 = 13.8;

/// Log-odds reward scale for Rescorla-Wagner feedback targets.
///
/// CALIBRATED PRIOR — a unit-strength `Useful` signal targets `+REWARD_LOG_ODDS_SCALE`
/// log need-odds (`project_salience ≈ 0.98`), unit-strength negative feedback targets
/// the symmetric `-REWARD_LOG_ODDS_SCALE`. The Rescorla-Wagner step moves a fraction
/// `eta` toward this target ([`crate::mechanics::interactions::lambda_reward`]).
pub const REWARD_LOG_ODDS_SCALE: f64 = 4.0;

/// Per-`node_type` policy multiplier on the decay exponent `d` (dissipation.md).
///
/// CALIBRATED PRIOR — tier/type is *policy*, not an independent decay knob: it
/// only scales the single free decay prior `d`. Core ≈ 0 (protected), Working
/// below one, Episodic one, Archive excluded. Forgetting lives entirely in the
/// retained-action dynamics governed by `d` together with this multiplier; there
/// is no separate per-type decay rate.
///
/// Returns the factor by which `d` is scaled for the given knowledge type.
pub fn decay_multiplier_for_type(kt: &crate::graph::KnowledgeType) -> f64 {
    use crate::graph::KnowledgeType;
    match kt {
        // Core identity — protected from ordinary decay.
        KnowledgeType::IdentityCore => 0.0,
        // Slow-decaying identity / convention layers (Working-like).
        KnowledgeType::IdentityLearned => 0.10,
        KnowledgeType::Convention | KnowledgeType::Decision => 0.30,
        KnowledgeType::IdentityState => 0.60,
        // Ordinary semantic / procedural knowledge.
        KnowledgeType::Semantic | KnowledgeType::Procedural | KnowledgeType::Entity => 0.40,
        KnowledgeType::Gotcha => 0.40,
        KnowledgeType::Event => 0.60,
        // Episodic decays at the full nominal rate.
        KnowledgeType::Episodic => 1.0,
        // Debug-lifecycle nodes are inert: they do not decay under maintenance.
        KnowledgeType::Hypothesis | KnowledgeType::Evidence | KnowledgeType::DebugSession => 0.0,
        KnowledgeType::Custom(_) => 0.40,
    }
}

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
