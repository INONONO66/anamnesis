//! Reservoir↔projection functions (Phase 1 foundation).
//!
//! Locked conventions (migration design Decision 5, ADR-0002/0003):
//! - `A_i` retained action = log need-odds (unbounded); `salience = logistic(A)`.
//! - `C_ij` conductance = log likelihood ratio (unbounded); `weight = logistic(C)`.
//! - Backfill inverses are `logit(clamp(x, EPS, 1-EPS))`, finite at the 0/1 ends.
//! - `clamp_log_odds` is the reservoir finite-guard (numerical safety), not [0,1].
//! - `project_conductance` is the flow's positive-bounded `(0,1)` transition
//!   conductance `logistic(clamp_log_odds(C))` (activation-flow.md).

use anamnesis::mechanics::priors::{
    LOG_ODDS_CLAMP, clamp_log_odds, project_conductance, project_salience, project_weight,
    salience_to_action, weight_to_conductance,
};

#[test]
fn salience_projection_round_trips() {
    for s in [0.01_f64, 0.1, 0.5, 0.9, 0.99] {
        let a = salience_to_action(s);
        let back = project_salience(a);
        assert!((back - s).abs() < 1e-9, "s={s} -> a={a} -> back={back}");
    }
}

#[test]
fn weight_projection_round_trips() {
    for w in [0.01_f64, 0.1, 0.5, 0.9, 0.99] {
        let c = weight_to_conductance(w);
        let back = project_weight(c);
        assert!((back - w).abs() < 1e-9, "w={w} -> c={c} -> back={back}");
    }
}

#[test]
fn projections_land_in_closed_unit_interval() {
    // Spec invariant: projections stay in CLOSED ranges (overview.md shared
    // invariants). logistic saturates to exactly 0.0/1.0 at f64 extremes, which
    // is within [0,1]; the clamped-logit backfill keeps the inverse finite.
    for a in [-100.0_f64, -13.8, -1.0, 0.0, 1.0, 13.8, 100.0] {
        let s = project_salience(a);
        assert!((0.0..=1.0).contains(&s), "salience out of [0,1]: {s}");
        let w = project_weight(a);
        assert!((0.0..=1.0).contains(&w), "weight out of [0,1]: {w}");
    }
    // Realistic backfill-range reservoirs project strictly interior.
    for a in [-13.0_f64, -1.0, 0.0, 1.0, 13.0] {
        let s = project_salience(a);
        assert!(
            s > 0.0 && s < 1.0,
            "salience should be interior for a={a}: {s}"
        );
    }
}

#[test]
fn backfill_clamps_extremes_to_finite_reservoir() {
    // salience/weight at the 0 and 1 boundaries must NOT yield ±inf (logit saturation).
    assert!(salience_to_action(0.0).is_finite());
    assert!(salience_to_action(1.0).is_finite());
    assert!(weight_to_conductance(0.0).is_finite());
    assert!(weight_to_conductance(1.0).is_finite());
}

#[test]
fn clamp_log_odds_is_a_finite_guard() {
    // The reservoir finite-guard clamps to [-LOG_ODDS_CLAMP, LOG_ODDS_CLAMP],
    // NOT to [0, 1] — `A_i`/`C_ij` are unbounded log-odds.
    assert_eq!(clamp_log_odds(1e9), LOG_ODDS_CLAMP);
    assert_eq!(clamp_log_odds(-1e9), -LOG_ODDS_CLAMP);
    assert_eq!(clamp_log_odds(0.5), 0.5);
    assert!(clamp_log_odds(f64::INFINITY).is_finite());
}

#[test]
fn project_conductance_is_positive_bounded_for_row_stochastic_p() {
    // activation-flow.md: g_ij = project_conductance(C_ij) * edge_type_factor_ij,
    // P(i,j) = g_ij / sum_k g_ik. For P to be row-stochastic, project_conductance
    // must be strictly positive and bounded for every finite C (including negative
    // log-LR). It is the logistic of the finite-guarded reservoir.
    // Strictly positive (g > 0) so the row sum is a valid normalizer; the upper
    // bound saturates to exactly 1.0 at extreme reservoirs, which is in [0, 1].
    for c in [-1e9_f64, -13.8, -1.0, 0.0, 1.0, 13.8, 1e9] {
        let g = project_conductance(c);
        assert!(
            g > 0.0 && g <= 1.0,
            "project_conductance({c}) = {g} not in (0,1]"
        );
        assert_eq!(g, project_weight(clamp_log_odds(c)));
    }
    assert!(project_conductance(f64::INFINITY).is_finite());
    assert!(project_conductance(f64::INFINITY) > 0.0);
}

#[test]
fn salience_projection_is_monotonic() {
    assert!(project_salience(1.0) > project_salience(0.0));
    assert!(project_salience(0.0) > project_salience(-1.0));
}
