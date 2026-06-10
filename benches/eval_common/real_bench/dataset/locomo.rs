use std::collections::BTreeSet;

use serde_json::Value;

use super::super::error::{BenchError, BenchResult};
use super::{
    BenchDatasetName, BenchQuestion, BenchSession, BenchTurn, GoldEvidence, LoadedBenchmark,
    answer_needles_for, answer_to_string, limit, string_array_field, string_field,
};

pub(super) fn parse_locomo(
    value: &Value,
    sample_limit: Option<usize>,
) -> BenchResult<LoadedBenchmark> {
    let samples = value
        .as_array()
        .ok_or_else(|| BenchError::Parse("LoCoMo root must be an array".to_string()))?;
    let mut sessions = Vec::new();
    let mut questions = Vec::new();

    for (sample_index, sample) in samples
        .iter()
        .enumerate()
        .take(limit(sample_limit, samples.len()))
    {
        let mut sample_session_ids = Vec::new();
        for session_key in ordered_session_keys(sample) {
            let session = parse_session(sample, sample_index, &session_key)?;
            if !session.turns.is_empty() {
                sample_session_ids.push(session.session_id.clone());
                sessions.push(session);
            }
        }
        parse_questions(sample, sample_index, &sample_session_ids, &mut questions);
    }

    Ok(LoadedBenchmark {
        dataset: BenchDatasetName::Locomo,
        sessions,
        questions,
    })
}

fn parse_session(
    sample: &Value,
    sample_index: usize,
    session_key: &str,
) -> BenchResult<BenchSession> {
    let turns_value = sample
        .get(session_key)
        .and_then(Value::as_array)
        .ok_or_else(|| BenchError::Parse(format!("{session_key} must be an array")))?;
    let session_id = format!("locomo-{sample_index}-{session_key}");
    let turns = turns_value
        .iter()
        .enumerate()
        .filter_map(|(turn_index, turn)| {
            let content = string_field(turn, "text").unwrap_or_default();
            (!content.trim().is_empty()).then(|| {
                let speaker = string_field(turn, "speaker").unwrap_or_else(|| "unknown".into());
                BenchTurn {
                    session_id: session_id.clone(),
                    raw_session_id: session_key.to_string(),
                    raw_turn_id: string_field(turn, "dia_id"),
                    turn_index,
                    role: speaker.clone(),
                    speaker,
                    content,
                }
            })
        })
        .collect();

    Ok(BenchSession {
        session_id,
        raw_session_id: session_key.to_string(),
        sample_index,
        turns,
    })
}

fn parse_questions(
    sample: &Value,
    sample_index: usize,
    sample_session_ids: &[String],
    out: &mut Vec<BenchQuestion>,
) {
    let Some(qas) = sample.get("qa").and_then(Value::as_array) else {
        return;
    };
    for (qa_index, qa) in qas.iter().enumerate() {
        let question = string_field(qa, "question").unwrap_or_default();
        if question.trim().is_empty() {
            continue;
        }
        let answer = qa.get("answer").cloned().unwrap_or(Value::Null);
        let evidence_turn_ids = evidence_turn_ids(qa);
        let category = qa.get("category").and_then(Value::as_u64).unwrap_or(0);
        out.push(BenchQuestion {
            question_id: format!("locomo-{sample_index}-qa-{qa_index}"),
            question,
            expected_answer: answer_to_string(&answer),
            question_type: locomo_category(category).to_string(),
            sample_index,
            session_ids: sample_session_ids.to_vec(),
            gold: GoldEvidence {
                answer_needles: answer_needles_for(&answer),
                evidence_session_ids: evidence_sessions(&evidence_turn_ids),
                evidence_turn_ids,
                answer_session_ids: Vec::new(),
            },
        });
    }
}

fn ordered_session_keys(sample: &Value) -> Vec<String> {
    let Some(object) = sample.as_object() else {
        return Vec::new();
    };
    let mut keys: Vec<_> = object
        .keys()
        .filter_map(|key| {
            key.strip_prefix("session_")
                .and_then(|suffix| suffix.parse::<usize>().ok())
                .map(|index| (index, key.clone()))
        })
        .collect();
    keys.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    keys.into_iter().map(|(_, key)| key).collect()
}

fn evidence_sessions(turn_ids: &[String]) -> Vec<String> {
    let mut sessions = BTreeSet::new();
    for turn_id in turn_ids {
        if let Some(prefix) = turn_id.split(':').next() {
            if let Some(number) = prefix.strip_prefix('D') {
                sessions.insert(format!("session_{number}"));
            }
        }
    }
    sessions.into_iter().collect()
}

fn evidence_turn_ids(qa: &Value) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for raw in string_array_field(qa, "evidence") {
        for part in raw.split(';') {
            let id = part.trim();
            if !id.is_empty() && seen.insert(id.to_string()) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

fn locomo_category(category: u64) -> &'static str {
    match category {
        1 => "single-hop",
        2 => "multi-hop",
        3 => "temporal",
        4 => "world-knowledge",
        5 => "adversarial",
        _ => "unknown",
    }
}
