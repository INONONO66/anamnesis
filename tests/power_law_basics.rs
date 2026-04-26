use anamnesis::graph::Timestamp;
use anamnesis::mechanics::forgetting::{base_level_to_salience, compute_base_level};
use std::collections::VecDeque;

#[test]
fn empty_history_returns_neg_infinity() {
    let h: VecDeque<Timestamp> = VecDeque::new();
    let result = compute_base_level(&h, Timestamp(1000), 0.5);
    assert!(
        result.is_infinite() && result < 0.0,
        "empty history should return NEG_INFINITY, got {result}"
    );
}

#[test]
fn single_access_act_r_formula_exact() {
    let mut h = VecDeque::new();
    h.push_back(Timestamp(0));
    // 1 week = 7 * 24 * 3600 * 1000 ms
    let now = Timestamp(7 * 24 * 3600 * 1000);
    let dt = now.0 as f64;
    // ACT-R exact: B = ln(dt^-0.5)
    let expected = (dt.powf(-0.5)).ln();
    let actual = compute_base_level(&h, now, 0.5);
    assert!(
        (actual - expected).abs() < 1e-9,
        "B = {actual} vs expected {expected}"
    );
}

#[test]
fn multiple_accesses_sum_correctly() {
    let mut h = VecDeque::new();
    h.push_back(Timestamp(0));
    h.push_back(Timestamp(1000));
    let now = Timestamp(2000);
    // dt1 = 2000, dt2 = 1000
    let expected = (2000_f64.powf(-0.5) + 1000_f64.powf(-0.5)).ln();
    let actual = compute_base_level(&h, now, 0.5);
    assert!(
        (actual - expected).abs() < 1e-9,
        "B = {actual} vs expected {expected}"
    );
}

#[test]
fn base_level_to_salience_in_unit_range() {
    // B = -∞ → salience = 0
    assert!(
        (base_level_to_salience(f64::NEG_INFINITY) - 0.0).abs() < 1e-9,
        "NEG_INFINITY should give salience 0"
    );
    // B = 0 → salience ≈ 0.5 (sigmoid center)
    assert!(
        (base_level_to_salience(0.0) - 0.5).abs() < 1e-9,
        "B=0 should give salience 0.5"
    );
    // B = +large → salience → 1.0
    assert!(
        base_level_to_salience(20.0) > 0.99,
        "large B should give salience near 1.0"
    );
    // B = -large → salience → 0.0
    assert!(
        base_level_to_salience(-20.0) < 0.01,
        "very negative B should give salience near 0.0"
    );
}
