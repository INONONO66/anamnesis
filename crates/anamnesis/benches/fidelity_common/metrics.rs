//! Pure measurement utilities — no engine dependency, so the *instrument* is
//! trustworthy on its own (unit-tested on synthetic data).

/// Ordinary least squares slope+intercept for y = slope*x + intercept.
fn linreg(xs: &[f64], ys: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let sx: f64 = xs.iter().sum();
    let sy: f64 = ys.iter().sum();
    let sxx: f64 = xs.iter().map(|x| x * x).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| x * y).sum();
    let denom = n * sxx - sx * sx;
    let slope = if denom.abs() < 1e-18 {
        0.0
    } else {
        (n * sxy - sx * sy) / denom
    };
    let intercept = (sy - slope * sx) / n;
    (slope, intercept)
}

fn rss(obs: &[f64], pred: &[f64]) -> f64 {
    obs.iter().zip(pred).map(|(o, p)| (o - p) * (o - p)).sum()
}

fn aic(obs: &[f64], pred: &[f64], k: usize) -> f64 {
    let n = obs.len() as f64;
    let r = rss(obs, pred).max(1e-12);
    n * (r / n).ln() + 2.0 * k as f64
}

fn r2(obs: &[f64], pred: &[f64]) -> f64 {
    let mean = obs.iter().sum::<f64>() / obs.len() as f64;
    let ss_tot: f64 = obs.iter().map(|o| (o - mean) * (o - mean)).sum();
    1.0 - rss(obs, pred) / ss_tot.max(1e-12)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FitResult {
    pub aic_power: f64,
    pub aic_exp: f64,
    pub r2_power: f64,
    pub r2_exp: f64,
    pub exponent_d: f64,
    /// Fitted predictions in original space, for plotting.
    pub pred_power: Vec<f64>,
    pub pred_exp: Vec<f64>,
}

/// Fit retention(delay) to a power law `y = a*t^-d` and a single exponential
/// `y = a*exp(-b t)` (both via log-linear regression), compare by AIC + R².
/// Requires all `delays_days > 0` and `retention > 0`.
pub fn fit_power_vs_exp(delays_days: &[f64], retention: &[f64]) -> FitResult {
    let lx: Vec<f64> = delays_days.iter().map(|t| t.ln()).collect();
    let ly: Vec<f64> = retention.iter().map(|y| y.ln()).collect();

    let (slope_p, icpt_p) = linreg(&lx, &ly); // ln y = ln a - d ln t
    let d = -slope_p;
    let a_p = icpt_p.exp();
    let pred_power: Vec<f64> = delays_days.iter().map(|t| a_p * t.powf(-d)).collect();

    let (slope_e, icpt_e) = linreg(delays_days, &ly); // ln y = ln a - b t
    let b = -slope_e;
    let a_e = icpt_e.exp();
    let pred_exp: Vec<f64> = delays_days.iter().map(|t| a_e * (-b * t).exp()).collect();

    FitResult {
        aic_power: aic(retention, &pred_power, 2),
        aic_exp: aic(retention, &pred_exp, 2),
        r2_power: r2(retention, &pred_power),
        r2_exp: r2(retention, &pred_exp),
        exponent_d: d,
        pred_power,
        pred_exp,
    }
}

/// True iff strictly decreasing.
pub fn is_strictly_monotone_decreasing(ys: &[f64]) -> bool {
    ys.windows(2).all(|w| w[1] < w[0])
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdditiveResult {
    pub sum: f64,
    pub max: f64,
    pub observed: f64,
    /// observed closer to sum than to max (additive, not max-pooled).
    pub additive: bool,
}

/// Compare a two-cue convergent activation against the SUM vs MAX of the two
/// single-cue contributions. `tol` is the absolute slack on "closer to sum".
pub fn classify_additive(single_a: f64, single_b: f64, both: f64) -> AdditiveResult {
    let sum = single_a + single_b;
    let max = single_a.max(single_b);
    let additive = (both - sum).abs() <= (both - max).abs() && both > max + 1e-9;
    AdditiveResult {
        sum,
        max,
        observed: both,
        additive,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogFit {
    /// R² of the ACT-R log-linear model `y = m*ln(x) + c` (power-law base-level).
    pub r2_log: f64,
    /// R² of the linear-in-time model `y = m*x + c` (exponential-style decay).
    pub r2_linear: f64,
    /// Slope of the log-linear fit (≈ -d for ACT-R base-level decay).
    pub slope_log: f64,
    pub pred_log: Vec<f64>,
    pub pred_linear: Vec<f64>,
}

/// Fit a decaying reservoir against the **ACT-R base-level** form (linear in
/// `ln(t)` — the multi-trace base-level `B_i = ln(Σ_j (now−t_j)^−d)`, which for a
/// single trace is `−d·ln(Δt)`, the signature of power-law forgetting) versus a
/// linear-in-time form (`A = c − b*t`, the shape an exponential/linear decay would
/// take). Power-law dissipation should fit the log form far better. Requires `xs > 0`.
pub fn fit_log_vs_linear(xs: &[f64], ys: &[f64]) -> LogFit {
    let lx: Vec<f64> = xs.iter().map(|x| x.ln()).collect();
    let (m_log, c_log) = linreg(&lx, ys);
    let pred_log: Vec<f64> = lx.iter().map(|l| m_log * l + c_log).collect();

    let (m_lin, c_lin) = linreg(xs, ys);
    let pred_linear: Vec<f64> = xs.iter().map(|x| m_lin * x + c_lin).collect();

    LogFit {
        r2_log: r2(ys, &pred_log),
        r2_linear: r2(ys, &pred_linear),
        slope_log: m_log,
        pred_log,
        pred_linear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_curve_prefers_power_fit() {
        let t: [f64; 5] = [0.05, 1.0, 2.0, 6.0, 31.0];
        let y: Vec<f64> = t.iter().map(|t| 0.9 * t.powf(-0.4)).collect();
        let f = fit_power_vs_exp(&t, &y);
        assert!(
            f.aic_power <= f.aic_exp,
            "power AIC {} !<= exp AIC {}",
            f.aic_power,
            f.aic_exp
        );
        assert!(f.r2_power > 0.99, "r2_power {}", f.r2_power);
        assert!((f.exponent_d - 0.4).abs() < 0.05, "d {}", f.exponent_d);
    }

    #[test]
    fn exponential_curve_prefers_exp_fit() {
        let t: [f64; 5] = [0.05, 1.0, 2.0, 6.0, 31.0];
        let y: Vec<f64> = t.iter().map(|t| 0.9 * (-0.5 * t).exp()).collect();
        let f = fit_power_vs_exp(&t, &y);
        assert!(
            f.aic_exp <= f.aic_power,
            "exp should win on exponential data"
        );
    }

    #[test]
    fn monotone_decreasing() {
        assert!(is_strictly_monotone_decreasing(&[0.5, 0.3, 0.2, 0.1]));
        assert!(!is_strictly_monotone_decreasing(&[0.5, 0.5, 0.2]));
        assert!(!is_strictly_monotone_decreasing(&[0.1, 0.2]));
    }

    #[test]
    fn additive_not_max() {
        let r = classify_additive(0.2, 0.25, 0.42); // ~sum 0.45, max 0.25
        assert!(r.additive, "{r:?}");
        let r2 = classify_additive(0.2, 0.25, 0.26); // ~max
        assert!(!r2.additive, "{r2:?}");
    }

    #[test]
    fn log_linear_prefers_log_on_actr_data() {
        // A = 5 - 0.5*ln(t): the ACT-R base-level shape (single-trace B_i = -d*ln(Δt)).
        let t: [f64; 6] = [0.02, 0.1, 1.0, 2.0, 6.0, 31.0];
        let y: Vec<f64> = t.iter().map(|t| 5.0 - 0.5 * t.ln()).collect();
        let f = fit_log_vs_linear(&t, &y);
        assert!(f.r2_log > 0.999, "r2_log {}", f.r2_log);
        assert!(
            f.r2_log > f.r2_linear,
            "log {} should beat linear {}",
            f.r2_log,
            f.r2_linear
        );
        assert!((f.slope_log + 0.5).abs() < 1e-6, "slope {}", f.slope_log);
    }
}
