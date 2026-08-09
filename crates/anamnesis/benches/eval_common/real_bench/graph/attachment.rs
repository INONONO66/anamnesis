use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::super::dataset::{
    FormationInput, decode_inline_image_data_uri, validate_attachment_locator,
};
use super::super::error::{BenchError, BenchResult};

pub const ATTACHMENT_OBSERVATION_ARTIFACT_SCHEMA_VERSION: u32 = 1;

const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_OBSERVATION_BYTES: usize = 128 * 1024;

/// Frozen identity of the offline processor that produced attachment text.
///
/// This is descriptive provenance. The benchmark loader never invokes the
/// processor or resolves an attachment itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentProcessorIdentity {
    pub processor_id: String,
    pub model: String,
    /// Caller-declared lowercase SHA-256 of the exact processor/model material.
    /// The loader requires equality with an independently supplied expected
    /// identity; it does not have model bytes and cannot attest them itself.
    pub model_sha256: String,
    /// Lowercase SHA-256 over the producer's canonical configuration object,
    /// including its exact prompt, class filter, decoder/resize profile, and
    /// generation parameters. This is separate from the model digest above.
    pub configuration_sha256: String,
    pub profile: String,
    pub output_schema: String,
}

/// Versioned wire artifact containing only attachment-derived observations.
///
/// Evaluation questions, reference answers, and gold evidence deliberately
/// have no representation in this schema. Unknown fields are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentObservationArtifact {
    pub schema_version: u32,
    pub dataset_fnv1a64: String,
    pub processor: AttachmentProcessorIdentity,
    /// One disposition for every attachment binding in the label-free input.
    ///
    /// This prevents a producer from silently omitting inconvenient assets or
    /// selecting only observations that happen to help evaluation questions.
    pub coverage: Vec<AttachmentCoverageRecord>,
    pub records: Vec<AttachmentObservationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentCoverageRecord {
    pub parent_session_id: String,
    pub parent_turn_id: String,
    pub attachment_index: usize,
    pub disposition: AttachmentCoverageDisposition,
}

/// Closed, question-independent outcome of an offline attachment attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttachmentCoverageDisposition {
    /// A validated observation record exists for this exact binding.
    Observed { record_id: String },
    /// The processor's frozen profile deliberately excludes this asset class.
    SkippedByProfile,
    /// The opaque locator could not be resolved to immutable bytes.
    Unavailable,
    /// Bytes were resolved but could not be decoded as a supported asset.
    DecodeFailed,
    /// Local processing failed after successful decode.
    ProcessorFailed,
}

/// Closed accounting of every attachment binding in a validated artifact.
///
/// The fields remain private so callers cannot manufacture inconsistent
/// totals. Serialized reports expose the named counts, and deserialization
/// rejects any value whose dispositions do not sum exactly to `total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AttachmentCoverageCounts {
    total: usize,
    observed: usize,
    skipped_by_profile: usize,
    unavailable: usize,
    decode_failed: usize,
    processor_failed: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentCoverageCountsWire {
    total: usize,
    observed: usize,
    skipped_by_profile: usize,
    unavailable: usize,
    decode_failed: usize,
    processor_failed: usize,
}

impl<'de> Deserialize<'de> for AttachmentCoverageCounts {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AttachmentCoverageCountsWire::deserialize(deserializer)?;
        let counts = Self {
            total: wire.total,
            observed: wire.observed,
            skipped_by_profile: wire.skipped_by_profile,
            unavailable: wire.unavailable,
            decode_failed: wire.decode_failed,
            processor_failed: wire.processor_failed,
        };
        counts
            .validate_sum()
            .map_err(<D::Error as serde::de::Error>::custom)?;
        Ok(counts)
    }
}

impl AttachmentCoverageCounts {
    fn from_coverage(coverage: &[AttachmentCoverageRecord]) -> BenchResult<Self> {
        let mut counts = Self {
            total: coverage.len(),
            observed: 0,
            skipped_by_profile: 0,
            unavailable: 0,
            decode_failed: 0,
            processor_failed: 0,
        };
        for item in coverage {
            let count = match &item.disposition {
                AttachmentCoverageDisposition::Observed { .. } => &mut counts.observed,
                AttachmentCoverageDisposition::SkippedByProfile => &mut counts.skipped_by_profile,
                AttachmentCoverageDisposition::Unavailable => &mut counts.unavailable,
                AttachmentCoverageDisposition::DecodeFailed => &mut counts.decode_failed,
                AttachmentCoverageDisposition::ProcessorFailed => &mut counts.processor_failed,
            };
            *count = count.checked_add(1).ok_or_else(|| {
                BenchError::Parse("attachment coverage disposition count overflowed".to_owned())
            })?;
        }
        counts.validate_sum().map_err(BenchError::Parse)?;
        Ok(counts)
    }

    fn validate_sum(&self) -> Result<(), String> {
        let classified = self
            .observed
            .checked_add(self.skipped_by_profile)
            .and_then(|value| value.checked_add(self.unavailable))
            .and_then(|value| value.checked_add(self.decode_failed))
            .and_then(|value| value.checked_add(self.processor_failed))
            .ok_or_else(|| "attachment coverage counts overflowed".to_owned())?;
        if classified != self.total {
            return Err(format!(
                "attachment coverage counts sum to {classified}, expected total {}",
                self.total
            ));
        }
        Ok(())
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn observed(&self) -> usize {
        self.observed
    }

    pub fn skipped_by_profile(&self) -> usize {
        self.skipped_by_profile
    }

    pub fn unavailable(&self) -> usize {
        self.unavailable
    }

    pub fn decode_failed(&self) -> usize {
        self.decode_failed
    }

    pub fn processor_failed(&self) -> usize {
        self.processor_failed
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentObservationRecord {
    pub record_id: String,
    /// Full benchmark session identity, not the dataset-local session label.
    pub parent_session_id: String,
    pub parent_turn_id: String,
    pub attachment_index: usize,
    /// Lowercase SHA-256 of the exact attachment bytes processed offline.
    pub asset_sha256: String,
    /// Immutable processor output admitted as a separate source fragment.
    pub observation: String,
    /// FNV-1a-64 of the exact UTF-8 bytes in `observation`.
    pub output_fnv1a64: String,
    pub confidence: f64,
}

/// An artifact that has passed schema, dataset, processor, output-digest, and
/// parent-attachment binding checks.
///
/// Fields remain private so graph formation cannot bypass the validating
/// loader by constructing this value directly.
#[derive(Debug, Clone)]
pub struct ValidatedAttachmentObservationArtifact {
    pub(super) dataset_fnv1a64: String,
    pub(super) processor: AttachmentProcessorIdentity,
    pub(super) records: Vec<ValidatedAttachmentObservationRecord>,
    artifact_bytes: u64,
    artifact_fnv1a64: String,
    coverage_counts: AttachmentCoverageCounts,
}

#[derive(Debug, Clone)]
pub(super) struct ValidatedAttachmentObservationRecord {
    pub(super) record: AttachmentObservationRecord,
    pub(super) bound_locator: String,
}

impl ValidatedAttachmentObservationArtifact {
    pub fn dataset_fnv1a64(&self) -> &str {
        &self.dataset_fnv1a64
    }

    pub fn processor(&self) -> &AttachmentProcessorIdentity {
        &self.processor
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// Exact byte length read and validated by the loader.
    pub fn artifact_bytes(&self) -> u64 {
        self.artifact_bytes
    }

    /// FNV-1a-64 of the exact byte buffer read and validated by the loader.
    pub fn artifact_fnv1a64(&self) -> &str {
        &self.artifact_fnv1a64
    }

    pub fn covered_attachment_count(&self) -> usize {
        self.coverage_counts.total()
    }

    pub fn coverage_counts(&self) -> &AttachmentCoverageCounts {
        &self.coverage_counts
    }
}

/// Load and validate an optional frozen attachment-observation artifact.
///
/// `None` is an explicit no-op. A supplied path is fail-closed: unreadable,
/// oversized, malformed, version/config mismatched, digest-invalid, duplicate,
/// or unbound records return an error. Validation only inspects local bytes and
/// the label-free [`FormationInput`]; it performs no attachment I/O or model
/// invocation.
pub fn load_optional_attachment_observation_artifact(
    path: Option<&Path>,
    expected_dataset_fnv1a64: &str,
    expected_processor: &AttachmentProcessorIdentity,
    input: FormationInput<'_>,
) -> BenchResult<Option<ValidatedAttachmentObservationArtifact>> {
    let Some(path) = path else {
        return Ok(None);
    };

    validate_fnv1a64(expected_dataset_fnv1a64, "expected dataset fingerprint")?;
    validate_processor(expected_processor)?;

    let file = File::open(path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to open attachment-observation artifact {}: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        BenchError::InvalidInput(format!(
            "failed to inspect attachment-observation artifact {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation artifact {} is not a regular file",
            path.display()
        )));
    }
    let size = metadata.len();
    if size > MAX_ARTIFACT_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation artifact {} is too large: {size} bytes > {MAX_ARTIFACT_BYTES}",
            path.display()
        )));
    }
    // Read through the same open handle that was inspected above, and cap the
    // stream independently of its metadata. This remains bounded if a file is
    // replaced or grows while it is being read.
    let mut bytes = Vec::with_capacity(size.min(MAX_ARTIFACT_BYTES) as usize);
    file.take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            BenchError::InvalidInput(format!(
                "failed to read attachment-observation artifact {}: {error}",
                path.display()
            ))
        })?;
    let artifact_bytes = u64::try_from(bytes.len()).map_err(|error| {
        BenchError::Parse(format!(
            "attachment-observation artifact byte count cannot be represented: {error}"
        ))
    })?;
    if artifact_bytes > MAX_ARTIFACT_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation artifact {} grew beyond {MAX_ARTIFACT_BYTES} bytes while reading",
            path.display()
        )));
    }
    let artifact_fnv1a64 = fnv1a64_bytes(&bytes);
    let artifact: AttachmentObservationArtifact =
        serde_json::from_slice(&bytes).map_err(|error| {
            BenchError::Parse(format!(
                "failed to parse attachment-observation artifact {}: {error}",
                path.display()
            ))
        })?;

    if artifact.schema_version != ATTACHMENT_OBSERVATION_ARTIFACT_SCHEMA_VERSION {
        return Err(BenchError::InvalidInput(format!(
            "unsupported attachment-observation artifact schema {}",
            artifact.schema_version
        )));
    }
    validate_fnv1a64(&artifact.dataset_fnv1a64, "artifact dataset fingerprint")?;
    if artifact.dataset_fnv1a64 != expected_dataset_fnv1a64 {
        return Err(BenchError::InvalidInput(
            "attachment-observation artifact dataset fingerprint differs".to_owned(),
        ));
    }
    validate_processor(&artifact.processor)?;
    if artifact.processor != *expected_processor {
        return Err(BenchError::InvalidInput(
            "attachment-observation artifact processor configuration differs".to_owned(),
        ));
    }
    if artifact.records.len() > MAX_RECORDS || artifact.coverage.len() > MAX_RECORDS {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation artifact has too many records: observations={}, coverage={} > {MAX_RECORDS}",
            artifact.records.len(),
            artifact.coverage.len()
        )));
    }

    let attachments = attachment_bindings(input)?;
    let coverage_counts = AttachmentCoverageCounts::from_coverage(&artifact.coverage)?;
    let mut record_ids = HashSet::new();
    let mut record_bindings = HashSet::new();
    let mut records = Vec::with_capacity(artifact.records.len());
    for record in artifact.records {
        validate_record(&record)?;
        if !record_ids.insert(record.record_id.clone()) {
            return Err(BenchError::InvalidInput(format!(
                "duplicate attachment-observation record id {:?}",
                record.record_id
            )));
        }
        let binding = (
            record.parent_session_id.clone(),
            record.parent_turn_id.clone(),
            record.attachment_index,
        );
        if !record_bindings.insert(binding.clone()) {
            return Err(BenchError::InvalidInput(format!(
                "duplicate attachment-observation binding for session {:?}, turn {:?}, attachment {}",
                record.parent_session_id, record.parent_turn_id, record.attachment_index
            )));
        }
        let locator = attachments.get(&binding).ok_or_else(|| {
            BenchError::InvalidInput(format!(
                "attachment-observation record {:?} does not bind to a loaded attachment",
                record.record_id
            ))
        })?;
        let inline_bytes = decode_inline_image_data_uri(locator).map_err(|error| {
            BenchError::InvalidInput(format!(
                "attachment-observation record {:?} has invalid inline source bytes: {error}",
                record.record_id
            ))
        })?;
        if inline_bytes
            .as_deref()
            .is_some_and(|bytes| sha256_lower_hex(bytes) != record.asset_sha256)
        {
            return Err(BenchError::InvalidInput(format!(
                "attachment-observation record {:?} asset digest differs from its inline source bytes",
                record.record_id
            )));
        }
        records.push(ValidatedAttachmentObservationRecord {
            record,
            bound_locator: locator.clone(),
        });
    }

    if artifact.coverage.len() != attachments.len() {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation coverage has {} entries for {} loaded attachments",
            artifact.coverage.len(),
            attachments.len()
        )));
    }
    let records_by_id: HashMap<&str, (&str, &str, usize)> = records
        .iter()
        .map(|validated| {
            (
                validated.record.record_id.as_str(),
                (
                    validated.record.parent_session_id.as_str(),
                    validated.record.parent_turn_id.as_str(),
                    validated.record.attachment_index,
                ),
            )
        })
        .collect();
    let mut covered_bindings = HashSet::with_capacity(artifact.coverage.len());
    let mut consumed_record_ids = HashSet::with_capacity(records.len());
    for coverage in &artifact.coverage {
        validate_bounded_text(
            &coverage.parent_session_id,
            "coverage parent session id",
            256,
        )?;
        validate_bounded_text(&coverage.parent_turn_id, "coverage parent turn id", 256)?;
        let binding = (
            coverage.parent_session_id.clone(),
            coverage.parent_turn_id.clone(),
            coverage.attachment_index,
        );
        if !attachments.contains_key(&binding) {
            return Err(BenchError::InvalidInput(
                "attachment-observation coverage does not bind to a loaded attachment".to_owned(),
            ));
        }
        if !covered_bindings.insert(binding.clone()) {
            return Err(BenchError::InvalidInput(
                "duplicate attachment-observation coverage binding".to_owned(),
            ));
        }
        if let AttachmentCoverageDisposition::Observed { record_id } = &coverage.disposition {
            validate_bounded_text(record_id, "coverage observation record id", 128)?;
            let Some(&(record_session_id, record_turn_id, record_attachment_index)) =
                records_by_id.get(record_id.as_str())
            else {
                return Err(BenchError::InvalidInput(format!(
                    "attachment-observation coverage references missing record {record_id:?}"
                )));
            };
            if record_session_id != binding.0.as_str()
                || record_turn_id != binding.1.as_str()
                || record_attachment_index != binding.2
            {
                return Err(BenchError::InvalidInput(format!(
                    "attachment-observation coverage record {record_id:?} belongs to a different attachment"
                )));
            }
            if !consumed_record_ids.insert(record_id.as_str()) {
                return Err(BenchError::InvalidInput(format!(
                    "attachment-observation record {record_id:?} is covered more than once"
                )));
            }
        }
    }
    if covered_bindings.len() != attachments.len()
        || consumed_record_ids.len() != records.len()
        || coverage_counts.observed() != records.len()
        || attachments
            .keys()
            .any(|key| !covered_bindings.contains(key))
    {
        return Err(BenchError::InvalidInput(
            "attachment-observation coverage is incomplete or leaves an observation unbound"
                .to_owned(),
        ));
    }

    Ok(Some(ValidatedAttachmentObservationArtifact {
        dataset_fnv1a64: artifact.dataset_fnv1a64,
        processor: artifact.processor,
        records,
        artifact_bytes,
        artifact_fnv1a64,
        coverage_counts,
    }))
}

/// Stable output digest used by artifact producers and the validating loader.
pub fn attachment_observation_output_fnv1a64(observation: &str) -> String {
    fnv1a64_bytes(observation.as_bytes())
}

fn fnv1a64_bytes(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn attachment_bindings(
    input: FormationInput<'_>,
) -> BenchResult<HashMap<(String, String, usize), String>> {
    let mut bindings = HashMap::new();
    for session in input.sessions {
        for turn in &session.turns {
            if turn.attachments.is_empty() {
                continue;
            }
            let turn_id = turn.raw_turn_id.as_deref().ok_or_else(|| {
                BenchError::InvalidInput(format!(
                    "attachment-bearing turn in session {:?} has no stable turn id",
                    turn.session_id
                ))
            })?;
            for attachment in &turn.attachments {
                validate_attachment_locator(&attachment.locator).map_err(|error| {
                    BenchError::InvalidInput(format!("attachment locator is invalid: {error}"))
                })?;
                let key = (
                    turn.session_id.clone(),
                    turn_id.to_owned(),
                    attachment.attachment_index,
                );
                if bindings.insert(key, attachment.locator.clone()).is_some() {
                    return Err(BenchError::InvalidInput(format!(
                        "duplicate attachment index {} for session {:?}, turn {:?}",
                        attachment.attachment_index, turn.session_id, turn_id
                    )));
                }
                if bindings.len() > MAX_RECORDS {
                    return Err(BenchError::InvalidInput(format!(
                        "label-free input contains more than {MAX_RECORDS} attachment bindings"
                    )));
                }
            }
        }
    }
    Ok(bindings)
}

fn validate_processor(processor: &AttachmentProcessorIdentity) -> BenchResult<()> {
    validate_bounded_text(&processor.processor_id, "processor id", 128)?;
    validate_bounded_text(&processor.model, "processor model", 256)?;
    validate_sha256(&processor.model_sha256, "processor model digest")?;
    validate_sha256(
        &processor.configuration_sha256,
        "processor configuration digest",
    )?;
    validate_bounded_text(&processor.profile, "processor profile", 128)?;
    validate_bounded_text(&processor.output_schema, "processor output schema", 128)
}

fn validate_record(record: &AttachmentObservationRecord) -> BenchResult<()> {
    validate_bounded_text(&record.record_id, "record id", 128)?;
    validate_bounded_text(&record.parent_session_id, "parent session id", 256)?;
    validate_bounded_text(&record.parent_turn_id, "parent turn id", 256)?;
    validate_sha256(&record.asset_sha256, "asset digest")?;
    validate_bounded_text(
        &record.observation,
        "record observation",
        MAX_OBSERVATION_BYTES,
    )?;
    if record.observation.len() > MAX_OBSERVATION_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation record {:?} has output larger than {MAX_OBSERVATION_BYTES} bytes",
            record.record_id
        )));
    }
    validate_fnv1a64(&record.output_fnv1a64, "observation output digest")?;
    if record.output_fnv1a64 != attachment_observation_output_fnv1a64(&record.observation) {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation record {:?} output digest differs",
            record.record_id
        )));
    }
    if !record.confidence.is_finite() || !(0.0..=1.0).contains(&record.confidence) {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation record {:?} has invalid confidence",
            record.record_id
        )));
    }
    Ok(())
}

fn validate_bounded_text(value: &str, field: &str, max_chars: usize) -> BenchResult<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation {field} is blank, untrimmed, contains controls, or exceeds {max_chars} characters"
        )));
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> BenchResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation {field} must be a lowercase SHA-256"
        )));
    }
    Ok(())
}

fn validate_fnv1a64(value: &str, field: &str) -> BenchResult<()> {
    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(BenchError::InvalidInput(format!(
            "attachment-observation {field} must be a lowercase FNV-1a-64 digest"
        )));
    }
    Ok(())
}

fn sha256_lower_hex(bytes: &[u8]) -> String {
    let mut state = [
        0x6a09e667_u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut chunks = bytes.chunks_exact(64);
    for chunk in &mut chunks {
        let mut block = [0_u8; 64];
        block.copy_from_slice(chunk);
        sha256_compress(&mut state, &block);
    }
    let remainder = chunks.remainder();
    let mut tail = [0_u8; 128];
    tail[..remainder.len()].copy_from_slice(remainder);
    tail[remainder.len()] = 0x80;
    let tail_len = if remainder.len() < 56 { 64 } else { 128 };
    let bit_len = (bytes.len() as u64).saturating_mul(8);
    tail[tail_len - 8..tail_len].copy_from_slice(&bit_len.to_be_bytes());
    for chunk in tail[..tail_len].chunks_exact(64) {
        let mut block = [0_u8; 64];
        block.copy_from_slice(chunk);
        sha256_compress(&mut state, &block);
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in state.into_iter().flat_map(u32::to_be_bytes) {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn sha256_compress(state: &mut [u32; 8], block: &[u8; 64]) {
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut words = [0_u32; 64];
    for (index, bytes) in block.chunks_exact(4).enumerate() {
        let &[a, b, c, d] = bytes else {
            continue;
        };
        words[index] = u32::from_be_bytes([a, b, c, d]);
    }
    for index in 16..64 {
        let s0 = words[index - 15].rotate_right(7)
            ^ words[index - 15].rotate_right(18)
            ^ (words[index - 15] >> 3);
        let s1 = words[index - 2].rotate_right(17)
            ^ words[index - 2].rotate_right(19)
            ^ (words[index - 2] >> 10);
        words[index] = words[index - 16]
            .wrapping_add(s0)
            .wrapping_add(words[index - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (constant, word) in ROUND.into_iter().zip(words) {
        let sum1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(constant)
            .wrapping_add(word);
        let sum0 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(sum1);
        d = c;
        c = b;
        b = a;
        a = sum1.wrapping_add(sum0);
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anamnesis::Error;
    use anamnesis::embedding::EmbeddingProvider;
    use anamnesis::graph::{KnowledgeType, SourceKind, Timestamp};
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::super::super::dataset::{
        BenchAttachmentRef, BenchDatasetName, BenchSession, BenchTurn, LoadedBenchmark,
    };
    use super::super::{
        build_memory_graph, build_memory_graph_with_derived_and_attachment_observations,
    };
    use super::*;

    const DATASET_DIGEST: &str = "0123456789abcdef";
    const MODEL_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CONFIGURATION_SHA256: &str =
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const ASSET_SHA256: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const OBSERVATION: &str = "The attachment contains a blue triangle beside two circles.";

    struct CountingProvider {
        calls: Arc<AtomicUsize>,
    }

    impl EmbeddingProvider for CountingProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(texts
                .iter()
                .map(|text| vec![text.len() as f32, 1.0])
                .collect())
        }

        fn dimensions(&self) -> usize {
            2
        }

        fn model_name(&self) -> &str {
            "synthetic-provider"
        }
    }

    fn loaded_fixture() -> LoadedBenchmark {
        LoadedBenchmark {
            dataset: BenchDatasetName::Locomo,
            sessions: vec![BenchSession {
                session_id: "locomo-0-session_1".to_owned(),
                raw_session_id: "session_1".to_owned(),
                sample_index: 0,
                turns: vec![BenchTurn {
                    session_id: "locomo-0-session_1".to_owned(),
                    raw_session_id: "session_1".to_owned(),
                    raw_turn_id: Some("D1:1".to_owned()),
                    turn_index: 0,
                    speaker: "Sam".to_owned(),
                    role: "Sam".to_owned(),
                    content: "I attached a diagram.".to_owned(),
                    attachments: vec![
                        BenchAttachmentRef {
                            attachment_index: 0,
                            locator: "asset:fixture:one".to_owned(),
                        },
                        BenchAttachmentRef {
                            attachment_index: 1,
                            locator: "asset:fixture:two".to_owned(),
                        },
                    ],
                }],
                start_timestamp: Some(1_700_000_000),
            }],
            questions: Vec::new(),
        }
    }

    fn processor() -> AttachmentProcessorIdentity {
        AttachmentProcessorIdentity {
            processor_id: "local-observer".to_owned(),
            model: "frozen-vision-model".to_owned(),
            model_sha256: MODEL_SHA256.to_owned(),
            configuration_sha256: CONFIGURATION_SHA256.to_owned(),
            profile: "descriptive-v1".to_owned(),
            output_schema: "plain-observation-v1".to_owned(),
        }
    }

    fn artifact() -> AttachmentObservationArtifact {
        AttachmentObservationArtifact {
            schema_version: ATTACHMENT_OBSERVATION_ARTIFACT_SCHEMA_VERSION,
            dataset_fnv1a64: DATASET_DIGEST.to_owned(),
            processor: processor(),
            coverage: vec![
                AttachmentCoverageRecord {
                    parent_session_id: "locomo-0-session_1".to_owned(),
                    parent_turn_id: "D1:1".to_owned(),
                    attachment_index: 0,
                    disposition: AttachmentCoverageDisposition::Observed {
                        record_id: "observation-1".to_owned(),
                    },
                },
                AttachmentCoverageRecord {
                    parent_session_id: "locomo-0-session_1".to_owned(),
                    parent_turn_id: "D1:1".to_owned(),
                    attachment_index: 1,
                    disposition: AttachmentCoverageDisposition::SkippedByProfile,
                },
            ],
            records: vec![AttachmentObservationRecord {
                record_id: "observation-1".to_owned(),
                parent_session_id: "locomo-0-session_1".to_owned(),
                parent_turn_id: "D1:1".to_owned(),
                attachment_index: 0,
                asset_sha256: ASSET_SHA256.to_owned(),
                observation: OBSERVATION.to_owned(),
                output_fnv1a64: attachment_observation_output_fnv1a64(OBSERVATION),
                confidence: 0.93,
            }],
        }
    }

    fn write_json(temp: &TempDir, name: &str, value: &Value) -> PathBuf {
        let path = temp.path().join(name);
        let bytes = serde_json::to_vec(value).expect("serialize artifact fixture");
        std::fs::write(&path, bytes).expect("write artifact fixture");
        path
    }

    #[test]
    fn validated_observation_is_a_separate_unembedded_source_with_full_provenance() {
        let loaded = loaded_fixture();
        let temp = tempfile::tempdir().expect("create temp directory");
        let path = write_json(
            &temp,
            "attachment-observations.json",
            &serde_json::to_value(artifact()).expect("serialize artifact"),
        );
        let validated = load_optional_attachment_observation_artifact(
            Some(&path),
            DATASET_DIGEST,
            &processor(),
            loaded.formation_input(),
        )
        .expect("load valid artifact")
        .expect("artifact present");
        let artifact_bytes = std::fs::read(&path).expect("read artifact fixture");
        assert_eq!(
            validated.artifact_bytes(),
            u64::try_from(artifact_bytes.len()).expect("fixture byte length")
        );
        assert_eq!(validated.artifact_fnv1a64(), fnv1a64_bytes(&artifact_bytes));
        assert_eq!(validated.covered_attachment_count(), 2);
        assert_eq!(validated.record_count(), 1);
        assert_eq!(validated.coverage_counts().total(), 2);
        assert_eq!(validated.coverage_counts().observed(), 1);
        assert_eq!(validated.coverage_counts().skipped_by_profile(), 1);
        assert_eq!(validated.coverage_counts().unavailable(), 0);
        assert_eq!(validated.coverage_counts().decode_failed(), 0);
        assert_eq!(validated.coverage_counts().processor_failed(), 0);

        let baseline_calls = Arc::new(AtomicUsize::new(0));
        let _baseline = build_memory_graph(
            loaded.formation_input(),
            Arc::new(CountingProvider {
                calls: baseline_calls.clone(),
            }),
        )
        .expect("build baseline graph");
        let observed_calls = Arc::new(AtomicUsize::new(0));
        let graph = build_memory_graph_with_derived_and_attachment_observations(
            loaded.formation_input(),
            Arc::new(CountingProvider {
                calls: observed_calls.clone(),
            }),
            &[],
            &[],
            Some(&validated),
        )
        .expect("build graph with observation");
        assert_eq!(
            observed_calls.load(Ordering::Relaxed),
            baseline_calls.load(Ordering::Relaxed),
            "unembedded observation admission must not invoke the provider"
        );

        let (node_id, provenance) = graph
            .provenance_by_node
            .iter()
            .find(|(_, provenance)| provenance.content == OBSERVATION)
            .expect("observation provenance");
        let node = graph
            .memory
            .engine()
            .graph()
            .get_node(*node_id)
            .expect("observation node");
        assert_eq!(node.node_type, KnowledgeType::Episodic);
        assert_eq!(node.origin.source_kind, SourceKind::DocumentExtract);
        assert_eq!(node.origin.session_id, "session_1");
        assert_eq!(node.origin.confidence, 0.93);
        assert!(node.embedding.is_none());
        assert_eq!(node.metadata["attachment:asset-sha256"], ASSET_SHA256);
        assert_eq!(
            node.metadata["attachment:output-fnv1a64"],
            attachment_observation_output_fnv1a64(OBSERVATION)
        );
        assert_eq!(node.metadata["processor:model-sha256"], MODEL_SHA256);
        assert_eq!(
            node.metadata["processor:configuration-sha256"],
            CONFIGURATION_SHA256
        );
        assert!(
            node.metadata
                .values()
                .all(|value| !value.contains("asset:fixture:one")),
            "the private source locator must not enter graph metadata"
        );
        assert_eq!(provenance.raw_turn_id.as_deref(), Some("D1:1"));
        assert_eq!(loaded.sessions[0].turns[0].content, "I attached a diagram.");
        assert!(!loaded.sessions[0].turns[0].content.contains(OBSERVATION));
    }

    #[test]
    fn observed_unembedded_fragment_is_reachable_through_product_search_and_render() {
        let loaded = loaded_fixture();
        let temp = tempfile::tempdir().expect("create temp directory");
        let path = write_json(
            &temp,
            "attachment-observations.json",
            &serde_json::to_value(artifact()).expect("serialize artifact"),
        );
        let validated = load_optional_attachment_observation_artifact(
            Some(&path),
            DATASET_DIGEST,
            &processor(),
            loaded.formation_input(),
        )
        .expect("load valid artifact")
        .expect("artifact present");
        let mut graph = build_memory_graph_with_derived_and_attachment_observations(
            loaded.formation_input(),
            Arc::new(CountingProvider {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            &[],
            &[],
            Some(&validated),
        )
        .expect("build graph with observation");
        let observation_node = graph
            .provenance_by_node
            .iter()
            .find_map(|(node_id, provenance)| {
                (provenance.content == OBSERVATION).then_some(*node_id)
            })
            .expect("observation node");

        let query = "What did Sam's attachment show beside the blue triangle and circles?";
        let recall = graph
            .memory
            .search_at(query, 8, Timestamp(1_800_000_000_000))
            .expect("product search");
        assert!(
            recall
                .hits
                .iter()
                .any(|hit| hit.node_id == observation_node),
            "the exact source terms and speaker cue must reach the unembedded observation"
        );
        let rendered = graph
            .memory
            .render_context_for(query, &recall)
            .expect("product context render");
        assert!(
            rendered.contains(OBSERVATION),
            "the observation must survive the public search-to-render path"
        );
        assert!(
            !rendered.contains("asset:fixture:one"),
            "the private source locator must not be rendered"
        );
    }

    #[test]
    fn loader_fails_closed_on_dataset_and_processor_config_mismatch() {
        let loaded = loaded_fixture();
        let temp = tempfile::tempdir().expect("create temp directory");
        let path = write_json(
            &temp,
            "attachment-observations.json",
            &serde_json::to_value(artifact()).expect("serialize artifact"),
        );
        let dataset_error = load_optional_attachment_observation_artifact(
            Some(&path),
            "fedcba9876543210",
            &processor(),
            loaded.formation_input(),
        );
        assert!(matches!(dataset_error, Err(BenchError::InvalidInput(_))));

        let mut other_processor = processor();
        other_processor.profile = "different-profile".to_owned();
        let config_error = load_optional_attachment_observation_artifact(
            Some(&path),
            DATASET_DIGEST,
            &other_processor,
            loaded.formation_input(),
        );
        assert!(matches!(config_error, Err(BenchError::InvalidInput(_))));
    }

    #[test]
    fn artifact_schema_rejects_query_gold_and_expected_answer_metadata() {
        let loaded = loaded_fixture();
        let temp = tempfile::tempdir().expect("create temp directory");
        for (index, (field, value)) in [
            ("query", json!("evaluation query")),
            ("gold", json!({"turns": ["D1:1"]})),
            ("expected_answer", json!("evaluation answer")),
        ]
        .into_iter()
        .enumerate()
        {
            let mut wire = serde_json::to_value(artifact()).expect("serialize artifact");
            wire["records"][0][field] = value;
            let path = write_json(&temp, &format!("forbidden-{index}.json"), &wire);
            let result = load_optional_attachment_observation_artifact(
                Some(&path),
                DATASET_DIGEST,
                &processor(),
                loaded.formation_input(),
            );
            assert!(matches!(result, Err(BenchError::Parse(_))), "field={field}");
        }
    }

    #[test]
    fn coverage_must_account_for_every_binding_and_every_observation() {
        let loaded = loaded_fixture();
        let temp = tempfile::tempdir().expect("create temp directory");

        let mut missing_binding = artifact();
        missing_binding.coverage.pop();
        let missing_path = write_json(
            &temp,
            "missing-coverage.json",
            &serde_json::to_value(missing_binding).expect("serialize missing coverage"),
        );
        let missing = load_optional_attachment_observation_artifact(
            Some(&missing_path),
            DATASET_DIGEST,
            &processor(),
            loaded.formation_input(),
        );
        assert!(matches!(missing, Err(BenchError::InvalidInput(_))));

        let mut orphaned_observation = artifact();
        orphaned_observation.coverage[0].disposition =
            AttachmentCoverageDisposition::SkippedByProfile;
        let orphaned_path = write_json(
            &temp,
            "orphaned-observation.json",
            &serde_json::to_value(orphaned_observation).expect("serialize orphaned observation"),
        );
        let orphaned = load_optional_attachment_observation_artifact(
            Some(&orphaned_path),
            DATASET_DIGEST,
            &processor(),
            loaded.formation_input(),
        );
        assert!(matches!(orphaned, Err(BenchError::InvalidInput(_))));
    }

    #[test]
    fn coverage_counts_are_exact_and_reject_malformed_or_inconsistent_wire_values() {
        let coverage = vec![
            AttachmentCoverageRecord {
                parent_session_id: "session".to_owned(),
                parent_turn_id: "turn".to_owned(),
                attachment_index: 0,
                disposition: AttachmentCoverageDisposition::Observed {
                    record_id: "record".to_owned(),
                },
            },
            AttachmentCoverageRecord {
                parent_session_id: "session".to_owned(),
                parent_turn_id: "turn".to_owned(),
                attachment_index: 1,
                disposition: AttachmentCoverageDisposition::SkippedByProfile,
            },
            AttachmentCoverageRecord {
                parent_session_id: "session".to_owned(),
                parent_turn_id: "turn".to_owned(),
                attachment_index: 2,
                disposition: AttachmentCoverageDisposition::Unavailable,
            },
            AttachmentCoverageRecord {
                parent_session_id: "session".to_owned(),
                parent_turn_id: "turn".to_owned(),
                attachment_index: 3,
                disposition: AttachmentCoverageDisposition::DecodeFailed,
            },
            AttachmentCoverageRecord {
                parent_session_id: "session".to_owned(),
                parent_turn_id: "turn".to_owned(),
                attachment_index: 4,
                disposition: AttachmentCoverageDisposition::ProcessorFailed,
            },
        ];
        let counts =
            AttachmentCoverageCounts::from_coverage(&coverage).expect("closed disposition counts");
        assert_eq!(counts.total(), 5);
        assert_eq!(counts.observed(), 1);
        assert_eq!(counts.skipped_by_profile(), 1);
        assert_eq!(counts.unavailable(), 1);
        assert_eq!(counts.decode_failed(), 1);
        assert_eq!(counts.processor_failed(), 1);
        let round_trip: AttachmentCoverageCounts = serde_json::from_value(
            serde_json::to_value(counts).expect("serialize coverage counts"),
        )
        .expect("deserialize valid coverage counts");
        assert_eq!(round_trip, counts);

        let inconsistent = serde_json::from_value::<AttachmentCoverageCounts>(json!({
            "total": 5,
            "observed": 1,
            "skipped_by_profile": 1,
            "unavailable": 1,
            "decode_failed": 0,
            "processor_failed": 0
        }));
        assert!(inconsistent.is_err());
        let unknown = serde_json::from_value::<AttachmentCoverageCounts>(json!({
            "total": 5,
            "observed": 1,
            "skipped_by_profile": 1,
            "unavailable": 1,
            "decode_failed": 1,
            "processor_failed": 1,
            "other": 0
        }));
        assert!(unknown.is_err());

        let mut malformed_artifact = serde_json::to_value(artifact()).expect("artifact wire");
        malformed_artifact["coverage"][1]["disposition"] = json!({"status": "unknown"});
        let temp = tempfile::tempdir().expect("create temp directory");
        let malformed_path = write_json(&temp, "malformed-status.json", &malformed_artifact);
        let loaded = loaded_fixture();
        assert!(matches!(
            load_optional_attachment_observation_artifact(
                Some(&malformed_path),
                DATASET_DIGEST,
                &processor(),
                loaded.formation_input(),
            ),
            Err(BenchError::Parse(_))
        ));
    }

    #[test]
    fn absent_artifact_is_an_explicit_no_op_but_missing_supplied_path_is_an_error() {
        let loaded = loaded_fixture();
        let absent = load_optional_attachment_observation_artifact(
            None,
            DATASET_DIGEST,
            &processor(),
            loaded.formation_input(),
        )
        .expect("absence is valid");
        assert!(absent.is_none());

        let temp = tempfile::tempdir().expect("create temp directory");
        let missing = temp.path().join("missing.json");
        let supplied_missing = load_optional_attachment_observation_artifact(
            Some(&missing),
            DATASET_DIGEST,
            &processor(),
            loaded.formation_input(),
        );
        assert!(matches!(supplied_missing, Err(BenchError::InvalidInput(_))));
    }

    #[test]
    fn inline_attachment_bytes_re_attest_the_observation_asset_digest() {
        assert_eq!(
            sha256_lower_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let mut loaded = loaded_fixture();
        loaded.sessions[0].turns[0].attachments[0].locator =
            "data:image/png;base64,aGVsbG8=".to_owned();
        let mut inline_artifact = artifact();
        inline_artifact.records[0].asset_sha256 =
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned();
        let temp = tempfile::tempdir().expect("create temp directory");
        let path = write_json(
            &temp,
            "inline.json",
            &serde_json::to_value(&inline_artifact).expect("serialize inline artifact"),
        );
        assert!(
            load_optional_attachment_observation_artifact(
                Some(&path),
                DATASET_DIGEST,
                &processor(),
                loaded.formation_input(),
            )
            .is_ok()
        );

        inline_artifact.records[0].asset_sha256 = ASSET_SHA256.to_owned();
        let mismatch_path = write_json(
            &temp,
            "inline-mismatch.json",
            &serde_json::to_value(inline_artifact).expect("serialize inline mismatch"),
        );
        assert!(matches!(
            load_optional_attachment_observation_artifact(
                Some(&mismatch_path),
                DATASET_DIGEST,
                &processor(),
                loaded.formation_input(),
            ),
            Err(BenchError::InvalidInput(_))
        ));
    }
}
