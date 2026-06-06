use super::super::{Paradigm, ParadigmResult};

pub struct Forgetting;

impl Paradigm for Forgetting {
    fn name(&self) -> &'static str {
        "forgetting"
    }
    fn measure(&self) -> ParadigmResult {
        ParadigmResult {
            name: "forgetting",
            series: vec![],
            metrics: serde_json::json!({}),
            passed: false,
            explanation: "stub — implemented in Phase 2".into(),
        }
    }
}
