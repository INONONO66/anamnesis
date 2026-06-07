use super::super::scenario::{self, ingest, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series, metrics};
use anamnesis::graph::KnowledgeType;

pub struct Forgetting;

impl Paradigm for Forgetting {
    fn name(&self) -> &'static str {
        "forgetting"
    }

    fn measure(&self) -> ParadigmResult {
        // Cohort of isolated Episodic nodes; no touch/commit (read-only decay).
        //
        // We measure the AUTHORITATIVE reservoir `retained_action` (A_i), not its
        // bounded `salience` projection: freshly-ingested nodes start at the prior
        // ceiling INITIAL_RETAINED_ACTION≈13.8, where logistic(A)≈1.0 saturates, so
        // salience cannot show the curve. The falsifiable claim for ACT-R power-law
        // dissipation (ADR-0008) is that the base-level reservoir declines LINEARLY
        // in ln(t) (A = c - d*ln t) — the signature of power-law forgetting — and
        // that this log-linear form fits far better than a linear-in-time
        // (exponential-style) decay.
        let delays_days = [0.02_f64, 0.04, 0.4, 1.0, 2.0, 6.0, 31.0];
        // Retention at delay d = a SINGLE decay from creation to T0+d (fresh cohort
        // per delay), not cumulative ticks — so the curve is the true decay-from-
        // creation shape, free of path-dependent incremental-tick artifacts.
        let mut retention = Vec::new();
        for &d in &delays_days {
            let mut engine = scenario_engine();
            let ids: Vec<_> = (0..50)
                .map(|i| ingest(&mut engine, &format!("ep-{i}"), KnowledgeType::Episodic))
                .collect();
            engine.tick(day_frac(d)).unwrap();
            let mean = ids
                .iter()
                .map(|&id| engine.retained_action(id).unwrap())
                .sum::<f64>()
                / ids.len() as f64;
            retention.push(mean);
        }

        let fit = metrics::fit_log_vs_linear(&delays_days, &retention);
        // The slope must match the CALIBRATED decay, not be an arbitrary negative
        // number: a single-trace node decays at d_j = m_type·α, so retained_action
        // falls along slope −m_type·α in ln(t). Tie the assertion to the real
        // constants (Episodic m_type · DECAY_INTERCEPT) within ±5%.
        let expected_slope =
            -anamnesis::mechanics::priors::decay_multiplier_for_type(&KnowledgeType::Episodic)
                * anamnesis::mechanics::priors::DECAY_INTERCEPT;
        let slope_matches =
            (fit.slope_log - expected_slope).abs() <= 0.05 * expected_slope.abs() + 0.01;
        let passed = fit.r2_log >= 0.98
            && fit.r2_log > fit.r2_linear
            && fit.slope_log < 0.0
            && slope_matches;

        ParadigmResult {
            name: "forgetting",
            series: vec![
                Series {
                    name: "retained_action".into(),
                    xs: delays_days.to_vec(),
                    ys: retention.clone(),
                },
                Series {
                    name: "actr_log_fit".into(),
                    xs: delays_days.to_vec(),
                    ys: fit.pred_log.clone(),
                },
                Series {
                    name: "linear_fit".into(),
                    xs: delays_days.to_vec(),
                    ys: fit.pred_linear.clone(),
                },
            ],
            metrics: serde_json::json!({
                "r2_log": fit.r2_log, "r2_linear": fit.r2_linear, "slope_log": fit.slope_log,
                "expected_slope": expected_slope, "slope_matches_calibrated_decay": slope_matches,
            }),
            passed,
            explanation: format!(
                "retained_action {} ACT-R log-linear in time (r2_log={:.4} vs r2_linear={:.4}, slope={:.4} vs calibrated −m_type·α={:.4}) — power-law base-level dissipation, not exponential, at the calibrated decay rate",
                if passed { "is" } else { "is NOT" },
                fit.r2_log,
                fit.r2_linear,
                fit.slope_log,
                expected_slope
            ),
        }
    }
}

/// Fractional-day timestamp: T0 + d*DAY_MS.
fn day_frac(d: f64) -> anamnesis::graph::Timestamp {
    anamnesis::graph::Timestamp(scenario::T0 + (d * scenario::DAY_MS as f64) as u64)
}
