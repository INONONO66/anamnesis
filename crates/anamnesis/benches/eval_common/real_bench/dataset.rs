use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::locomo_pipeline::{answer_needles, normalize_for_match};
use super::error::{BenchError, BenchResult};

pub mod dates;
mod locomo;
mod longmemeval;

const MAX_DATASET_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BenchDatasetName {
    Locomo,
    LongMemEval,
}

impl BenchDatasetName {
    pub fn as_str(self) -> &'static str {
        match self {
            BenchDatasetName::Locomo => "locomo",
            BenchDatasetName::LongMemEval => "longmemeval",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "locomo" => Ok(BenchDatasetName::Locomo),
            "longmemeval" => Ok(BenchDatasetName::LongMemEval),
            other => Err(format!(
                "unknown dataset {other:?}; expected locomo or longmemeval"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedBenchmark {
    pub dataset: BenchDatasetName,
    pub sessions: Vec<BenchSession>,
    pub questions: Vec<BenchQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchSession {
    pub session_id: String,
    pub raw_session_id: String,
    pub sample_index: usize,
    pub turns: Vec<BenchTurn>,
    /// Dataset-declared session start, epoch seconds UTC, when parseable.
    #[serde(default)]
    pub start_timestamp: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchTurn {
    pub session_id: String,
    pub raw_session_id: String,
    pub raw_turn_id: Option<String>,
    pub turn_index: usize,
    pub speaker: String,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchQuestion {
    pub question_id: String,
    pub question: String,
    pub expected_answer: String,
    pub question_type: String,
    pub sample_index: usize,
    pub session_ids: Vec<String>,
    pub gold: GoldEvidence,
    /// Dataset-declared question date, epoch seconds UTC, when parseable.
    #[serde(default)]
    pub question_date: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldEvidence {
    pub answer_needles: Vec<String>,
    pub evidence_turn_ids: Vec<String>,
    pub evidence_session_ids: Vec<String>,
    pub answer_session_ids: Vec<String>,
}

impl GoldEvidence {
    pub fn total_relevant_units(&self) -> usize {
        if !self.evidence_turn_ids.is_empty() {
            return self.evidence_turn_ids.len();
        }
        if !self.answer_session_ids.is_empty() {
            return self.answer_session_ids.len();
        }
        if !self.evidence_session_ids.is_empty() {
            return self.evidence_session_ids.len();
        }
        self.answer_needles.len()
    }

    pub fn matched_units(
        &self,
        raw_session_id: &str,
        raw_turn_id: Option<&str>,
        text: &str,
    ) -> Vec<String> {
        if !self.evidence_turn_ids.is_empty() {
            return raw_turn_id
                .filter(|turn_id| self.evidence_turn_ids.iter().any(|gold| gold == turn_id))
                .map(|turn_id| vec![format!("turn:{turn_id}")])
                .unwrap_or_default();
        }
        if !self.answer_session_ids.is_empty() {
            if self
                .answer_session_ids
                .iter()
                .any(|gold| gold == raw_session_id)
            {
                return vec![format!("session:{raw_session_id}")];
            }
            return Vec::new();
        }
        if !self.evidence_session_ids.is_empty() {
            if self
                .evidence_session_ids
                .iter()
                .any(|gold| gold == raw_session_id)
            {
                return vec![format!("session:{raw_session_id}")];
            }
            return Vec::new();
        }
        let normalized = normalize_for_match(text);
        self.answer_needles
            .iter()
            .filter(|needle| normalized.contains(*needle))
            .map(|needle| format!("answer:{needle}"))
            .collect()
    }
}

pub fn load_benchmark_dataset(
    dataset: BenchDatasetName,
    data_dir: &Path,
    sample_limit: Option<usize>,
) -> BenchResult<LoadedBenchmark> {
    let path = dataset_path(dataset, data_dir);
    if !path.exists() {
        return Err(BenchError::DatasetNotFound {
            path,
            hint: format!(
                "Download with: cargo bench --bench download_datasets -- --dataset {}",
                dataset.as_str()
            ),
        });
    }
    let size = std::fs::metadata(&path)
        .map_err(|err| BenchError::Parse(format!("failed to stat {}: {err}", path.display())))?
        .len();
    if size > MAX_DATASET_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "dataset file {} is too large: {size} bytes > {MAX_DATASET_BYTES}",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|err| BenchError::Parse(format!("failed to read {}: {err}", path.display())))?;
    let value = serde_json::from_str(&text)
        .map_err(|err| BenchError::Parse(format!("failed to parse {}: {err}", path.display())))?;
    parse_benchmark_dataset(dataset, &value, sample_limit)
}

pub fn parse_benchmark_dataset(
    dataset: BenchDatasetName,
    value: &Value,
    sample_limit: Option<usize>,
) -> BenchResult<LoadedBenchmark> {
    match dataset {
        BenchDatasetName::Locomo => locomo::parse_locomo(value, sample_limit),
        BenchDatasetName::LongMemEval => longmemeval::parse_longmemeval(value, sample_limit),
    }
}

/// Split a loaded benchmark into independent per-sample benchmarks: each
/// LoCoMo conversation (or LongMemEval question haystack) becomes its own
/// memory store, matching the standard per-conversation evaluation protocol
/// instead of mixing unrelated histories into one graph.
pub fn split_by_sample(loaded: LoadedBenchmark) -> Vec<LoadedBenchmark> {
    let mut indices: Vec<usize> = loaded
        .questions
        .iter()
        .map(|question| question.sample_index)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    indices.sort_unstable();
    indices
        .into_iter()
        .map(|index| LoadedBenchmark {
            dataset: loaded.dataset,
            sessions: loaded
                .sessions
                .iter()
                .filter(|session| session.sample_index == index)
                .cloned()
                .collect(),
            questions: loaded
                .questions
                .iter()
                .filter(|question| question.sample_index == index)
                .cloned()
                .collect(),
        })
        .collect()
}

/// Keep the first `per_type` questions of each `question_type`, discarding the
/// rest.  Order among survivors is preserved.  Sessions are NOT pruned here;
/// call `restrict_to_questions` (or `split_by_sample`) afterward to drop
/// unreferenced sessions.
pub fn stratify_questions(questions: &mut Vec<BenchQuestion>, per_type: usize) {
    let mut kept_per_type: std::collections::HashMap<String, usize> = Default::default();
    questions.retain(|question| {
        let count = kept_per_type
            .entry(question.question_type.clone())
            .or_insert(0);
        if *count < per_type {
            *count += 1;
            true
        } else {
            false
        }
    });
}

pub fn restrict_to_questions(
    mut loaded: LoadedBenchmark,
    question_limit: Option<usize>,
) -> LoadedBenchmark {
    let Some(limit) = question_limit else {
        return loaded;
    };
    loaded.questions.truncate(limit.min(loaded.questions.len()));
    let keep: std::collections::BTreeSet<_> = loaded
        .questions
        .iter()
        .flat_map(|question| question.session_ids.iter().cloned())
        .collect();
    loaded
        .sessions
        .retain(|session| keep.contains(&session.session_id));
    loaded
}

fn dataset_path(dataset: BenchDatasetName, data_dir: &Path) -> PathBuf {
    match dataset {
        BenchDatasetName::Locomo => data_dir.join("locomo").join("locomo10.json"),
        BenchDatasetName::LongMemEval => data_dir.join("longmemeval").join("longmemeval_s.json"),
    }
}

pub(crate) fn answer_needles_for(value: &Value) -> Vec<String> {
    answer_needles(value)
}

pub(crate) fn answer_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn limit(sample_limit: Option<usize>, len: usize) -> usize {
    sample_limit.unwrap_or(len).min(len)
}

pub(crate) fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(|inner| match inner {
        Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    })
}

pub(crate) fn string_array_field(value: &Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.trim().to_string()),
                    Value::Number(number) => Some(number.to_string()),
                    _ => None,
                })
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
