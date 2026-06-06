use super::super::{Paradigm, ParadigmResult};

pub struct TestingEffect;

impl Paradigm for TestingEffect {
    fn name(&self) -> &'static str {
        "testing_effect"
    }
    fn measure(&self) -> ParadigmResult {
        ParadigmResult {
            name: "testing_effect",
            series: vec![],
            metrics: serde_json::json!({}),
            passed: false,
            explanation: "stub — implemented in Phase 2".into(),
        }
    }
}
