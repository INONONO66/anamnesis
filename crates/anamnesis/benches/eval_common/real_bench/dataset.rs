use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::locomo_pipeline::{answer_needles, normalize_for_match};
use super::error::{BenchError, BenchResult};

pub mod dates;
mod locomo;
mod longmemeval;

const MAX_DATASET_BYTES: u64 = 512 * 1024 * 1024;
const MAX_OPAQUE_ATTACHMENT_LOCATOR_BYTES: usize = 4_096;
const MAX_INLINE_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_INLINE_BASE64_BYTES: usize = 4 * MAX_INLINE_ATTACHMENT_BYTES.div_ceil(3);

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

/// Label-free input accepted by benchmark formation code.
///
/// Questions, reference answers, and relevance annotations are intentionally
/// absent so graph construction cannot observe evaluation labels.
#[derive(Debug, Clone, Copy)]
pub struct FormationInput<'a> {
    pub dataset: BenchDatasetName,
    pub sessions: &'a [BenchSession],
}

impl LoadedBenchmark {
    pub fn formation_input(&self) -> FormationInput<'_> {
        FormationInput {
            dataset: self.dataset,
            sessions: &self.sessions,
        }
    }
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
    /// Non-text assets attached to this turn.
    ///
    /// The locator is retained only so an offline consumer can resolve and
    /// fingerprint the asset. It is not concatenated into `content`, and
    /// dataset query/gold fields are never copied into this formation type.
    #[serde(default)]
    pub attachments: Vec<BenchAttachmentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchAttachmentRef {
    /// Stable zero-based position within the source turn's attachment list.
    pub attachment_index: usize,
    /// Opaque consumer-resolved asset locator. This is provenance, not text
    /// evidence, and is never admitted into the memory graph.
    pub locator: String,
}

pub(super) fn validate_attachment_locator(locator: &str) -> Result<(), String> {
    if locator.trim().is_empty()
        || locator.trim() != locator
        || locator.chars().any(char::is_control)
    {
        return Err("attachment locator is blank, untrimmed, or contains controls".to_owned());
    }
    if locator.starts_with("data:") {
        decode_inline_image_data_uri(locator).map(|_| ())
    } else if locator.len() > MAX_OPAQUE_ATTACHMENT_LOCATOR_BYTES {
        Err(format!(
            "opaque attachment locator exceeds {MAX_OPAQUE_ATTACHMENT_LOCATOR_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

/// Strictly decode the bounded inline image form accepted by LoCoMo.
///
/// `Ok(None)` means the locator is ordinary opaque provenance. Any `data:` URI
/// must match one of the closed image MIME prefixes and canonical padded
/// standard Base64; malformed inline bytes fail rather than becoming an
/// opaque path or URL.
pub(super) fn decode_inline_image_data_uri(locator: &str) -> Result<Option<Vec<u8>>, String> {
    let payload = if let Some(payload) = locator.strip_prefix("data:image/jpeg;base64,") {
        payload
    } else if let Some(payload) = locator.strip_prefix("data:image/png;base64,") {
        payload
    } else if let Some(payload) = locator.strip_prefix("data:image/webp;base64,") {
        payload
    } else if locator.starts_with("data:") {
        return Err("inline attachment must use data:image/{jpeg,png,webp};base64,".to_owned());
    } else {
        return Ok(None);
    };
    if payload.is_empty() || payload.len() % 4 != 0 {
        return Err("inline attachment Base64 length is invalid or oversized".to_owned());
    }
    let padding = if payload.ends_with("==") {
        2
    } else if payload.ends_with('=') {
        1
    } else {
        0
    };
    let decoded_len = bounded_inline_decoded_len(payload.len(), padding)?;
    if decoded_len == 0 {
        return Err("inline attachment decoded bytes are empty or oversized".to_owned());
    }

    let mut decoded = Vec::with_capacity(decoded_len);
    let chunks = payload.as_bytes().chunks_exact(4);
    if !chunks.remainder().is_empty() {
        return Err("inline attachment Base64 length is not divisible by four".to_owned());
    }
    let chunk_count = payload.len() / 4;
    for (chunk_index, chunk) in chunks.enumerate() {
        let &[raw_a, raw_b, raw_c, raw_d] = chunk else {
            return Err("inline attachment Base64 chunk is incomplete".to_owned());
        };
        let last = chunk_index + 1 == chunk_count;
        let a = base64_value(raw_a)
            .ok_or_else(|| "inline attachment contains non-Base64 bytes".to_owned())?;
        let b = base64_value(raw_b)
            .ok_or_else(|| "inline attachment contains non-Base64 bytes".to_owned())?;
        let c_padding = raw_c == b'=';
        let d_padding = raw_d == b'=';
        if (c_padding && !d_padding) || ((c_padding || d_padding) && !last) {
            return Err("inline attachment has non-canonical Base64 padding".to_owned());
        }
        let c = if c_padding {
            0
        } else {
            base64_value(raw_c)
                .ok_or_else(|| "inline attachment contains non-Base64 bytes".to_owned())?
        };
        let d = if d_padding {
            0
        } else {
            base64_value(raw_d)
                .ok_or_else(|| "inline attachment contains non-Base64 bytes".to_owned())?
        };
        if (c_padding && (b & 0x0f) != 0) || (d_padding && !c_padding && (c & 0x03) != 0) {
            return Err("inline attachment has non-canonical Base64 trailing bits".to_owned());
        }
        decoded.push((a << 2) | (b >> 4));
        if !c_padding {
            decoded.push((b << 4) | (c >> 2));
        }
        if !d_padding {
            decoded.push((c << 6) | d);
        }
    }
    if decoded.len() != decoded_len {
        return Err("inline attachment decoded length differs".to_owned());
    }
    Ok(Some(decoded))
}

fn bounded_inline_decoded_len(encoded_len: usize, padding: usize) -> Result<usize, String> {
    if encoded_len > MAX_INLINE_BASE64_BYTES || padding > 2 {
        return Err("inline attachment Base64 length is invalid or oversized".to_owned());
    }
    let decoded_len = encoded_len
        .checked_div(4)
        .and_then(|blocks| blocks.checked_mul(3))
        .and_then(|bytes| bytes.checked_sub(padding))
        .ok_or_else(|| "inline attachment decoded length overflowed".to_owned())?;
    if decoded_len > MAX_INLINE_ATTACHMENT_BYTES {
        return Err("inline attachment decoded bytes are empty or oversized".to_owned());
    }
    Ok(decoded_len)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
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

/// Label-free input accepted by retrieval and context-rendering code.
///
/// The evaluation layer keeps the reference answer and gold evidence, but
/// the product-shaped query path receives only fields available at runtime.
#[derive(Debug, Clone, Copy)]
pub struct RetrievalInput<'a> {
    pub question_id: &'a str,
    pub question: &'a str,
    pub question_date: Option<u64>,
}

impl BenchQuestion {
    pub fn retrieval_input(&self) -> RetrievalInput<'_> {
        RetrievalInput {
            question_id: &self.question_id,
            question: &self.question,
            question_date: self.question_date,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn locomo_loader_includes_official_image_caption_context() {
        let value = json!([{
            "session_1": [{
                "speaker": "Sam",
                "text": "I use this to reflect.",
                "blip_caption": "a journal with gratitude entries",
                "img_url": ["asset://fixture/one"],
                "query": "evaluation-side image-search metadata",
                "dia_id": "D1:1"
            }],
            "session_1_date_time": "1:00 pm on 8 May, 2023",
            "qa": [{
                "question": "What did Sam share?",
                "answer": "a journal",
                "category": 4,
                "evidence": ["D1:1"]
            }]
        }]);

        let loaded =
            parse_benchmark_dataset(BenchDatasetName::Locomo, &value, None).expect("valid LoCoMo");
        assert_eq!(
            loaded.sessions[0].turns[0].content,
            "I use this to reflect.\nSam shared a journal with gratitude entries."
        );
        assert_eq!(
            loaded.sessions[0].turns[0].attachments,
            vec![BenchAttachmentRef {
                attachment_index: 0,
                locator: "asset://fixture/one".to_owned(),
            }]
        );
        let formation_turn =
            serde_json::to_string(&loaded.sessions[0].turns[0]).expect("serialize formation turn");
        assert!(!formation_turn.contains("evaluation-side image-search metadata"));
        assert!(
            !loaded.sessions[0].turns[0]
                .content
                .contains("asset://fixture/one")
        );
    }

    #[test]
    fn locomo_attachment_positions_are_exact_and_malformed_entries_fail_closed() {
        let valid = json!([{
            "session_1": [{
                "speaker": "Sam",
                "text": "Two source assets are attached.",
                "img_url": [" asset://fixture/first ", "asset://fixture/second"],
                "dia_id": "D1:1"
            }],
            "qa": []
        }]);
        let loaded = parse_benchmark_dataset(BenchDatasetName::Locomo, &valid, None)
            .expect("valid attachment array");
        assert_eq!(
            loaded.sessions[0].turns[0].attachments,
            vec![
                BenchAttachmentRef {
                    attachment_index: 0,
                    locator: "asset://fixture/first".to_owned(),
                },
                BenchAttachmentRef {
                    attachment_index: 1,
                    locator: "asset://fixture/second".to_owned(),
                },
            ]
        );

        for invalid in [
            json!([{
                "session_1": [{
                    "speaker": "Sam",
                    "img_url": ["asset://fixture/first", null, "asset://fixture/third"],
                    "dia_id": "D1:1"
                }],
                "qa": []
            }]),
            json!([{
                "session_1": [{
                    "speaker": "Sam",
                    "img_url": ["asset://fixture/first", "   "],
                    "dia_id": "D1:1"
                }],
                "qa": []
            }]),
            json!([{
                "session_1": [{
                    "speaker": "Sam",
                    "img_url": {"path": "asset://fixture/first"},
                    "dia_id": "D1:1"
                }],
                "qa": []
            }]),
        ] {
            assert!(matches!(
                parse_benchmark_dataset(BenchDatasetName::Locomo, &invalid, None),
                Err(BenchError::Parse(_))
            ));
        }
    }

    #[test]
    fn attachment_locator_validation_separates_opaque_and_inline_limits() {
        let inline = "data:image/png;base64,aGVsbG8=";
        assert_eq!(
            decode_inline_image_data_uri(inline).expect("valid inline image bytes"),
            Some(b"hello".to_vec())
        );
        assert!(validate_attachment_locator(inline).is_ok());
        let loaded = parse_benchmark_dataset(
            BenchDatasetName::Locomo,
            &json!([{
                "session_1": [{
                    "speaker": "Sam",
                    "img_url": inline,
                    "dia_id": "D1:1"
                }],
                "qa": []
            }]),
            None,
        )
        .expect("strict scalar inline attachment should survive dataset loading");
        assert_eq!(loaded.sessions[0].turns[0].attachments[0].locator, inline);
        assert!(validate_attachment_locator(&"x".repeat(4_096)).is_ok());
        assert!(validate_attachment_locator(&"x".repeat(4_097)).is_err());
        for invalid in [
            "data:text/plain;base64,aGVsbG8=",
            "data:image/jpg;base64,aGVsbG8=",
            "data:image/png;charset=utf-8;base64,aGVsbG8=",
            "data:image/png,aGVsbG8=",
            "data:image/png;base64,aGVs bG8=",
            "data:image/png;base64,!!!!",
            "data:image/png;base64,Zh==",
            "data:image/png;base64,",
            " data:image/png;base64,aGVsbG8= ",
        ] {
            assert!(
                validate_attachment_locator(invalid).is_err(),
                "invalid inline locator was accepted: {invalid:?}"
            );
        }
        assert!(
            bounded_inline_decoded_len(MAX_INLINE_BASE64_BYTES + 4, 0).is_err(),
            "encoded-size cap must be checked without allocating an oversized fixture"
        );
    }
}
