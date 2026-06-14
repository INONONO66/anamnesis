use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocomoTurn {
    pub sample_index: usize,
    pub session_id: String,
    pub turn_index: usize,
    pub speaker: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocomoSession {
    pub sample_index: usize,
    pub session_id: String,
    pub turns: Vec<LocomoTurn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LocomoQuestion {
    pub question_id: String,
    pub question: String,
    pub expected_answer: Value,
    pub category: u64,
    pub question_type: String,
    pub session_ids: Vec<String>,
    pub answer_needles: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LoadedLocomo {
    pub sessions: Vec<LocomoSession>,
    pub questions: Vec<LocomoQuestion>,
    pub speakers: BTreeSet<String>,
}

pub fn load_locomo(path: &Path, sample_limit: Option<usize>) -> Result<LoadedLocomo, String> {
    let file_path = path.join("locomo").join("locomo10.json");
    let content = std::fs::read_to_string(&file_path)
        .map_err(|err| format!("failed to read {}: {err}", file_path.display()))?;
    let samples: Vec<Value> = serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse {}: {err}", file_path.display()))?;
    parse_locomo_samples(&samples, sample_limit)
}

pub fn parse_locomo_samples(
    samples: &[Value],
    sample_limit: Option<usize>,
) -> Result<LoadedLocomo, String> {
    let mut sessions = Vec::new();
    let mut questions = Vec::new();
    let mut speakers = BTreeSet::new();
    let max_samples = sample_limit.unwrap_or(samples.len()).min(samples.len());

    for (sample_index, sample) in samples.iter().enumerate().take(max_samples) {
        let mut sample_session_ids = Vec::new();

        for session_key in ordered_session_keys(sample) {
            let Some(session_data) = sample.get(&session_key) else {
                continue;
            };
            let Some(turn_values) = session_data.as_array() else {
                return Err(format!("{session_key} is not an array"));
            };
            let session_id = format!("locomo-{sample_index}-{session_key}");
            let mut turns = Vec::new();

            for (turn_index, turn_value) in turn_values.iter().enumerate() {
                let speaker = scalar_to_string(turn_value.get("speaker"))
                    .unwrap_or_else(|| "unknown".to_string());
                let text = turn_value
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if text.is_empty() {
                    continue;
                }
                speakers.insert(speaker.clone());
                turns.push(LocomoTurn {
                    sample_index,
                    session_id: session_id.clone(),
                    turn_index,
                    speaker,
                    text,
                });
            }

            if !turns.is_empty() {
                sample_session_ids.push(session_id.clone());
                sessions.push(LocomoSession {
                    sample_index,
                    session_id,
                    turns,
                });
            }
        }

        if let Some(qa_values) = sample.get("qa").and_then(Value::as_array) {
            for (qa_index, qa) in qa_values.iter().enumerate() {
                let question = qa
                    .get("question")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if question.is_empty() {
                    continue;
                }
                let expected_answer = qa.get("answer").cloned().unwrap_or(Value::Null);
                let category = qa.get("category").and_then(Value::as_u64).unwrap_or(1);
                questions.push(LocomoQuestion {
                    question_id: format!("locomo-{sample_index}-qa-{qa_index}"),
                    question,
                    answer_needles: answer_needles(&expected_answer),
                    expected_answer,
                    category,
                    question_type: locomo_category_name(category).to_string(),
                    session_ids: sample_session_ids.clone(),
                });
            }
        }
    }

    Ok(LoadedLocomo {
        sessions,
        questions,
        speakers,
    })
}

pub fn locomo_category_name(category: u64) -> &'static str {
    match category {
        1 => "single-hop",
        2 => "multi-hop",
        3 => "temporal",
        4 => "world-knowledge",
        5 => "adversarial",
        _ => "unknown",
    }
}

pub fn answer_needles(value: &Value) -> Vec<String> {
    let mut set = BTreeSet::new();
    collect_answer_needles(value, &mut set);
    set.into_iter().collect()
}

pub fn normalize_for_match(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn contains_any_needle(text: &str, needles: &[String]) -> bool {
    let normalized = normalize_for_match(text);
    needles.iter().any(|needle| normalized.contains(needle))
}

fn ordered_session_keys(sample: &Value) -> Vec<String> {
    let Some(object) = sample.as_object() else {
        return Vec::new();
    };
    let mut keys: Vec<(usize, String)> = object
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

fn scalar_to_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn collect_answer_needles(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Null => {}
        Value::Bool(flag) => insert_normalized(&flag.to_string(), out),
        Value::Number(number) => insert_normalized(&number.to_string(), out),
        Value::String(text) => insert_normalized(text, out),
        Value::Array(values) => {
            for value in values {
                collect_answer_needles(value, out);
            }
        }
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(value) = object.get(key) {
                    collect_answer_needles(value, out);
                }
            }
        }
    }
}

fn insert_normalized(value: &str, out: &mut BTreeSet<String>) {
    let normalized = normalize_for_match(value);
    if !normalized.is_empty() {
        out.insert(normalized);
    }
}
