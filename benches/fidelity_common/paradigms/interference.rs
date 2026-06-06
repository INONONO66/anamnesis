use super::super::{Paradigm, ParadigmResult};

pub struct Interference;

impl Paradigm for Interference {
    fn name(&self) -> &'static str {
        "interference"
    }
    fn measure(&self) -> ParadigmResult {
        ParadigmResult {
            name: "interference",
            series: vec![],
            metrics: serde_json::json!({}),
            passed: false,
            explanation: "stub — implemented in Phase 2".into(),
        }
    }
}
