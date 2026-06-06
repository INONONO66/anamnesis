use super::super::scenario::{day, ingest, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series};
use anamnesis::graph::KnowledgeType;
use anamnesis::query::{Query, QueryConfig};

pub struct TestingEffect;

impl TestingEffect {
    /// Study, a retention interval that decays the trace BELOW the prior ceiling
    /// (so reinforcement has headroom), then `practice` events, then a common
    /// delayed test. `reinforce=true` is retrieval practice (`touch` = access
    /// reinforcement); `reinforce=false` is passive restudy (read-only query, no
    /// reservoir change). Returns delayed `retained_action`.
    fn arm(&self, reinforce: bool) -> f64 {
        let mut e = scenario_engine();
        let seed = ingest(&mut e, "seed", KnowledgeType::Semantic);
        // Retention interval 1: decay below the INITIAL_RETAINED_ACTION ceiling.
        e.tick(day(7)).unwrap();
        for _ in 0..3 {
            if reinforce {
                e.touch(seed, day(7)).unwrap(); // committed retrieval / access reinforcement
            } else {
                // passive re-exposure: read-only retrieval, mutates no reservoir
                let _ = e
                    .query(
                        &Query::Associative { seed, budget: 100 },
                        &QueryConfig::default(),
                    )
                    .unwrap();
            }
        }
        // Retention interval 2: common delayed test.
        e.tick(day(37)).unwrap();
        e.retained_action(seed).unwrap()
    }
}

impl Paradigm for TestingEffect {
    fn name(&self) -> &'static str {
        "testing_effect"
    }

    fn measure(&self) -> ParadigmResult {
        let retrieved = self.arm(true);
        let restudied = self.arm(false);
        let passed = retrieved > restudied;
        ParadigmResult {
            name: "testing_effect",
            series: vec![Series {
                name: "delayed_retained_action".into(),
                xs: vec![0.0, 1.0],
                ys: vec![restudied, retrieved],
            }],
            metrics: serde_json::json!({ "retrieved": retrieved, "restudied": restudied }),
            passed,
            explanation: format!(
                "at delayed test, retrieval-practiced {} passive restudy (retained_action {:.4} {} {:.4})",
                if passed {
                    "outlasts"
                } else {
                    "does NOT outlast"
                },
                retrieved,
                if passed { ">" } else { "<=" },
                restudied
            ),
        }
    }
}
