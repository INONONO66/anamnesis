use super::super::scenario::{day, ingest_at, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series};
use anamnesis::graph::KnowledgeType;

pub struct Spacing;

impl Spacing {
    /// One arm of the spacing paradigm. `study_days[0]` is the node's BIRTH day, so
    /// its creation trace IS the first study event (Pavlik & Anderson 2005 framing:
    /// every study event is its own access trace, with NO synthetic day-0 trace ahead
    /// of the first study). The remaining days are committed re-presentations
    /// (`touch`). Reads `retained_action` at `test_day`.
    fn arm(&self, study_days: &[u64], test_day: u64) -> f64 {
        let mut e = scenario_engine();
        let seed = ingest_at(&mut e, "seed", KnowledgeType::Semantic, day(study_days[0]));
        for &d in &study_days[1..] {
            e.touch(seed, day(d)).unwrap();
        }
        e.tick(day(test_day)).unwrap();
        e.retained_action(seed).unwrap()
    }

    /// Number of access traces a schedule actually produces. Must equal the study
    /// count (creation trace + one per re-presentation); proves no spurious day-0
    /// trace is injected — without which the recency-controlled comparison is moot.
    fn trace_count(&self, study_days: &[u64]) -> usize {
        let mut e = scenario_engine();
        let seed = ingest_at(&mut e, "seed", KnowledgeType::Semantic, day(study_days[0]));
        for &d in &study_days[1..] {
            e.touch(seed, day(d)).unwrap();
        }
        e.graph().get_node(seed).unwrap().access_history.len()
    }
}

impl Paradigm for Spacing {
    fn name(&self) -> &'static str {
        "spacing"
    }

    fn measure(&self) -> ParadigmResult {
        // Three schedules, each 3 study events, evaluated at a delayed test (day 40):
        //   spaced    [1,13,25]  — distributed practice
        //   clustered [23,24,25] — massed, but with the SAME final study day (25) as
        //                          spaced, so recency is held constant between them
        //   massed    [1,2,3]    — massed early
        //
        // The GENUINE spacing claim is RECENCY-CONTROLLED: spaced vs clustered share
        // the final study day (25), so a pure recency / single-scalar model cannot
        // tell them apart. Under activation-dependent per-trace decay, spaced's
        // spread-out re-presentations are encoded at LOW activation -> LOW per-trace
        // decay d_j -> durable, so spaced ends ABOVE clustered at a delayed test.
        // (spaced vs massed is the classic comparison but is confounded by recency —
        // reported, not the pass criterion.)
        let spaced = self.arm(&[1, 13, 25], 40);
        let clustered = self.arm(&[23, 24, 25], 40);
        let massed = self.arm(&[1, 2, 3], 40);
        let genuine_margin = spaced - clustered;
        let genuine_spacing = genuine_margin > 0.02;

        // Spacing x retention-interval crossover: at a SHORT retention interval
        // clustered's three tightly-packed recent traces still lead; spaced overtakes
        // only at a sufficiently delayed test. A pure recency model cannot produce a
        // crossover, so its presence is positive evidence that the win comes from
        // activation-dependent decay rather than recency. Reported (not gated on the
        // narrow short-RI margin) to keep the test robust.
        let spaced_short = self.arm(&[1, 13, 25], 30);
        let clustered_short = self.arm(&[23, 24, 25], 30);
        let crossover_present = clustered_short >= spaced_short && spaced > clustered;

        // Pavlik-Anderson framing guard: each schedule must yield exactly 3 traces
        // (creation + 2 re-presentations); a stray day-0 trace would break recency
        // control and is what makes the recency-controlled inequality satisfiable.
        let framing_ok = self.trace_count(&[1, 13, 25]) == 3
            && self.trace_count(&[23, 24, 25]) == 3
            && self.trace_count(&[1, 2, 3]) == 3;

        // Gate on the crossover too: a single-scalar / recency model cannot produce a
        // spacing × RI crossover, so requiring it makes the test maximally falsifiable.
        let passed = genuine_spacing && framing_ok && crossover_present;

        ParadigmResult {
            name: "spacing",
            series: vec![
                Series {
                    name: "recency_controlled_day40".into(),
                    xs: vec![0.0, 1.0],
                    ys: vec![clustered, spaced],
                },
                Series {
                    name: "spaced_minus_clustered_by_RI".into(),
                    xs: vec![30.0, 40.0],
                    ys: vec![spaced_short - clustered_short, spaced - clustered],
                },
            ],
            metrics: serde_json::json!({
                "spaced_day40": spaced,
                "clustered_day40": clustered,
                "massed_day40": massed,
                "genuine_margin_spaced_minus_clustered": genuine_margin,
                "genuine_spacing_recency_controlled": genuine_spacing,
                "spaced_beats_massed_classic": spaced > massed,
                "spaced_short_RI_day30": spaced_short,
                "clustered_short_RI_day30": clustered_short,
                "crossover_present": crossover_present,
                "framing_ok_three_traces": framing_ok,
            }),
            passed,
            explanation: format!(
                "GENUINE spacing (recency-controlled: spaced [1,13,25] vs clustered [23,24,25], both last study day 25, test day 40): spaced {:.4} {} clustered {:.4} (margin {:+.4}) — {}. Activation-dependent per-trace decay, not recency: at a short RI (day 30) clustered still leads ({:.4} vs {:.4}), spaced overtakes only at the delayed test (spacing x retention-interval crossover present={}). Classic spaced>massed (recency-confounded) = {}. PA framing (3 traces, no day-0) = {}.",
                spaced,
                if genuine_spacing { ">" } else { "<=" },
                clustered,
                genuine_margin,
                if passed { "PASSES" } else { "FAILS" },
                spaced_short,
                clustered_short,
                crossover_present,
                spaced > massed,
                framing_ok,
            ),
        }
    }
}
