//! Pins the calibrated-prior constants to their declared values so that a
//! future refit is a deliberate, reviewed change rather than accidental drift.
//!
//! Per ADR-0010 these are calibrated priors, not physical laws — but their
//! single home is `anamnesis::mechanics::priors`, and this test guards that
//! home against silent divergence.

use anamnesis::mechanics::priors;

#[test]
fn per_trace_decay_constants_are_locked() {
    // Activation-dependent per-trace decay d_j = m_type·(c·e^{m} + α)
    // (Pavlik & Anderson 2005, ADR-0008): the locked floor α and scale c.
    assert_eq!(priors::DECAY_INTERCEPT, 0.40);
    assert_eq!(priors::DECAY_SCALE, 2.0);
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
