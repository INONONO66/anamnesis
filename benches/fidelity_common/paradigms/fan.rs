use super::super::{Paradigm, ParadigmResult};

pub struct FanEffect;

impl Paradigm for FanEffect {
    fn name(&self) -> &'static str {
        "fan_effect"
    }
    fn measure(&self) -> ParadigmResult {
        ParadigmResult {
            name: "fan_effect",
            series: vec![],
            metrics: serde_json::json!({}),
            passed: false,
            explanation: "stub — implemented in Phase 2".into(),
        }
    }
}
