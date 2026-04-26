//! Validates that power-law decay fits the Ebbinghaus forgetting curve better than
//! exponential decay, using Murre & Dros (2015) reanalysis of Ebbinghaus (1885) data.
//!
//! Reference: https://doi.org/10.1371/journal.pone.0120644

use anamnesis::graph::Timestamp;
use anamnesis::mechanics::forgetting::{base_level_to_salience, compute_base_level};
use std::collections::VecDeque;

/// Ebbinghaus (1885) retention data, reanalyzed by Murre & Dros (2015).
/// Format: (time_in_days, retention_fraction)
const EBBINGHAUS_RETENTION: &[(f64, f64)] = &[
    (0.0056, 0.580), // 20 minutes
    (0.0417, 0.444), // 1 hour
    (0.375, 0.358),  // 9 hours
    (1.0, 0.337),    // 1 day
    (2.0, 0.278),    // 2 days
    (6.0, 0.254),    // 6 days
    (31.0, 0.211),   // 31 days
];

/// Fit exponential decay: retention = exp(-k * t), find k minimizing RMSE.
fn fit_exponential(data: &[(f64, f64)]) -> f64 {
    // Simple grid search for k in [0.001, 2.0]
    let mut best_rmse = f64::MAX;
    let mut k = 0.001_f64;
    while k <= 2.0 {
        let rmse = rmse_exponential(data, k);
        if rmse < best_rmse {
            best_rmse = rmse;
        }
        k += 0.001;
    }
    best_rmse
}

fn rmse_exponential(data: &[(f64, f64)], k: f64) -> f64 {
    let sum_sq: f64 = data
        .iter()
        .map(|&(t, r)| {
            let predicted = (-k * t).exp();
            (predicted - r).powi(2)
        })
        .sum();
    (sum_sq / data.len() as f64).sqrt()
}

/// Fit power-law decay using ACT-R base-level activation.
/// Simulate a single access at t=0, then compute salience at each time point.
fn fit_power_law(data: &[(f64, f64)]) -> f64 {
    // Grid-search a sigmoid scale because base-level activation is in log-millisecond
    // units while the retention observations are fractions.
    let mut best_rmse = f64::MAX;
    let mut scale = 0.001_f64;
    while scale <= 1.0 {
        let rmse = rmse_power_law(data, scale);
        if rmse < best_rmse {
            best_rmse = rmse;
        }
        scale += 0.001;
    }
    best_rmse
}

fn rmse_power_law(data: &[(f64, f64)], scale: f64) -> f64 {
    // Single access at t=0 (in ms)
    let access_time = Timestamp(0);
    let sum_sq: f64 = data
        .iter()
        .map(|&(t_days, r)| {
            let t_ms = (t_days * 86_400_000.0) as u64;
            let now = Timestamp(t_ms.max(1));
            let mut history = VecDeque::new();
            history.push_back(access_time);
            let b = compute_base_level(&history, now, 0.5);
            let predicted = base_level_to_salience(b * scale);
            (predicted - r).powi(2)
        })
        .sum();
    (sum_sq / data.len() as f64).sqrt()
}

#[test]
fn power_law_outperforms_exponential() {
    let exp_rmse = fit_exponential(EBBINGHAUS_RETENTION);
    let power_rmse = fit_power_law(EBBINGHAUS_RETENTION);
    println!("Exponential RMSE: {exp_rmse:.6}");
    println!("Power-law   RMSE: {power_rmse:.6}");
    assert!(
        power_rmse < exp_rmse,
        "Power-law ({power_rmse:.6}) should fit better than Exponential ({exp_rmse:.6})"
    );
}
