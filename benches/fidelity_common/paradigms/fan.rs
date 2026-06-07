use super::super::scenario::{activation_from, ingest, scenario_engine};
use super::super::{Paradigm, ParadigmResult, Series, metrics};
use anamnesis::graph::{EdgeType, KnowledgeType};

pub struct FanEffect;

impl Paradigm for FanEffect {
    fn name(&self) -> &'static str {
        "fan_effect"
    }

    fn measure(&self) -> ParadigmResult {
        let fans = [1usize, 2, 3, 4, 5];
        let mut activations = Vec::new();
        for &k in &fans {
            // Fresh graph per fan level: one hub linked to k targets.
            let mut engine = scenario_engine();
            let hub = ingest(&mut engine, "hub", KnowledgeType::Semantic);
            let targets: Vec<_> = (0..k)
                .map(|i| ingest(&mut engine, &format!("t-{k}-{i}"), KnowledgeType::Semantic))
                .collect();
            for &t in &targets {
                engine.link(hub, t, EdgeType::Semantic).unwrap();
            }
            // First target's activation when seeded at the hub.
            activations.push(activation_from(&engine, hub, targets[0]));
        }

        let passed = metrics::is_strictly_monotone_decreasing(&activations);
        ParadigmResult {
            name: "fan_effect",
            series: vec![Series {
                name: "activation_by_fan".into(),
                xs: fans.iter().map(|&k| k as f64).collect(),
                ys: activations.clone(),
            }],
            metrics: serde_json::json!({ "activations": activations, "fans": fans }),
            passed,
            explanation: format!(
                "target activation {} monotonically as fan grows: {:?}",
                if passed {
                    "decreases"
                } else {
                    "does NOT decrease"
                },
                activations
            ),
        }
    }
}
