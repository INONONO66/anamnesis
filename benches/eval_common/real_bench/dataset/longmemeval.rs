use serde_json::Value;

use super::super::error::{BenchError, BenchResult};
use super::{
    BenchDatasetName, BenchQuestion, BenchSession, BenchTurn, GoldEvidence, LoadedBenchmark,
    answer_needles_for, limit, string_array_field, string_field,
};

pub(super) fn parse_longmemeval(
    value: &Value,
    sample_limit: Option<usize>,
) -> BenchResult<LoadedBenchmark> {
    let instances = value
        .as_array()
        .ok_or_else(|| BenchError::Parse("LongMemEval root must be an array".to_string()))?;
    let mut sessions = Vec::new();
    let mut questions = Vec::new();

    for instance in instances.iter().take(limit(sample_limit, instances.len())) {
        let question_id = string_field(instance, "question_id").unwrap_or_default();
        let haystack = instance
            .get("haystack_sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| BenchError::Parse("haystack_sessions missing".to_string()))?;
        let raw_session_ids = string_array_field(instance, "haystack_session_ids");
        let haystack_dates = string_array_field(instance, "haystack_dates");
        let mut session_ids = Vec::new();

        for (session_index, turns_value) in haystack.iter().enumerate() {
            let raw_session_id = raw_session_ids
                .get(session_index)
                .cloned()
                .unwrap_or_else(|| format!("{question_id}-{session_index}"));
            let session_id = format!("lme-{question_id}-{session_index}");
            let turns = parse_turns(turns_value, &session_id, &raw_session_id);
            if !turns.is_empty() {
                let start_timestamp = haystack_dates
                    .get(session_index)
                    .and_then(|d| super::dates::parse_longmemeval_date(d));
                session_ids.push(session_id.clone());
                sessions.push(BenchSession {
                    session_id,
                    raw_session_id,
                    sample_index: questions.len(),
                    turns,
                    start_timestamp,
                });
            }
        }

        let answer = string_field(instance, "answer").unwrap_or_default();
        let question_date = string_field(instance, "question_date")
            .and_then(|d| super::dates::parse_longmemeval_date(&d));
        questions.push(BenchQuestion {
            question_id,
            question: string_field(instance, "question").unwrap_or_default(),
            expected_answer: answer.clone(),
            question_type: string_field(instance, "question_type")
                .unwrap_or_else(|| "unknown".into()),
            sample_index: questions.len(),
            session_ids,
            gold: GoldEvidence {
                answer_needles: answer_needles_for(&Value::String(answer)),
                evidence_turn_ids: Vec::new(),
                evidence_session_ids: Vec::new(),
                answer_session_ids: string_array_field(instance, "answer_session_ids"),
            },
            question_date,
        });
    }

    Ok(LoadedBenchmark {
        dataset: BenchDatasetName::LongMemEval,
        sessions,
        questions,
    })
}

fn parse_turns(value: &Value, session_id: &str, raw_session_id: &str) -> Vec<BenchTurn> {
    let Some(turns) = value.as_array() else {
        return Vec::new();
    };
    turns
        .iter()
        .enumerate()
        .filter_map(|(turn_index, turn)| {
            let content = string_field(turn, "content").unwrap_or_default();
            (!content.trim().is_empty()).then(|| {
                let role = string_field(turn, "role").unwrap_or_else(|| "user".into());
                BenchTurn {
                    session_id: session_id.to_string(),
                    raw_session_id: raw_session_id.to_string(),
                    raw_turn_id: None,
                    turn_index,
                    speaker: role.clone(),
                    role,
                    content,
                }
            })
        })
        .collect()
}
