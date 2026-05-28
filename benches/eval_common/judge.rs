use serde::{Deserialize, Serialize};

use super::provider::LlmProvider;

/// Result of a single judge evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeResult {
    /// Whether the answer was judged correct.
    pub correct: bool,
    /// Confidence in the judgment [0, 1].
    pub confidence: f64,
    /// Reasoning for the judgment.
    pub reasoning: String,
}

impl JudgeResult {
    pub fn correct(reasoning: impl Into<String>) -> Self {
        JudgeResult {
            correct: true,
            confidence: 1.0,
            reasoning: reasoning.into(),
        }
    }

    pub fn incorrect(reasoning: impl Into<String>) -> Self {
        JudgeResult {
            correct: false,
            confidence: 1.0,
            reasoning: reasoning.into(),
        }
    }
}

/// Trait for evaluating engine answers against expected answers.
pub trait Judge {
    /// Evaluate whether `actual` correctly answers `question` given `expected`.
    fn evaluate(&self, question: &str, expected: &str, actual: &str) -> JudgeResult;
}

/// Mock judge using exact string matching (case-insensitive, trimmed).
///
/// Suitable for testing and CI where LLM calls are not available.
#[derive(Debug, Default, Clone)]
pub struct MockJudge;

impl Judge for MockJudge {
    fn evaluate(&self, _question: &str, expected: &str, actual: &str) -> JudgeResult {
        let expected_norm = expected.trim().to_lowercase();
        let actual_norm = actual.trim().to_lowercase();
        if expected_norm == actual_norm {
            JudgeResult::correct("exact match")
        } else {
            JudgeResult::incorrect(format!(
                "expected {:?}, got {:?}",
                expected.trim(),
                actual.trim()
            ))
        }
    }
}

/// Placeholder for an LLM-backed judge.
///
/// The generation and parsing logic is intentionally deferred to the provider
/// integration step.
pub struct LlmJudge {
    pub provider: Box<dyn LlmProvider>,
    pub system_prompt: String,
}

impl Judge for LlmJudge {
    fn evaluate(&self, question: &str, expected: &str, actual: &str) -> JudgeResult {
        let prompt = format!(
            "Question: {question}\nExpected: {expected}\nActual: {actual}\nIs the actual answer correct? Reply with 'correct' or 'incorrect'."
        );

        match self.provider.generate(&prompt) {
            Ok(response) => {
                let normalized = response.to_lowercase();
                if normalized.contains("incorrect") {
                    JudgeResult::incorrect(response)
                } else if normalized.contains("correct") {
                    JudgeResult::correct(response)
                } else {
                    JudgeResult::incorrect("unparseable judge response")
                }
            }
            Err(err) => JudgeResult::incorrect(format!("judge provider failed: {err}")),
        }
    }
}

/// Run majority voting over multiple judge results.
///
/// Returns true if more than half of the results are correct.
pub fn majority_vote(results: &[JudgeResult]) -> bool {
    if results.is_empty() {
        return false;
    }
    let correct_count = results.iter().filter(|r| r.correct).count();
    correct_count * 2 > results.len()
}

#[cfg(test)]
mod tests {
    use super::super::provider::MockProvider;
    use super::*;

    #[test]
    fn eval_common_mock_judge_marks_match_as_correct() {
        let j = MockJudge;
        let r = j.evaluate("Q?", "Hello", "Hello");
        assert!(r.correct);
    }

    #[test]
    fn eval_common_mock_judge_marks_mismatch_as_incorrect() {
        let j = MockJudge;
        let r = j.evaluate("Q?", "Hello", "World");
        assert!(!r.correct);
    }

    #[test]
    fn eval_common_mock_judge_case_insensitive() {
        let j = MockJudge;
        let r = j.evaluate("Q?", "Hello", "hello");
        assert!(r.correct, "should match case-insensitively");
    }

    #[test]
    fn eval_common_llm_judge_parses_correct_response() {
        let j = LlmJudge {
            provider: Box::new(MockProvider {
                response: "correct".to_string(),
            }),
            system_prompt: String::new(),
        };
        let r = j.evaluate("Q?", "Hello", "Hello");
        assert!(r.correct);
    }

    #[test]
    fn eval_common_majority_voting_3_of_3_correct() {
        let results = vec![
            JudgeResult::correct(""),
            JudgeResult::correct(""),
            JudgeResult::correct(""),
        ];
        assert!(majority_vote(&results));
    }

    #[test]
    fn eval_common_majority_voting_2_of_3_correct() {
        let results = vec![
            JudgeResult::correct(""),
            JudgeResult::correct(""),
            JudgeResult::incorrect(""),
        ];
        assert!(majority_vote(&results));
    }

    #[test]
    fn eval_common_majority_voting_1_of_3_correct() {
        let results = vec![
            JudgeResult::correct(""),
            JudgeResult::incorrect(""),
            JudgeResult::incorrect(""),
        ];
        assert!(!majority_vote(&results));
    }

    #[test]
    fn eval_common_majority_voting_empty_returns_false() {
        assert!(!majority_vote(&[]));
    }
}
