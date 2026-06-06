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
//! The unbounded reservoirs are finite-guarded by [`clamp_log_odds`]; the flow's
//! positive-bounded transition conductance is [`project_conductance`] = `logistic(C)`.
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

/// Largest forward [`edge_type_factor`] over all edge types — the `Supersedes`
/// forward factor. Used to normalize the type-affinity feature into `[0, 1]`.
///
/// DERIVED — the max of the declared `edge_type_factor` ordinal prior; not an
/// independent knob. If the ordering is refit this must track its new maximum.
pub const EDGE_TYPE_FACTOR_MAX: f64 = 1.20;

/// Edge-type-affinity NPMI feature `type_npmi` for the cold-start coupling seed
/// (conductance.md "Cold Start").
///
/// DERIVED — the forward [`edge_type_factor`] of the requested relation,
/// normalized into `[0, 1]` against [`EDGE_TYPE_FACTOR_MAX`] so it can enter the
/// `coupling_seed` regression as a unit NPMI feature. `Contradicts` returns `0.0`
/// (excluded from propagation), so a contradiction link contributes no type
/// coupling and its seed comes only from the other features.
#[inline]
pub fn edge_type_affinity_npmi(edge_type: &crate::graph::EdgeType) -> f64 {
    (edge_type_factor(edge_type, true) / EDGE_TYPE_FACTOR_MAX).clamp(0.0, 1.0)
}

// --- Cold-start coupling seed (conductance.md, Phase 5 link()) --------------
//
// When a link is created before any co-activation history exists, its initial
// conductance `C_ij` is a calibrated log-LR prior estimated from features
// (conductance.md "Cold Start"). The four coefficients are ONE calibrated
// regression vector `beta_coupling` over the normalized NPMI features
// `{sim, entity, scope, type}`, jointly fit at cold start — not four independent
// knobs. The illustrative normalization sums to 1.

/// `beta_coupling[sim]` — weight on the embedding-similarity NPMI feature.
/// CALIBRATED PRIOR — one entry of the single `beta_coupling` regression vector.
pub const BETA_COUPLING_SIM: f64 = 0.45;
/// `beta_coupling[entity]` — weight on the entity-overlap NPMI feature.
/// CALIBRATED PRIOR — `beta_coupling` regression vector.
pub const BETA_COUPLING_ENTITY: f64 = 0.25;
/// `beta_coupling[scope]` — weight on the scope-compatibility NPMI feature.
/// CALIBRATED PRIOR — `beta_coupling` regression vector.
pub const BETA_COUPLING_SCOPE: f64 = 0.15;
/// `beta_coupling[type]` — weight on the edge-type-affinity NPMI feature.
/// CALIBRATED PRIOR — `beta_coupling` regression vector.
pub const BETA_COUPLING_TYPE: f64 = 0.15;

/// Cold-start coupling seed `coupling_seed = sum_f beta_f * npmi_f` (conductance.md).
///
/// CALIBRATED PRIOR mapping — the four normalized NPMI features
/// `{sim, entity, scope, type}` are combined by the single `beta_coupling`
/// regression vector. Inputs are clamped to `[0, 1]`; the result is the cold-start
/// coupling strength, mapped to an initial conductance reservoir by
/// [`initialize_conductance`].
pub fn coupling_seed(sim_npmi: f64, entity_npmi: f64, scope_npmi: f64, type_npmi: f64) -> f64 {
    let f = |v: f64| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    BETA_COUPLING_SIM * f(sim_npmi)
        + BETA_COUPLING_ENTITY * f(entity_npmi)
        + BETA_COUPLING_SCOPE * f(scope_npmi)
        + BETA_COUPLING_TYPE * f(type_npmi)
}

/// Cold-start edge-density gate `conductance_threshold` (conductance.md "Cold
/// Start": `if coupling_seed >= conductance_threshold: create edge`).
///
/// CALIBRATED PRIOR (declared density knob, [ADR-0010](../../docs/adr/0010-calibrated-priors-not-laws.md)) —
/// the minimum cold-start coupling strength below which an auto-link is *not*
/// created, realizing the "minimum coupling" density control (dissipation.md /
/// conductance.md "Density Control"). It gates the auto-edge path on the
/// `coupling_seed` crossing this floor, suppressing genuinely noisy weak paths
/// before any edge exists. `coupling_seed` lives in `[0, 1]`; this is a small
/// positive floor. Not a behavioral law — refit as graph density statistics change.
pub const CONDUCTANCE_THRESHOLD: f64 = 0.05;

/// True when a cold-start `coupling_seed` clears the [`CONDUCTANCE_THRESHOLD`]
/// density gate and an auto-link should be created (conductance.md "Cold Start").
///
/// DERIVED — the boolean form of the documented gate
/// `coupling_seed >= conductance_threshold`. A non-finite seed never passes.
#[inline]
pub fn coupling_clears_threshold(coupling_seed: f64) -> bool {
    coupling_seed.is_finite() && coupling_seed >= CONDUCTANCE_THRESHOLD
}

/// Idle-edge leak rate `eta_leak` for the `TimeElapsed` conductance dissipation
/// (conductance.md post-commit plasticity `- eta_leak * idle_edge_leakage_ij`;
/// interactions.md `TimeElapsed`: `C_ij' = leak_idle_edge(C_ij, idle_days)`).
///
/// CALIBRATED PRIOR (declared density/temperature knob, [ADR-0010](../../docs/adr/0010-calibrated-priors-not-laws.md)) —
/// the rate at which unused weak coupling is removed over time, realizing the
/// "edge leakage" density control (dissipation.md / conductance.md "Density
/// Control"). It scales the per-edge idle leakage [`crate::mechanics::interactions::idle_edge_leakage`]; a value
/// of `0.0` disables edge leakage entirely. Not a behavioral law — refit from
/// observed edge re-use hazard, mirroring the node decay prior `d`.
pub const ETA_LEAK: f64 = 0.10;

/// Map a cold-start `coupling_seed` (in `[0, 1]`) to an initial conductance
/// reservoir `C_ij` in log-LR units (conductance.md `initialize_conductance`).
///
/// DERIVED — the coupling seed is a probability-like coupling strength; its log-LR
/// image is `logit(coupling_seed)`, the inverse of the `project_weight` logistic,
/// so that `project_weight(initialize_conductance(s)) ≈ s`. A zero/sub-threshold
/// seed maps to a strongly negative (near-zero weight) cold-start path; the
/// committed Hebbian flux later replaces the prior with measured strength. Finite
/// at the `0`/`1` ends via the clamped logit.
#[inline]
pub fn initialize_conductance(coupling_seed: f64) -> f64 {
    clamped_logit(coupling_seed)
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

// --- Peer trust as evidence (social.md, peer-identity.md) -------------------
//
// Trust is a calibrated evidence signal, not an authorization decision
// (social.md "Peer Trust"). It is an authoritative LOG-ODDS reservoir on the
// peer profile — same units as `A_i`/`C_ij` — that corroboration and feedback
// move through the single traceable update path `update_peer_trust`. The coarse
// `TrustLevel` enum supplies the *prior* (the cold-start reservoir); evidence
// then moves it. The readout `trust_weight` term is the bounded projection of
// this reservoir, so origin/provenance is never erased — only the evidence
// estimate moves (social.md: "Trust updates must leave traces").

/// Per-evidence trust-update rate `eta_trust` — the SLOW peer-reliability rate
/// (social.md "Fast/Slow Learning": slow = accumulated peer reliability).
///
/// CALIBRATED PRIOR (declared) — peer trust is *durable*: a single corroboration
/// or feedback event should nudge the trust reservoir, not swing it. It is the
/// slow consolidation rate of complementary learning systems, so it is a small
/// fraction of the core fast learning rate [`learning_rate`]`(N)`. The trust
/// reservoir moves a fraction `eta_trust` toward the per-event evidence target
/// (`update_trust_reservoir`); accumulated events compound into durable trust.
/// Refit from observed peer-reliability hazard, mirroring the node decay prior.
pub const TRUST_LEARNING_RATE: f64 = 0.05;

/// Per-event corroboration evidence target in log-odds units
/// (social.md "Peer Trust": corroboration raises trust).
///
/// CALIBRATED PRIOR (declared) — a single full-strength multi-agent
/// corroboration is positive evidence of peer reliability, targeting
/// `+CORROBORATION_LOG_ODDS` log trust-odds; a single full-strength negative
/// feedback / contradiction targets the symmetric `-CORROBORATION_LOG_ODDS`.
/// One `update_trust_reservoir` step moves the reservoir a fraction
/// [`TRUST_LEARNING_RATE`] toward this target, so trust saturates at the target
/// projection only after many consistent events — never from one signal.
pub const CORROBORATION_LOG_ODDS: f64 = REWARD_LOG_ODDS_SCALE;

/// Move a peer's trust reservoir a fraction `eta` toward an evidence `target`
/// (log-odds), the Rescorla-Wagner form shared with feedback
/// ([`crate::mechanics::interactions::rescorla_wagner`]).
///
/// DERIVED — trust is evidence, so it integrates like every other reservoir:
/// `trust' = trust + eta * (target - trust)`. With `target = ±CORROBORATION_LOG_ODDS`
/// and `eta = TRUST_LEARNING_RATE` this is the minimal evidence-driven update
/// social.md specifies; positive evidence (corroboration / useful feedback) raises
/// trust, negative evidence lowers it, both bounded and traceable. The result is
/// finite-guarded by [`clamp_log_odds`] so the reservoir stays in safe range.
#[inline]
pub fn update_trust_reservoir(trust: f64, target: f64, eta: f64) -> f64 {
    if !trust.is_finite() || !target.is_finite() || !eta.is_finite() {
        return clamp_log_odds(if trust.is_finite() { trust } else { 0.0 });
    }
    clamp_log_odds(trust + eta * (target - trust))
}

/// Per-event trust evidence target for a signed evidence strength in `[-1, 1]`
/// (positive = corroboration / useful, negative = contradiction / not-useful).
///
/// DERIVED — scales the declared per-event [`CORROBORATION_LOG_ODDS`] by the
/// signed strength, the same shape as [`crate::mechanics::interactions::lambda_reward`]
/// for site feedback. A full-strength corroboration targets `+CORROBORATION_LOG_ODDS`;
/// strength is clamped to `[-1, 1]` so a single event cannot overshoot the prior.
#[inline]
pub fn trust_evidence_target(signed_strength: f64) -> f64 {
    let s = if signed_strength.is_finite() {
        signed_strength.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    s * CORROBORATION_LOG_ODDS
}

/// `trust_weight = project_trust(trust_reservoir)` — the readout term's
/// evidence-driven component (social.md "Retrieval Effects": ranking through
/// trust-weighted readout).
///
/// DERIVED — the trust reservoir is log trust-odds; its bounded view is the
/// centered logistic `logistic(trust) - 0.5 ∈ (-0.5, 0.5)`, so a peer with no
/// evidence (`trust = 0`) contributes `0` to the readout, positive evidence adds
/// a bounded positive bonus, and negative evidence a bounded penalty. This is the
/// `w_trust` input in `readout_score`; the coarse `TrustLevel` bonus is added on
/// top as the declared prior offset, so both the level and the moved evidence
/// show up in ranking without either erasing the other.
#[inline]
pub fn project_trust(trust_reservoir: f64) -> f64 {
    project_salience(clamp_log_odds(trust_reservoir)) - 0.5
}

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

/// Finite-guard for a log-odds reservoir value: clamps to `[-LOG_ODDS_CLAMP,
/// LOG_ODDS_CLAMP]`. This is NOT a `[0, 1]` bound — `A_i` and `C_ij` are unbounded
/// log-odds; this only traps numerical blowups well inside `f64` range.
///
/// The doc's Hebbian Oja bound `dC = η·flux·(1 - C_ij)` (conductance.md) is
/// realized in Phase 3 as saturation via the *projection* `(1 - project_weight(C))`,
/// keeping `C` in log-LR units (migration design Decision 5). The Hebbian
/// reservoir update `C_next = clamp_log_odds(C_ij + dC_ij)` (overview.md) uses this
/// guard to keep the updated reservoir finite.
#[inline]
pub fn clamp_log_odds(value: f64) -> f64 {
    value.clamp(-LOG_ODDS_CLAMP, LOG_ODDS_CLAMP)
}

/// Flow conductance mapping `g = project_conductance(C)` for the additive directed
/// RWR (`g_ij = project_conductance(C_ij) * edge_type_factor_ij`,
/// [activation-flow.md](../../docs/05-context-retrieval/activation-flow.md)).
///
/// DERIVED — the activation flow needs a strictly **positive, bounded** per-edge
/// conductance so that row normalization `P(i,j) = g_ij / sum_k g_ik` is
/// well-defined and `P` stays row-stochastic for *every* finite reservoir,
/// including negative log-LR. `C_ij` is unbounded log-LR; its probability-like
/// image is `logistic(C)` ∈ (0, 1), which is `> 0` for all finite `C` (the
/// reservoir is first finite-guarded by [`clamp_log_odds`]). This is the same
/// logistic projection as [`project_weight`]; the distinct name marks its role as
/// the flow's transition conductance. `edge_type_factor` then scales it within the
/// row, and `Contradicts` (factor `0.0`) drops out of `P` entirely.
#[inline]
pub fn project_conductance(conductance: f64) -> f64 {
    logistic(clamp_log_odds(conductance))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cold-start coupling seed + conductance_threshold density gate ─────────
    //
    // Proving traces for the conductance.md "Cold Start" formulas:
    //   coupling_seed = sum_f beta_coupling[f] * npmi_f
    //   if coupling_seed >= conductance_threshold: create edge
    //   C_ij = initialize_conductance(coupling_seed); weight = project_weight(C_ij)

    #[test]
    fn coupling_seed_is_the_beta_weighted_npmi_sum() {
        // The four declared betas combine the normalized NPMI features.
        let s = coupling_seed(1.0, 1.0, 1.0, 1.0);
        let expected =
            BETA_COUPLING_SIM + BETA_COUPLING_ENTITY + BETA_COUPLING_SCOPE + BETA_COUPLING_TYPE;
        assert!((s - expected).abs() < 1e-12);
        // Per-feature contribution: only the sim feature fires.
        assert!((coupling_seed(1.0, 0.0, 0.0, 0.0) - BETA_COUPLING_SIM).abs() < 1e-12);
        // All-zero features → zero seed.
        assert_eq!(coupling_seed(0.0, 0.0, 0.0, 0.0), 0.0);
    }

    #[test]
    fn coupling_seed_clamps_features_to_unit_and_rejects_nonfinite() {
        // Out-of-range and non-finite NPMI inputs are clamped/zeroed, never explode.
        let over = coupling_seed(5.0, -2.0, f64::NAN, f64::INFINITY);
        // Only sim survives clamped to 1.0; entity clamps to 0; nan/inf → 0.
        assert!((over - BETA_COUPLING_SIM).abs() < 1e-12);
    }

    #[test]
    fn coupling_clears_threshold_matches_documented_gate() {
        // `coupling_seed >= conductance_threshold` is the create-edge predicate.
        assert!(!coupling_clears_threshold(0.0));
        assert!(!coupling_clears_threshold(CONDUCTANCE_THRESHOLD - 1e-9));
        assert!(coupling_clears_threshold(CONDUCTANCE_THRESHOLD));
        assert!(coupling_clears_threshold(CONDUCTANCE_THRESHOLD + 1e-9));
        // A non-finite seed never passes the gate.
        assert!(!coupling_clears_threshold(f64::NAN));
        assert!(!coupling_clears_threshold(f64::INFINITY));
    }

    // ── Flow conductance mapping `project_conductance` (activation-flow.md) ───
    //
    // Proving trace for `g_ij = project_conductance(C_ij) * edge_type_factor_ij`
    // keeping `P` row-stochastic: `project_conductance` must be strictly positive
    // and bounded for every finite reservoir `C`, so each `g_ij >= 0` and the row
    // sum is a valid normalizer.

    #[test]
    fn project_conductance_is_positive_and_bounded_for_all_finite_c() {
        // Strictly positive and at most 1 across the full finite log-LR range — the
        // row-stochastic requirement g > 0 for all finite C so that the row sum is a
        // valid normalizer (activation-flow.md). The upper bound saturates to exactly
        // 1.0 at extreme reservoirs (logistic(±LOG_ODDS_CLAMP)), which is in [0, 1].
        for c in [-1e9_f64, -100.0, -13.8, -1.0, 0.0, 1.0, 13.8, 100.0, 1e9] {
            let g = project_conductance(c);
            assert!(
                g > 0.0 && g <= 1.0,
                "project_conductance({c}) = {g} not in (0,1]"
            );
        }
        // Moderate reservoirs land strictly interior.
        for c in [-13.0_f64, -1.0, 0.0, 1.0, 13.0] {
            let g = project_conductance(c);
            assert!(g > 0.0 && g < 1.0, "interior expected for c={c}: {g}");
        }
        // C = 0 (no evidence) projects to the 0.5 midpoint.
        assert!((project_conductance(0.0) - 0.5).abs() < 1e-12);
        // Monotone increasing in the reservoir.
        assert!(project_conductance(1.0) > project_conductance(-1.0));
        // Non-finite reservoirs are finite-guarded, never NaN/inf.
        assert!(project_conductance(f64::INFINITY).is_finite());
        assert!(project_conductance(f64::NEG_INFINITY).is_finite());
        assert!(project_conductance(f64::INFINITY) > 0.0);
    }

    #[test]
    fn edge_type_factor_is_positive_for_propagation_and_zero_only_for_contradicts() {
        use crate::graph::EdgeType;
        // The second half of the `g_ij >= 0` row-stochasticity invariant
        // (activation-flow.md "Conductance Matrix"): every *propagating* edge type
        // factor is strictly positive, so `g_ij = project_conductance(C) * factor`
        // stays non-negative; only `Contradicts` is `0` and it is excluded from `P`.
        let propagating = [
            EdgeType::Semantic,
            EdgeType::Causal,
            EdgeType::Temporal,
            EdgeType::Reason,
            EdgeType::ReinforcedBy,
            EdgeType::ConsolidatedFrom,
            EdgeType::ExtractedFrom,
            EdgeType::Entity,
            EdgeType::Supersedes,
            EdgeType::RejectedAlternative,
            EdgeType::Supports,
            EdgeType::Refutes,
            EdgeType::BelongsTo,
            EdgeType::Custom("x".to_string()),
        ];
        for et in &propagating {
            // Both directions of every propagating edge are strictly positive.
            assert!(
                edge_type_factor(et, true) > 0.0,
                "forward {et:?} factor must be > 0"
            );
            assert!(
                edge_type_factor(et, false) > 0.0,
                "backward {et:?} factor must be > 0"
            );
        }
        // Contradicts is the only zero factor — excluded from propagation.
        assert_eq!(edge_type_factor(&EdgeType::Contradicts, true), 0.0);
        assert_eq!(edge_type_factor(&EdgeType::Contradicts, false), 0.0);
    }

    #[test]
    fn flow_weight_g_is_non_negative_for_every_finite_reservoir_and_propagating_type() {
        use crate::graph::EdgeType;
        // The composed invariant: `g_ij = project_conductance(C_ij) * edge_type_factor_ij
        // >= 0` for every finite reservoir `C` (including negative log-LR) and every
        // propagating edge type, so each row sum is a valid non-negative normalizer and
        // `P(i,j) = g_ij / sum_k g_ik` is row-stochastic (activation-flow.md).
        for c in [-1e9_f64, -40.0, -13.8, -1.0, 0.0, 1.0, 13.8, 40.0, 1e9] {
            for et in [
                EdgeType::Semantic,
                EdgeType::Reason,
                EdgeType::Temporal,
                EdgeType::RejectedAlternative,
                EdgeType::Refutes,
                EdgeType::Supersedes,
            ] {
                for is_forward in [true, false] {
                    let g = project_conductance(c) * edge_type_factor(&et, is_forward);
                    assert!(
                        g >= 0.0,
                        "g({c}, {et:?}, fwd={is_forward}) = {g} must be >= 0"
                    );
                    assert!(g.is_finite(), "g must be finite");
                }
            }
            // Contradicts collapses g to exactly 0 — it never enters P.
            let g_contra = project_conductance(c) * edge_type_factor(&EdgeType::Contradicts, true);
            assert_eq!(g_contra, 0.0);
        }
    }

    #[test]
    fn project_conductance_is_logistic_of_clamped_reservoir() {
        // Code symbol == doc symbol: `project_conductance(C)` is the logistic of the
        // finite-guarded reservoir, i.e. the same projection as `project_weight`
        // composed with `clamp_log_odds`.
        for c in [-50.0_f64, -2.0, 0.0, 2.0, 50.0] {
            assert_eq!(project_conductance(c), project_weight(clamp_log_odds(c)));
        }
    }

    #[test]
    fn clamp_log_odds_is_a_finite_guard_not_a_unit_bound() {
        // The renamed reservoir finite-guard clamps to [-LOG_ODDS_CLAMP, LOG_ODDS_CLAMP],
        // NOT to [0, 1]: values inside the band pass through, including negatives.
        assert_eq!(clamp_log_odds(1e9), LOG_ODDS_CLAMP);
        assert_eq!(clamp_log_odds(-1e9), -LOG_ODDS_CLAMP);
        assert_eq!(clamp_log_odds(0.5), 0.5);
        assert_eq!(clamp_log_odds(-3.0), -3.0);
        assert!(clamp_log_odds(f64::INFINITY).is_finite());
        assert!(clamp_log_odds(f64::NEG_INFINITY).is_finite());
    }

    // ── Peer trust as evidence (social.md "Peer Trust") ──────────────────────
    //
    // Proving traces: corroboration raises the trust reservoir, contradiction
    // lowers it, both bounded; the readout `project_trust` term moves with the
    // reservoir and is `0` at the no-evidence prior.

    #[test]
    fn update_trust_reservoir_moves_a_fraction_toward_the_target() {
        // Rescorla-Wagner form: one step closes a fraction `eta` of the gap.
        let trust = 0.0;
        let target = CORROBORATION_LOG_ODDS;
        let eta = TRUST_LEARNING_RATE;
        let next = update_trust_reservoir(trust, target, eta);
        let expected = trust + eta * (target - trust);
        assert!((next - expected).abs() < 1e-12);
        // Positive evidence raises, negative evidence lowers, both from the prior.
        assert!(update_trust_reservoir(0.0, CORROBORATION_LOG_ODDS, eta) > 0.0);
        assert!(update_trust_reservoir(0.0, -CORROBORATION_LOG_ODDS, eta) < 0.0);
    }

    #[test]
    fn repeated_corroboration_saturates_at_the_target_not_beyond() {
        // Durable, bounded: many consistent corroborations converge to the target
        // projection from below, never overshoot it (social.md: bounded, traceable).
        let mut trust = 0.0;
        for _ in 0..1000 {
            trust = update_trust_reservoir(trust, CORROBORATION_LOG_ODDS, TRUST_LEARNING_RATE);
        }
        assert!((trust - CORROBORATION_LOG_ODDS).abs() < 1e-6);
        // Slow rate: a single event does NOT swing trust to the target.
        let one = update_trust_reservoir(0.0, CORROBORATION_LOG_ODDS, TRUST_LEARNING_RATE);
        assert!(one < 0.5 * CORROBORATION_LOG_ODDS);
    }

    #[test]
    fn trust_evidence_target_scales_and_clamps_signed_strength() {
        assert!((trust_evidence_target(1.0) - CORROBORATION_LOG_ODDS).abs() < 1e-12);
        assert!((trust_evidence_target(-1.0) + CORROBORATION_LOG_ODDS).abs() < 1e-12);
        assert_eq!(trust_evidence_target(0.0), 0.0);
        // Out-of-range strength is clamped — one event cannot overshoot the prior.
        assert!((trust_evidence_target(5.0) - CORROBORATION_LOG_ODDS).abs() < 1e-12);
        assert_eq!(trust_evidence_target(f64::NAN), 0.0);
    }

    #[test]
    fn project_trust_is_zero_at_prior_and_monotone_bounded() {
        // No evidence (trust reservoir = 0) contributes nothing to the readout.
        assert!(project_trust(0.0).abs() < 1e-12);
        // Bounded in [-0.5, 0.5] and monotone increasing in the reservoir; the
        // extremes saturate to exactly ±0.5 at the log-odds clamp (logistic(±40)).
        assert!(project_trust(1.0) > project_trust(-1.0));
        for t in [-1e9_f64, -40.0, -1.0, 0.0, 1.0, 40.0, 1e9] {
            let w = project_trust(t);
            assert!(
                (-0.5..=0.5).contains(&w),
                "project_trust({t}) = {w} not in [-0.5, 0.5]"
            );
        }
        // Moderate reservoirs land strictly interior.
        for t in [-13.0_f64, -1.0, 0.0, 1.0, 13.0] {
            let w = project_trust(t);
            assert!(w > -0.5 && w < 0.5, "interior expected for t={t}: {w}");
        }
        // Non-finite reservoirs are finite-guarded.
        assert!(project_trust(f64::INFINITY).is_finite());
        assert!(project_trust(f64::NEG_INFINITY).is_finite());
    }

    #[test]
    fn initialize_conductance_is_the_logit_inverse_of_project_weight() {
        // C_ij = logit(coupling_seed), so project_weight(C_ij) recovers the seed
        // (round-trip through the logistic projection, ADR-0002).
        for seed in [0.05_f64, 0.2, 0.5, 0.8, 0.95] {
            let c = initialize_conductance(seed);
            assert!(c.is_finite());
            assert!((project_weight(c) - seed).abs() < 1e-9);
        }
        // Endpoints stay finite via the clamped logit.
        assert!(initialize_conductance(0.0).is_finite());
        assert!(initialize_conductance(1.0).is_finite());
    }
}
