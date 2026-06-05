//! Pins the calibrated-prior constants to their declared values so that a
//! future refit is a deliberate, reviewed change rather than accidental drift.
//!
//! Per ADR-0010 these are calibrated priors, not physical laws — but their
//! single home is `anamnesis::mechanics::priors`, and this test guards that
//! home against silent divergence.

use anamnesis::mechanics::priors;

#[test]
fn decay_exponent_d_is_actr_default() {
    // ACT-R canonical power-law base-level decay exponent (ADR-0008).
    assert_eq!(priors::DECAY_EXPONENT_D, 0.5);
}

#[test]
fn rwr_restart_probability_default() {
    // RWR restart alpha default; reach-derived alpha replaces it in Phase 3 (ADR-0005).
    assert_eq!(priors::RWR_RESTART_PROBABILITY, 0.15);
}

#[test]
fn logit_backfill_eps_reserved() {
    // Epsilon for the clamped-logit projection backfill (Phase 1 reservoir migration).
    assert_eq!(priors::LOGIT_BACKFILL_EPS, 1e-6);
}
