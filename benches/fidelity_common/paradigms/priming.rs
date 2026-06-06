use super::super::{Paradigm, ParadigmResult};

pub struct Priming;

impl Paradigm for Priming {
    fn name(&self) -> &'static str {
        "priming"
    }
    fn measure(&self) -> ParadigmResult {
        ParadigmResult {
            name: "priming",
            series: vec![],
            metrics: serde_json::json!({}),
            passed: false,
            explanation: "stub — implemented in Phase 2".into(),
        }
    }
}
