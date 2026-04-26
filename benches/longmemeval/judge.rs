//! LLM-as-judge evaluation.
//!
//! Provides the Judge trait and implementations for evaluating engine answers.
//! Placeholder — will be implemented in T17.

/// Result of a single judge evaluation.
#[derive(Debug, Clone)]
pub struct JudgeResult {
    pub correct: bool,
    pub confidence: f64,
    pub reasoning: String,
}

/// Trait for evaluating engine answers against expected answers.
pub trait Judge {
    fn evaluate(&self, question: &str, expected: &str, actual: &str) -> JudgeResult;
}
