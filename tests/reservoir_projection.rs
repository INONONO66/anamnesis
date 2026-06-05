//! Reservoir↔projection functions (Phase 1 foundation).
//!
//! Locked conventions (migration design Decision 5, ADR-0002/0003):
//! - `A_i` retained action = log need-odds (unbounded); `salience = logistic(A)`.
//! - `C_ij` conductance = log likelihood ratio (unbounded); `weight = logistic(C)`.
//! - Backfill inverses are `logit(clamp(x, EPS, 1-EPS))`, finite at the 0/1 ends.
//! - `project_conductance` is a finite-guard clamp (numerical safety), not [0,1].

use anamnesis::mechanics::priors::{
    LOG_ODDS_CLAMP, project_conductance, project_salience, project_weight, salience_to_action,
    weight_to_conductance,
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
        assert!(s > 0.0 && s < 1.0, "salience should be interior for a={a}: {s}");
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
fn project_conductance_is_a_finite_guard() {
    assert_eq!(project_conductance(1e9), LOG_ODDS_CLAMP);
    assert_eq!(project_conductance(-1e9), -LOG_ODDS_CLAMP);
    assert_eq!(project_conductance(0.5), 0.5);
    assert!(project_conductance(f64::INFINITY).is_finite());
}

#[test]
fn salience_projection_is_monotonic() {
    assert!(project_salience(1.0) > project_salience(0.0));
    assert!(project_salience(0.0) > project_salience(-1.0));
}
