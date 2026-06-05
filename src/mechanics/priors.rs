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
//! ## Phase 3 (flow + readout)
//!
//! Additive directed RWR: [`restart_alpha`] (reach-derived `alpha = 1/(L+1)`),
//! [`edge_type_factor`] (within-row relative conductance per edge type, `Contradicts`
//! excluded), [`RWR_TOLERANCE`]/[`RWR_MAX_ITERATIONS`]. Potential field:
//! [`SEED_SOFTMAX_TAU`] plus the `beta_*` feature weights. Readout: the seven
//! `READOUT_W_*` coefficients of the authoritative additive log-odds score.
//!
//! ## Phase 4 (frustration + surprise-gated perception)
//!
//! Frustration: [`CONTRADICTION_WEIGHT_DEFAULT`] is the declared per-edge stress
//! gate factor when none is stored. Perception: the surprise gain [`SURPRISE_GAIN_K`]
//! converts Bayesian surprise `eps` into the initial allocate charge `dA = k*eps`,
//! and [`theta_sep`] derives the novelty separation boundary from the encoder's
//! distinct-pair q95 ([`ENCODER_DISTINCT_PAIR_Q95`]).

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
/// CALIBRATED PRIOR — superseded by the reach-derived [`restart_alpha`] below
/// ([ADR-0005](../../docs/adr/0005-additive-activation-flow.md)). Retained only as
/// the fallback when no mean-reach prior is supplied.
pub const RWR_RESTART_PROBABILITY: f64 = 0.15;

// --- Additive directed RWR flow (Phase 3, ADR-0005 / activation-flow.md) ----

/// Mean associative reach `L` — the behavioral specification from which the RWR
/// restart rate derives ([activation-flow.md](../../docs/05-context-retrieval/activation-flow.md)).
///
/// CALIBRATED PRIOR — `L` is the mean number of hops a cue's influence travels
/// before restart. `alpha = 1 / (L + 1)`; the canonical `0.15` prior corresponds to
/// `L ≈ 5.67` (roughly six-hop mean reach). Refit per graph statistics.
pub const MEAN_ASSOCIATIVE_REACH_L: f64 = 5.667;

/// Reach-derived RWR restart rate `alpha = 1 / (L + 1)` ([ADR-0005](../../docs/adr/0005-additive-activation-flow.md)).
///
/// DERIVED — forced by mean associative reach `L`: the per-hop attenuation is
/// `(1 - alpha)` and the operator's contraction modulus is `(1 - alpha) < 1`, so the
/// additive RWR converges geometrically to a unique fixed point. Returns the canonical
/// fallback [`RWR_RESTART_PROBABILITY`] for non-positive `L`.
#[inline]
pub fn restart_alpha(l: f64) -> f64 {
    if l <= 0.0 || !l.is_finite() {
        return RWR_RESTART_PROBABILITY;
    }
    1.0 / (l + 1.0)
}

/// Convergence tolerance for the additive RWR iteration (`||a_next - a||_1`).
///
/// CALIBRATED PRIOR — iteration stops when the L1 change between successive
/// responses drops below this; the geometric `(1 - alpha)` contraction guarantees
/// it is reached well within [`RWR_MAX_ITERATIONS`].
pub const RWR_TOLERANCE: f64 = 1e-10;

/// Hard iteration bound for the additive RWR flow.
///
/// DERIVED — a safety cap. With contraction `(1 - alpha)` and tolerance
/// [`RWR_TOLERANCE`], convergence is reached long before this bound; reaching it
/// is reported as `truncated = true` (activation-flow.md failure conditions).
pub const RWR_MAX_ITERATIONS: usize = 256;

/// Within-row relative conductance multiplier per edge type
/// (`edge_type_factor_ij`, [activation-flow.md](../../docs/05-context-retrieval/activation-flow.md)).
///
/// CALIBRATED PRIOR — a declared ordinal prior at cold start with the ordering
/// `Reason` > `ReinforcedBy` > `Semantic` > `Temporal` > `RejectedAlternative`.
/// Factors are *relative within a row* (`P` is re-normalized row-stochastic), so
/// only the ordering is load-bearing. `Contradicts` returns `0.0` — it is excluded
/// from propagation and routed to frustration; `Refutes` is a weak debug relation.
/// Once per-type co-activation data exists these are refit from per-type mean `C_ij`.
pub fn edge_type_factor(edge_type: &crate::graph::EdgeType, is_forward: bool) -> f64 {
    use crate::graph::EdgeType;
    match edge_type {
        // Supersedes is directional: strong toward the new fact, weak toward the old.
        EdgeType::Supersedes => {
            if is_forward {
                1.20
            } else {
                0.40
            }
        }
        EdgeType::Reason => 1.15,
        EdgeType::ReinforcedBy => 1.10,
        EdgeType::Supports => 1.10,
        EdgeType::Semantic => 1.00,
        EdgeType::Causal => 1.00,
        EdgeType::ConsolidatedFrom => 1.00,
        EdgeType::ExtractedFrom => 1.00,
        EdgeType::Entity => 0.95,
        EdgeType::BelongsTo => 0.95,
        EdgeType::Temporal => 0.85,
        EdgeType::RejectedAlternative => 0.60,
        EdgeType::Refutes => 0.30,
        // Excluded from propagation — handled by frustration (ADR-0005).
        EdgeType::Contradicts => 0.0,
        EdgeType::Custom(_) => 1.00,
    }
}

// --- Potential field / seed distribution (potential-landscape.md) -----------

/// Softmax temperature `tau` for the RWR restart seed distribution
/// (`seed_i = softmax(phi_i / tau)`, [potential-landscape.md](../../docs/04-cognitive-dynamics/potential-landscape.md)).
///
/// CALIBRATED PRIOR — controls how sharply the restart mass concentrates on the
/// highest-potential cues. Lower `tau` = sharper. Fit from accepted readout data.
pub const SEED_SOFTMAX_TAU: f64 = 1.0;

/// Feature weight `beta_text` for the lexical-match term of the potential bias.
/// CALIBRATED PRIOR — one entry of the single potential-field regression object.
pub const BETA_TEXT: f64 = 1.0;
/// Feature weight `beta_embed` for the embedding-similarity term.
/// CALIBRATED PRIOR — potential-field regression object.
pub const BETA_EMBED: f64 = 1.0;
/// Feature weight `beta_entity` for the entity-overlap term.
/// CALIBRATED PRIOR — potential-field regression object.
pub const BETA_ENTITY: f64 = 1.0;
/// Feature weight `beta_scope` for the scope-compatibility term.
/// CALIBRATED PRIOR — potential-field regression object.
pub const BETA_SCOPE: f64 = 1.0;
/// Feature weight `beta_identity` for the identity-bias term.
/// CALIBRATED PRIOR — potential-field regression object.
pub const BETA_IDENTITY: f64 = 1.0;
/// Feature weight `beta_prior` for the retained-action term.
///
/// DERIVED — fixed at `1.0` by design ([potential-landscape.md](../../docs/04-cognitive-dynamics/potential-landscape.md)):
/// `A_i` is already log prior-odds, so by ACT-R/Bayes odds-additivity it enters
/// `phi_i` with unit coefficient. Not a free knob.
pub const BETA_PRIOR: f64 = 1.0;

// --- Readout score (readout-scoring.md, the authoritative 7-term form) ------
//
// The seven coefficients are ONE calibrated re-ranking regression object, not
// seven independent knobs. The default is unit coefficients, which recovers the
// plain additive log-odds sum `posterior = prior + sum of evidence`. They are
// calibrated priors, not laws (ADR-0010); refit from accepted-readout data.

/// `w_a` — weight on the (logit-of) query-local activation response `a_i`.
pub const READOUT_W_A: f64 = 1.0;
/// `w_phi` — weight on the potential bias `phi_i`.
pub const READOUT_W_PHI: f64 = 1.0;
/// `w_s` — weight on the salience projection `logit(s_i)`.
pub const READOUT_W_S: f64 = 1.0;
/// `w_z` — penalty weight on the effective impedance `Z_i` (subtracted).
pub const READOUT_W_Z: f64 = 1.0;
/// `w_scope` — weight on the scope-compatibility term.
pub const READOUT_W_SCOPE: f64 = 1.0;
/// `w_trust` — weight on the origin/peer-reliability term.
pub const READOUT_W_TRUST: f64 = 1.0;
/// `w_stress` — penalty weight on attached frustration `stress_i` (subtracted).
pub const READOUT_W_STRESS: f64 = 1.0;

// --- Frustration (frustration.md, ADR-0006) --------------------------------
//
// Contradictions are SURFACED as query-local stress, never suppressed and never
// deleted. Stress is purely a multiplicative product of gates
// (`sigma_ij = contradiction_weight * min(a_i, a_j) * scope_overlap * temporal_overlap`);
// if any gate is zero, `sigma = 0`. There is no exponential activation damping
// and no rigidity term — `Contradicts` activation is never reduced.

/// Default contradiction-weight gate factor `contradiction_weight_ij` for a
/// `Contradicts` edge that carries no explicit stored weight (frustration.md).
///
/// CALIBRATED PRIOR — the per-edge strength of a contradiction as a stress gate.
/// It is one multiplicative factor in `sigma_ij`; the stored edge weight (the
/// `project_weight(C_ij)` projection) is used when present, and this is the
/// fallback. Unit default keeps `sigma_ij = min(a_i, a_j) * scope * temporal`.
pub const CONTRADICTION_WEIGHT_DEFAULT: f64 = 1.0;

// --- Surprise-gated perception (perception.md, ADR-0009) --------------------
//
// Perception is a two-stage gate. Stage 1 rejects only untrusted (low-confidence)
// or unaffordable-and-not-novel observations. Stage 2 routes survivors: novelty
// `> theta_sep` allocates a new site (surprise-gated charge `dA = k*eps`); novelty
// `<= theta_sep` routes to and reinforces the nearest existing site. Familiarity
// is never a rejection reason.

/// Surprise gain `k` for the allocate initial charge `dA_i = k * eps`
/// (perception.md, [ADR-0009](../../docs/adr/0009-surprise-gated-perception.md)).
///
/// CALIBRATED PRIOR — the single magnitude that cannot be derived from theory
/// alone. `eps` is the precision-weighted (Mahalanobis) embedding prediction
/// error, a computable proxy for Bayesian surprise; absent a stored precision
/// matrix `Sigma`, `k` absorbs both units and variance, so it must be declared and
/// fit from encoder statistics and the target initial-charge magnitude. A new site
/// with maximal surprise (`eps ≈ 1` under the isotropic fallback) lands near the
/// flat-prior ceiling [`INITIAL_RETAINED_ACTION`]; a low-surprise allocate enters
/// proportionally weaker. Replaces the old flat `salience = 1.0` initialization.
pub const SURPRISE_GAIN_K: f64 = INITIAL_RETAINED_ACTION;

/// Encoder distinct-pair cosine-similarity 95th percentile `q95`, the only input
/// to the separation boundary [`theta_sep`] (perception.md).
///
/// CALIBRATED PRIOR — a property of the embedding encoder, not a behavioral knob:
/// measure the cosine-similarity distribution over distinct sentence pairs and take
/// its 95th percentile. It recomputes exactly whenever the encoder changes. The
/// declared default reflects a typical sentence-embedding encoder whose distinct
/// pairs rarely exceed `~0.7` similarity.
pub const ENCODER_DISTINCT_PAIR_Q95: f64 = 0.70;

/// Novelty separation boundary `theta_sep = 1 - q95(similarity_distinct_pairs)`
/// (perception.md).
///
/// DERIVED — forced by the fixed 95th-percentile convention: `theta_sep` carries no
/// behavioral freedom; its only input is the encoder's distinct-pair distribution.
/// Novelty (`1 - max_similarity`) above `theta_sep` means the observation is farther
/// from known sites than 95% of distinct pairs and should allocate a new site;
/// otherwise it completes a known pattern and routes. Clamped to `[0, 1]`.
#[inline]
pub fn theta_sep(q95: f64) -> f64 {
    if !q95.is_finite() {
        return 1.0 - ENCODER_DISTINCT_PAIR_Q95;
    }
    (1.0 - q95).clamp(0.0, 1.0)
}

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
