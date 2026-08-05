use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::extract::types::{
    CandidateKind, ExtractionSource, ExtractionSourceRef, RelationKind, ValidatedCandidate,
    ValidatedExtraction, ValidatedRelation,
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_ITEMS: usize = 32;
const MAX_ITEM_ID_CHARS: usize = 64;
const MAX_LEGACY_CONTENT_CHARS: usize = 500;
// Canonical claims join the independently bounded subject, relation, and
// object with two spaces: 128 + 128 + 256 + 2.
const MAX_CONTENT_CHARS: usize = 514;
const MAX_SUBJECT_CHARS: usize = 128;
const MAX_RELATION_CHARS: usize = 128;
const MAX_OBJECT_CHARS: usize = 256;
const MAX_EVIDENCE_SPAN_CHARS: usize = 500;
const MAX_ENTITY_TAGS: usize = 16;
const MAX_ENTITY_TAG_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidationError {
    InvalidUtf8,
    InvalidJson,
    SchemaReject,
    TooManyItems,
    InvalidItemId,
    DuplicateItemId,
    InvalidContent,
    InvalidGrounding,
    InvalidEvidenceSpan,
    InvalidConfidence,
    InvalidEntityTags,
    InvalidValidityWindow,
    InvalidSourceReference,
    InvalidRelationReference,
    SelfRelation,
    DuplicateCandidateKey,
    DuplicateRelation,
}
impl std::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::InvalidUtf8 => "invalid-utf8",
            Self::InvalidJson => "invalid-json",
            Self::SchemaReject => "schema-reject",
            Self::TooManyItems => "too-many-items",
            Self::InvalidItemId => "invalid-item-id",
            Self::DuplicateItemId => "duplicate-item-id",
            Self::InvalidContent => "invalid-content",
            Self::InvalidGrounding => "invalid-grounding",
            Self::InvalidEvidenceSpan => "invalid-evidence-span",
            Self::InvalidConfidence => "invalid-confidence",
            Self::InvalidEntityTags => "invalid-entity-tags",
            Self::InvalidValidityWindow => "invalid-validity-window",
            Self::InvalidSourceReference => "invalid-source-reference",
            Self::InvalidRelationReference => "invalid-relation-reference",
            Self::SelfRelation => "self-relation",
            Self::DuplicateCandidateKey => "duplicate-candidate-key",
            Self::DuplicateRelation => "duplicate-relation",
        };
        formatter.write_str(label)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtraction {
    items: Vec<RawItem>,
    relations: Vec<RawRelation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    item_local_id: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    relation: Option<String>,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    evidence_object: Option<String>,
    #[serde(default)]
    evidence_span: Option<String>,
    kind: CandidateKind,
    confidence: f64,
    #[serde(default)]
    entity_tags: Vec<String>,
    #[serde(default)]
    valid_from_ms: Option<u64>,
    #[serde(default)]
    valid_until_ms: Option<u64>,
    source_node_ids: Vec<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRelation {
    from_item_local_id: String,
    to_item_local_id: String,
    relation_type: RelationKind,
}

pub(crate) fn validate_output(
    bytes: &[u8],
    batch: &[ExtractionSource],
    profile_id: &str,
) -> Result<ValidatedExtraction, ValidationError> {
    validate_output_for_schema(
        bytes,
        batch,
        profile_id,
        crate::extract::profile::EXTRACT_SCHEMA_VERSION,
    )
}

pub(crate) fn validate_output_for_schema(
    bytes: &[u8],
    batch: &[ExtractionSource],
    profile_id: &str,
    schema_version: u32,
) -> Result<ValidatedExtraction, ValidationError> {
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(ValidationError::SchemaReject);
    }

    let text = std::str::from_utf8(bytes).map_err(|_| ValidationError::InvalidUtf8)?;
    let value: Value = serde_json::from_str(text).map_err(|_| ValidationError::InvalidJson)?;
    let raw: RawExtraction =
        serde_json::from_value(value).map_err(|_| ValidationError::SchemaReject)?;

    if raw.items.len() > MAX_ITEMS {
        return Err(ValidationError::TooManyItems);
    }

    let batch_refs: std::collections::HashMap<_, _> = batch
        .iter()
        .map(|source| {
            (
                source.node_id,
                ExtractionSourceRef {
                    node_id: source.node_id,
                    turn_key: source.turn_key.clone(),
                    content_hash: source.content_hash.clone(),
                },
            )
        })
        .collect();
    let batch_sources: std::collections::HashMap<_, _> = batch
        .iter()
        .map(|source| (source.node_id, source))
        .collect();
    let mut item_ids = HashSet::new();
    let mut items = Vec::with_capacity(raw.items.len());

    for raw_item in raw.items {
        let RawItem {
            item_local_id: raw_item_local_id,
            content: raw_content,
            subject: raw_subject,
            relation: raw_relation,
            object: raw_object,
            evidence_object: raw_evidence_object,
            evidence_span: raw_evidence_span,
            kind,
            confidence,
            entity_tags: raw_entity_tags,
            valid_from_ms,
            valid_until_ms,
            source_node_ids,
        } = raw_item;
        let item_local_id = normalize(&raw_item_local_id);
        if item_local_id.is_empty()
            || item_local_id.chars().count() > MAX_ITEM_ID_CHARS
            || item_local_id.chars().any(char::is_control)
        {
            return Err(ValidationError::InvalidItemId);
        }
        if !item_ids.insert(item_local_id.clone()) {
            return Err(ValidationError::DuplicateItemId);
        }

        if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
            return Err(ValidationError::InvalidConfidence);
        }
        if raw_entity_tags.len() > MAX_ENTITY_TAGS {
            return Err(ValidationError::InvalidEntityTags);
        }
        let mut entity_tags = Vec::with_capacity(raw_entity_tags.len());
        for tag in raw_entity_tags {
            let tag = normalize(&tag);
            if tag.is_empty()
                || tag.chars().count() > MAX_ENTITY_TAG_CHARS
                || tag.chars().any(char::is_control)
            {
                return Err(ValidationError::InvalidEntityTags);
            }
            if entity_tags.iter().any(|existing| existing == &tag) {
                return Err(ValidationError::InvalidEntityTags);
            }
            entity_tags.push(tag);
        }
        entity_tags.sort();
        let (valid_from_ms, valid_until_ms) =
            normalize_validity_window(schema_version, valid_from_ms, valid_until_ms)?;
        if source_node_ids.is_empty() {
            return Err(ValidationError::InvalidSourceReference);
        }

        let mut source_refs = Vec::with_capacity(source_node_ids.len());
        let mut source_ids = HashSet::new();
        for node_id in source_node_ids {
            if !source_ids.insert(node_id) {
                return Err(ValidationError::InvalidSourceReference);
            }
            let Some(source_ref) = batch_refs.get(&node_id) else {
                return Err(ValidationError::InvalidSourceReference);
            };
            source_refs.push(source_ref.clone());
        }
        source_refs.sort_by(|left, right| {
            (
                left.turn_key.as_str(),
                left.node_id,
                left.content_hash.as_str(),
            )
                .cmp(&(
                    right.turn_key.as_str(),
                    right.node_id,
                    right.content_hash.as_str(),
                ))
        });

        let grounding_fields_present = [
            raw_subject.is_some(),
            raw_relation.is_some(),
            raw_object.is_some(),
            raw_evidence_span.is_some(),
        ];
        let base_grounding = grounding_fields_present.iter().all(|present| *present);
        let requires_evidence_object = schema_version >= 5;
        let has_grounding =
            base_grounding && (!requires_evidence_object || raw_evidence_object.is_some());
        let has_partial_grounding = grounding_fields_present.iter().any(|present| *present)
            || raw_evidence_object.is_some();
        if has_partial_grounding != has_grounding || (schema_version >= 4 && !has_grounding) {
            return Err(ValidationError::InvalidGrounding);
        }

        let (
            content,
            subject,
            relation,
            object,
            evidence_object,
            evidence_span,
            evidence_source_node_id,
        ) = if has_grounding {
            let subject = normalize(raw_subject.as_deref().unwrap_or_default());
            let relation = if schema_version >= 5 {
                normalize_relation(raw_relation.as_deref().unwrap_or_default())
            } else {
                normalize(raw_relation.as_deref().unwrap_or_default())
            };
            let object = normalize(raw_object.as_deref().unwrap_or_default());
            let evidence_object = if schema_version >= 5 {
                normalize(raw_evidence_object.as_deref().unwrap_or_default())
            } else {
                object.clone()
            };
            let evidence_span = normalize(raw_evidence_span.as_deref().unwrap_or_default());
            validate_grounding_components(
                &subject,
                &relation,
                &object,
                &evidence_object,
                &evidence_span,
                schema_version,
            )?;

            let evidence_source_node_id = source_refs
                .iter()
                .find_map(|source_ref| {
                    batch_sources
                        .get(&source_ref.node_id)
                        .copied()
                        .filter(|source| source.content.contains(&evidence_span))
                        .map(|source| source.node_id)
                })
                .ok_or(ValidationError::InvalidEvidenceSpan)?;
            let evidence_object_is_grounded = if schema_version >= 5 {
                evidence_span.contains(&evidence_object)
            } else {
                phrase_tokens_contain(&evidence_span, &evidence_object)
            };
            if !evidence_object_is_grounded {
                return Err(ValidationError::InvalidEvidenceSpan);
            }
            let content = canonical_claim(&subject, &relation, &object);
            validate_content(&content, MAX_CONTENT_CHARS)?;
            (
                content,
                Some(subject),
                Some(relation),
                Some(object),
                (schema_version >= 5).then_some(evidence_object),
                Some(evidence_span),
                Some(evidence_source_node_id),
            )
        } else {
            let content = normalize(
                raw_content
                    .as_deref()
                    .ok_or(ValidationError::InvalidContent)?,
            );
            validate_content(&content, MAX_LEGACY_CONTENT_CHARS)?;
            (content, None, None, None, None, None, None)
        };

        let idempotency_key = candidate_key(
            profile_id,
            &source_refs,
            &kind,
            &content,
            GroundingKeyComponents {
                subject: subject.as_deref(),
                relation: relation.as_deref(),
                object: object.as_deref(),
                evidence_object: evidence_object.as_deref(),
                evidence_span: evidence_span.as_deref(),
                evidence_source_node_id,
            },
            &entity_tags,
            (valid_from_ms, valid_until_ms),
        );
        items.push(ValidatedCandidate {
            item_local_id,
            content,
            kind,
            confidence,
            subject,
            relation,
            object,
            evidence_object,
            evidence_span,
            evidence_source_node_id,
            entity_tags,
            valid_from_ms,
            valid_until_ms,
            sources: source_refs,
            idempotency_key,
        });
    }
    items.sort_by(|left, right| left.item_local_id.cmp(&right.item_local_id));

    let candidate_keys_by_id: std::collections::HashMap<_, _> = items
        .iter()
        .map(|item| (item.item_local_id.as_str(), item.idempotency_key.as_str()))
        .collect();
    let mut relations = Vec::with_capacity(raw.relations.len());
    for raw_relation in raw.relations {
        let from_item_local_id = normalize(&raw_relation.from_item_local_id);
        let to_item_local_id = normalize(&raw_relation.to_item_local_id);
        if from_item_local_id.chars().any(char::is_control)
            || to_item_local_id.chars().any(char::is_control)
        {
            return Err(ValidationError::InvalidRelationReference);
        }
        let Some(from_key) = candidate_keys_by_id.get(from_item_local_id.as_str()) else {
            return Err(ValidationError::InvalidRelationReference);
        };
        let Some(to_key) = candidate_keys_by_id.get(to_item_local_id.as_str()) else {
            return Err(ValidationError::InvalidRelationReference);
        };
        if from_item_local_id == to_item_local_id {
            return Err(ValidationError::SelfRelation);
        }

        relations.push(ValidatedRelation {
            from_item_local_id,
            to_item_local_id,
            idempotency_key: relation_key(from_key, to_key, &raw_relation.relation_type),
            relation_type: raw_relation.relation_type,
        });
    }
    let mut candidate_keys = HashSet::new();
    for item in &items {
        if !candidate_keys.insert(item.idempotency_key.as_str()) {
            return Err(ValidationError::DuplicateCandidateKey);
        }
    }

    let mut relation_tuples = HashSet::new();
    for relation in &relations {
        let tuple = (
            relation.from_item_local_id.as_str(),
            relation.to_item_local_id.as_str(),
            relation_kind_name(&relation.relation_type),
        );
        if !relation_tuples.insert(tuple) {
            return Err(ValidationError::DuplicateRelation);
        }
    }
    Ok(ValidatedExtraction { items, relations })
}

fn normalize(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}

fn normalize_relation(value: &str) -> String {
    normalize(value)
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_validity_window(
    schema_version: u32,
    valid_from_ms: Option<u64>,
    valid_until_ms: Option<u64>,
) -> Result<(Option<u64>, Option<u64>), ValidationError> {
    if valid_from_ms
        .zip(valid_until_ms)
        .is_some_and(|(from, until)| until <= from)
    {
        if schema_version >= 5 {
            // An inverted provider interval cannot safely constrain a durable
            // fact. Keep the grounded claim and explicitly discard only the
            // malformed optional interval.
            return Ok((None, None));
        }
        return Err(ValidationError::InvalidValidityWindow);
    }
    Ok((valid_from_ms, valid_until_ms))
}

fn validate_content(content: &str, max_chars: usize) -> Result<(), ValidationError> {
    if content.is_empty()
        || content.chars().count() > max_chars
        || content
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        Err(ValidationError::InvalidContent)
    } else {
        Ok(())
    }
}

fn validate_grounding_components(
    subject: &str,
    relation: &str,
    object: &str,
    evidence_object: &str,
    evidence_span: &str,
    schema_version: u32,
) -> Result<(), ValidationError> {
    let components = [
        (subject, MAX_SUBJECT_CHARS),
        (relation, MAX_RELATION_CHARS),
        (object, MAX_OBJECT_CHARS),
        (evidence_object, MAX_OBJECT_CHARS),
        (evidence_span, MAX_EVIDENCE_SPAN_CHARS),
    ];
    if components.iter().any(|(component, limit)| {
        component.is_empty()
            || component.chars().count() > *limit
            || component
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    }) {
        return Err(ValidationError::InvalidGrounding);
    }

    let subject_has_unresolved_pronoun = phrase_tokens(subject)
        .iter()
        .any(|token| is_unresolved_subject_token(token));
    if subject_has_unresolved_pronoun
        || (schema_version >= 5
            && phrase_tokens(object)
                .iter()
                .any(|token| is_first_person_token(token)))
    {
        return Err(ValidationError::InvalidGrounding);
    }
    Ok(())
}

fn is_first_person_token(token: &str) -> bool {
    matches!(
        token,
        "i" | "me" | "my" | "mine" | "myself" | "we" | "us" | "our" | "ours" | "ourselves"
    )
}

fn is_unresolved_subject_token(token: &str) -> bool {
    is_first_person_token(token)
        || matches!(
            token,
            "you"
                | "your"
                | "yours"
                | "yourself"
                | "yourselves"
                | "he"
                | "him"
                | "his"
                | "himself"
                | "she"
                | "her"
                | "hers"
                | "herself"
                | "they"
                | "them"
                | "their"
                | "theirs"
                | "themselves"
                | "it"
                | "its"
                | "itself"
        )
}

fn canonical_claim(subject: &str, relation: &str, object: &str) -> String {
    format!("{subject} {relation} {object}")
}

fn phrase_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn phrase_tokens_contain(haystack: &str, needle: &str) -> bool {
    let haystack = phrase_tokens(haystack);
    let needle = phrase_tokens(needle);
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle.as_slice())
}

#[derive(Clone, Copy)]
struct GroundingKeyComponents<'a> {
    subject: Option<&'a str>,
    relation: Option<&'a str>,
    object: Option<&'a str>,
    evidence_object: Option<&'a str>,
    evidence_span: Option<&'a str>,
    evidence_source_node_id: Option<u64>,
}

fn candidate_key(
    profile_id: &str,
    sources: &[ExtractionSourceRef],
    kind: &CandidateKind,
    content: &str,
    grounding: GroundingKeyComponents<'_>,
    entity_tags: &[String],
    validity: (Option<u64>, Option<u64>),
) -> String {
    let GroundingKeyComponents {
        subject,
        relation,
        object,
        evidence_object,
        evidence_span,
        evidence_source_node_id,
    } = grounding;
    let (valid_from_ms, valid_until_ms) = validity;
    let mut hasher = Sha256::new();
    hasher.update(profile_id.as_bytes());
    for source in sources {
        hasher.update([0]);
        hasher.update(source.turn_key.as_bytes());
    }
    hasher.update([0]);
    hasher.update(kind_name(kind).as_bytes());
    hasher.update([0]);
    hasher.update(content.as_bytes());
    for component in [subject, relation, object, evidence_object, evidence_span] {
        hasher.update([u8::from(component.is_some())]);
        if let Some(component) = component {
            hasher.update(component.as_bytes());
        }
    }
    hasher.update([u8::from(evidence_source_node_id.is_some())]);
    hasher.update(evidence_source_node_id.unwrap_or_default().to_le_bytes());
    for tag in entity_tags {
        hasher.update([0]);
        hasher.update(tag.as_bytes());
    }
    hasher.update([u8::from(valid_from_ms.is_some())]);
    hasher.update(valid_from_ms.unwrap_or_default().to_le_bytes());
    hasher.update([u8::from(valid_until_ms.is_some())]);
    hasher.update(valid_until_ms.unwrap_or_default().to_le_bytes());
    format!("{:x}", hasher.finalize())
}

fn relation_key(from_key: &str, to_key: &str, relation_type: &RelationKind) -> String {
    let mut hasher = Sha256::new();
    hasher.update(from_key.as_bytes());
    hasher.update([0]);
    hasher.update(to_key.as_bytes());
    hasher.update([0]);
    hasher.update(relation_kind_name(relation_type).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn kind_name(kind: &CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Fact => "fact",
        CandidateKind::Entity => "entity",
        CandidateKind::Event => "event",
        CandidateKind::Preference => "preference",
        CandidateKind::Decision => "decision",
        CandidateKind::Causal => "causal",
        CandidateKind::Lesson => "lesson",
        CandidateKind::Convention => "convention",
        CandidateKind::Gotcha => "gotcha",
    }
}

fn relation_kind_name(kind: &RelationKind) -> &'static str {
    match kind {
        RelationKind::Reason => "reason",
        RelationKind::Causal => "causal",
        RelationKind::Contradicts => "contradicts",
        RelationKind::Supports => "supports",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE_ID: &str = "profile";

    fn batch() -> Vec<ExtractionSource> {
        vec![
            source(7, "turn-a", "hash-a"),
            source(8, "turn-b", "hash-b"),
            source(9, "turn-c", "hash-c"),
        ]
    }

    fn source(node_id: u64, turn_key: &str, content_hash: &str) -> ExtractionSource {
        ExtractionSource {
            node_id,
            turn_key: turn_key.into(),
            session_id: "session".into(),
            scope: "scope".into(),
            content: "source content".into(),
            content_hash: content_hash.into(),
            at_ms: node_id,
        }
    }

    fn validate(bytes: &[u8]) -> Result<ValidatedExtraction, ValidationError> {
        validate_output_for_schema(bytes, &batch(), PROFILE_ID, 3)
    }

    fn valid_output(items: &str, relations: &str) -> Vec<u8> {
        format!(r#"{{"items":[{items}],"relations":[{relations}]}}"#).into_bytes()
    }

    fn grounded_item(subject: &str, relation: &str, object: &str, evidence_span: &str) -> String {
        grounded_item_with_evidence(subject, relation, object, object, evidence_span)
    }

    fn grounded_item_with_evidence(
        subject: &str,
        relation: &str,
        object: &str,
        evidence_object: &str,
        evidence_span: &str,
    ) -> String {
        format!(
            r#"{{"item_local_id":"grounded","subject":"{subject}","relation":"{relation}","object":"{object}","evidence_object":"{evidence_object}","evidence_span":"{evidence_span}","kind":"fact","confidence":0.9,"source_node_ids":[7]}}"#
        )
    }

    #[test]
    fn current_schema_derives_content_and_requires_verbatim_object_grounding() {
        let valid = valid_output(
            &grounded_item("Alice", "reported", "source content", "source content"),
            "",
        );
        let extraction = validate_output(&valid, &batch(), PROFILE_ID).expect("grounded fact");
        assert_eq!(extraction.items[0].content, "Alice reported source content");
        assert_eq!(extraction.items[0].subject.as_deref(), Some("Alice"));
        assert_eq!(
            extraction.items[0].evidence_object.as_deref(),
            Some("source content")
        );
        assert_eq!(
            extraction.items[0].evidence_source_node_id,
            Some(7),
            "the validator, not the provider, resolves the evidence source"
        );

        let legacy = valid_output(&item("legacy", "content", SOURCE_A), "");
        assert_eq!(
            validate_output(&legacy, &batch(), PROFILE_ID),
            Err(ValidationError::InvalidGrounding)
        );
        assert!(
            validate_output_for_schema(&legacy, &batch(), PROFILE_ID, 3).is_ok(),
            "schema-3 staged payloads remain replayable"
        );

        for invalid in [
            grounded_item("I", "reported", "source content", "source content"),
            grounded_item("she", "reported", "source content", "source content"),
            grounded_item(
                "Alice and her friend",
                "reported",
                "source content",
                "source content",
            ),
            grounded_item(
                "Alice and I",
                "reported",
                "source content",
                "source content",
            ),
            grounded_item("Alice", "reported", "missing object", "source content"),
            grounded_item("Alice", "reported", "source content", "not in source"),
        ] {
            assert!(validate_output(&valid_output(&invalid, ""), &batch(), PROFILE_ID).is_err());
        }
    }

    #[test]
    fn canonical_content_allows_the_sum_of_grounding_component_limits() {
        let subject = "s".repeat(MAX_SUBJECT_CHARS);
        let relation = "r".repeat(MAX_RELATION_CHARS);
        let object = "o".repeat(MAX_OBJECT_CHARS);
        validate_grounding_components(&subject, &relation, &object, &object, &object, 5)
            .expect("individually bounded grounding components");

        let content = canonical_claim(&subject, &relation, &object);
        assert_eq!(content.chars().count(), MAX_CONTENT_CHARS);
        assert!(validate_content(&content, MAX_CONTENT_CHARS).is_ok());
        assert_eq!(
            validate_content(&content, MAX_LEGACY_CONTENT_CHARS),
            Err(ValidationError::InvalidContent)
        );
    }

    #[test]
    fn current_schema_separates_canonical_object_from_exact_evidence_object() {
        let valid = valid_output(
            &grounded_item_with_evidence(
                "Alice",
                "cares_for",
                "her family",
                "my family",
                "source content about my family",
            ),
            "",
        );
        let extraction = validate_output(
            &valid,
            &[ExtractionSource {
                content: "source content about my family".into(),
                ..source(7, "turn-a", "hash-a")
            }],
            PROFILE_ID,
        )
        .expect("canonical and evidence objects");
        assert_eq!(extraction.items[0].content, "Alice cares for her family");
        assert_eq!(extraction.items[0].relation.as_deref(), Some("cares for"));
        assert_eq!(extraction.items[0].object.as_deref(), Some("her family"));
        assert_eq!(
            extraction.items[0].evidence_object.as_deref(),
            Some("my family")
        );

        let unresolved = valid_output(
            &grounded_item_with_evidence(
                "Alice",
                "cares for",
                "my family",
                "my family",
                "source content about my family",
            ),
            "",
        );
        assert_eq!(
            validate_output(
                &unresolved,
                &[ExtractionSource {
                    content: "source content about my family".into(),
                    ..source(7, "turn-a", "hash-a")
                }],
                PROFILE_ID,
            ),
            Err(ValidationError::InvalidGrounding)
        );
    }

    #[test]
    fn generic_memory_tags_and_validity_are_canonical_and_validated() {
        let payload = valid_output(
            r#"{"item_local_id":"event","content":"Alice moved to Paris.","kind":"event","confidence":0.9,"entity_tags":["Paris","Alice"],"valid_from_ms":10,"valid_until_ms":20,"source_node_ids":[7]}"#,
            "",
        );
        let extraction = validate(&payload).expect("generic event");
        assert_eq!(extraction.items[0].entity_tags, ["Alice", "Paris"]);
        assert_eq!(extraction.items[0].valid_from_ms, Some(10));
        assert_eq!(extraction.items[0].valid_until_ms, Some(20));

        let invalid_window = valid_output(
            r#"{"item_local_id":"event","content":"Alice moved.","kind":"event","confidence":0.9,"entity_tags":[],"valid_from_ms":20,"valid_until_ms":10,"source_node_ids":[7]}"#,
            "",
        );
        assert_eq!(
            validate(&invalid_window),
            Err(ValidationError::InvalidValidityWindow)
        );
        let empty_window = valid_output(
            r#"{"item_local_id":"event","content":"Alice moved.","kind":"event","confidence":0.9,"entity_tags":[],"valid_from_ms":20,"valid_until_ms":20,"source_node_ids":[7]}"#,
            "",
        );
        assert_eq!(
            validate(&empty_window),
            Err(ValidationError::InvalidValidityWindow)
        );
        let current_invalid_window = valid_output(
            &grounded_item("Alice", "reported", "source content", "source content").replace(
                r#""source_node_ids":[7]"#,
                r#""valid_from_ms":20,"valid_until_ms":10,"source_node_ids":[7]"#,
            ),
            "",
        );
        let normalized =
            validate_output(&current_invalid_window, &batch(), PROFILE_ID).expect("safe fallback");
        assert_eq!(normalized.items[0].valid_from_ms, None);
        assert_eq!(normalized.items[0].valid_until_ms, None);
        let duplicate_tags = valid_output(
            r#"{"item_local_id":"event","content":"Alice moved.","kind":"event","confidence":0.9,"entity_tags":["Alice","Alice"],"valid_from_ms":null,"valid_until_ms":null,"source_node_ids":[7]}"#,
            "",
        );
        assert_eq!(
            validate(&duplicate_tags),
            Err(ValidationError::InvalidEntityTags)
        );
    }

    fn item(id: &str, content: &str, sources: &str) -> String {
        format!(
            r#"{{"item_local_id":"{id}","content":"{content}","kind":"decision","confidence":0.5,"source_node_ids":[{sources}]}}"#
        )
    }

    const SOURCE_A: &str = "7";
    const SOURCE_B: &str = "8";

    #[test]
    fn rejects_invalid_utf8_json_and_schema_with_distinct_errors() {
        let cases: Vec<(&str, Vec<u8>, ValidationError)> = vec![
            (
                "invalid utf8",
                vec![b'{', 0xff, b'}'],
                ValidationError::InvalidUtf8,
            ),
            (
                "invalid json",
                br#"{"items":[}"#.to_vec(),
                ValidationError::InvalidJson,
            ),
            (
                "output over one MiB",
                vec![b' '; MAX_OUTPUT_BYTES + 1],
                ValidationError::SchemaReject,
            ),
            (
                "unknown output field",
                br#"{"items":[],"relations":[],"unexpected":true}"#.to_vec(),
                ValidationError::SchemaReject,
            ),
            (
                "wrong output field type",
                br#"{"items":{},"relations":[]}"#.to_vec(),
                ValidationError::SchemaReject,
            ),
            (
                "unknown candidate kind",
                valid_output(
                    &item("one", "content", SOURCE_A).replace("decision", "unknown"),
                    "",
                ),
                ValidationError::SchemaReject,
            ),
            (
                "unknown relation type",
                valid_output(
                    &item("one", "content", SOURCE_A),
                    r#"{"from_item_local_id":"one","to_item_local_id":"two","relation_type":"unknown"}"#,
                ),
                ValidationError::SchemaReject,
            ),
        ];

        for (name, bytes, expected) in cases {
            assert_eq!(validate(&bytes), Err(expected), "{name}");
        }
    }

    #[test]
    fn rejects_item_limits_ids_content_and_confidence() {
        let thirty_three_items = (0..33)
            .map(|index| item(&format!("item-{index}"), "content", SOURCE_A))
            .collect::<Vec<_>>()
            .join(",");
        let cjk_501 = "界".repeat(501);
        let cases = vec![
            (
                "thirty-three items",
                valid_output(&thirty_three_items, ""),
                ValidationError::TooManyItems,
            ),
            (
                "empty id",
                valid_output(&item("", "content", SOURCE_A), ""),
                ValidationError::InvalidItemId,
            ),
            (
                "id longer than 64 characters",
                valid_output(&item(&"x".repeat(65), "content", SOURCE_A), ""),
                ValidationError::InvalidItemId,
            ),
            (
                "id containing an escaped terminal control",
                valid_output(&item("bad\\u001b", "content", SOURCE_A), ""),
                ValidationError::InvalidItemId,
            ),
            (
                "duplicate id",
                valid_output(
                    &format!(
                        "{},{}",
                        item("one", "first", SOURCE_A),
                        item("one", "second", SOURCE_B)
                    ),
                    "",
                ),
                ValidationError::DuplicateItemId,
            ),
            (
                "empty content",
                valid_output(&item("one", "", SOURCE_A), ""),
                ValidationError::InvalidContent,
            ),
            (
                "501 CJK characters",
                valid_output(&item("one", &cjk_501, SOURCE_A), ""),
                ValidationError::InvalidContent,
            ),
            (
                "content containing an escaped terminal control",
                valid_output(&item("one", "corrupt\\u001b[8D text", SOURCE_A), ""),
                ValidationError::InvalidContent,
            ),
            (
                "nonfinite confidence",
                valid_output(
                    &item("one", "content", SOURCE_A).replace("0.5", "1e999"),
                    "",
                ),
                ValidationError::InvalidJson,
            ),
            (
                "confidence below range",
                valid_output(
                    &item("one", "content", SOURCE_A).replace("0.5", "-0.01"),
                    "",
                ),
                ValidationError::InvalidConfidence,
            ),
            (
                "confidence above range",
                valid_output(&item("one", "content", SOURCE_A).replace("0.5", "1.01"), ""),
                ValidationError::InvalidConfidence,
            ),
        ];

        for (name, bytes, expected) in cases {
            assert_eq!(validate(&bytes), Err(expected), "{name}");
        }
    }

    #[test]
    fn rejects_duplicate_candidate_keys_and_invalid_source_references() {
        let cases = vec![
            (
                "duplicate candidate key",
                valid_output(
                    &format!(
                        "{},{}",
                        item("one", "same", SOURCE_A),
                        item("two", "same", SOURCE_A)
                    ),
                    "",
                ),
                ValidationError::DuplicateCandidateKey,
            ),
            (
                "whole batch is rejected when one item has a foreign source",
                valid_output(
                    &format!(
                        "{},{}",
                        item("valid", "valid content", SOURCE_A),
                        item("invalid", "invalid content", "99",),
                    ),
                    "",
                ),
                ValidationError::InvalidSourceReference,
            ),
            (
                "foreign source",
                valid_output(&item("one", "content", "99"), ""),
                ValidationError::InvalidSourceReference,
            ),
            (
                "duplicate source",
                valid_output(
                    &item("one", "content", &format!("{SOURCE_A},{SOURCE_A}")),
                    "",
                ),
                ValidationError::InvalidSourceReference,
            ),
            (
                "empty sources",
                valid_output(&item("one", "content", ""), ""),
                ValidationError::InvalidSourceReference,
            ),
        ];

        for (name, bytes, expected) in cases {
            assert_eq!(validate(&bytes), Err(expected), "{name}");
        }
    }

    #[test]
    fn rejects_duplicate_and_invalid_relations_as_whole_batches() {
        let items = format!(
            "{},{}",
            item("one", "first", SOURCE_A),
            item("two", "second", SOURCE_B)
        );
        let relation =
            r#"{"from_item_local_id":"one","to_item_local_id":"two","relation_type":"supports"}"#;
        let cases = vec![
            (
                "duplicate relation tuple",
                valid_output(&items, &format!("{relation},{relation}")),
                ValidationError::DuplicateRelation,
            ),
            (
                "missing from endpoint",
                valid_output(
                    &items,
                    r#"{"from_item_local_id":"missing","to_item_local_id":"two","relation_type":"supports"}"#,
                ),
                ValidationError::InvalidRelationReference,
            ),
            (
                "missing to endpoint",
                valid_output(
                    &items,
                    r#"{"from_item_local_id":"one","to_item_local_id":"missing","relation_type":"supports"}"#,
                ),
                ValidationError::InvalidRelationReference,
            ),
            (
                "self relation",
                valid_output(
                    &items,
                    r#"{"from_item_local_id":"one","to_item_local_id":"one","relation_type":"supports"}"#,
                ),
                ValidationError::SelfRelation,
            ),
        ];

        for (name, bytes, expected) in cases {
            assert_eq!(validate(&bytes), Err(expected), "{name}");
        }
    }

    #[test]
    fn source_and_item_order_do_not_change_canonical_output() {
        let first = valid_output(
            &format!(
                "{},{}",
                item("one", "first", &format!("{SOURCE_B},{SOURCE_A}")),
                item("two", "second", SOURCE_A)
            ),
            r#"{"from_item_local_id":"one","to_item_local_id":"two","relation_type":"supports"}"#,
        );
        let second = valid_output(
            &format!(
                "{},{}",
                item("two", "second", SOURCE_A),
                item("one", "first", &format!("{SOURCE_A},{SOURCE_B}"))
            ),
            r#"{"from_item_local_id":"one","to_item_local_id":"two","relation_type":"supports"}"#,
        );

        assert_eq!(
            validate_output_for_schema(&first, &batch(), PROFILE_ID, 3).expect("first valid"),
            validate_output_for_schema(
                &second,
                &[
                    source(9, "turn-c", "hash-c"),
                    source(8, "turn-b", "hash-b"),
                    source(7, "turn-a", "hash-a"),
                ],
                PROFILE_ID,
                3,
            )
            .expect("second valid"),
        );
    }

    #[test]
    fn outer_trim_and_newline_are_canonicalized_but_internal_whitespace_and_case_are_not() {
        let trimmed = valid_output(&item("one", "durable memory", SOURCE_A), "");
        let padded = valid_output(&item("one", "  durable memory\\n", SOURCE_A), "");
        let internal_whitespace = valid_output(&item("one", "durable  memory", SOURCE_A), "");
        let different_case = valid_output(&item("one", "Durable memory", SOURCE_A), "");

        let canonical = validate(&trimmed).expect("trimmed valid");
        assert_eq!(canonical, validate(&padded).expect("padded valid"));
        assert_ne!(
            canonical,
            validate(&internal_whitespace).expect("internal whitespace valid")
        );
        assert_ne!(
            canonical,
            validate(&different_case).expect("different case valid")
        );
    }

    #[test]
    fn source_references_expand_from_authoritative_turn_key_aligned_batch() {
        let aligned_batch = vec![source(7, "turn-b", "hash-b")];
        let bytes = valid_output(&item("one", "content", "7"), "");

        let extraction = validate_output_for_schema(&bytes, &aligned_batch, PROFILE_ID, 3)
            .expect("valid authoritative source");
        assert_eq!(extraction.items[0].sources[0].turn_key, "turn-b");
        assert_eq!(extraction.items[0].sources[0].content_hash, "hash-b");

        let provider_reference_fields = valid_output(
            &item("one", "content", "7").replace(
                r#""source_node_ids":[7]"#,
                r#""sources":[{"node_id":7,"turn_key":"turn-b","content_hash":"hash-b"}]"#,
            ),
            "",
        );
        assert_eq!(
            validate_output_for_schema(&provider_reference_fields, &aligned_batch, PROFILE_ID, 3,),
            Err(ValidationError::SchemaReject),
        );
    }

    #[test]
    fn relation_direction_is_preserved() {
        let bytes = valid_output(
            &format!(
                "{},{}",
                item("one", "first", SOURCE_A),
                item("two", "second", SOURCE_B)
            ),
            r#"{"from_item_local_id":"two","to_item_local_id":"one","relation_type":"supports"}"#,
        );

        let extraction = validate(&bytes).expect("valid relation");
        assert_eq!(extraction.relations[0].from_item_local_id, "two");
        assert_eq!(extraction.relations[0].to_item_local_id, "one");
    }
    #[test]
    fn validation_errors_display_stable_sanitized_labels() {
        let cases = [
            (ValidationError::InvalidUtf8, "invalid-utf8"),
            (ValidationError::InvalidJson, "invalid-json"),
            (ValidationError::SchemaReject, "schema-reject"),
            (ValidationError::TooManyItems, "too-many-items"),
            (ValidationError::InvalidItemId, "invalid-item-id"),
            (ValidationError::DuplicateItemId, "duplicate-item-id"),
            (ValidationError::InvalidContent, "invalid-content"),
            (ValidationError::InvalidGrounding, "invalid-grounding"),
            (
                ValidationError::InvalidEvidenceSpan,
                "invalid-evidence-span",
            ),
            (ValidationError::InvalidConfidence, "invalid-confidence"),
            (
                ValidationError::InvalidSourceReference,
                "invalid-source-reference",
            ),
            (
                ValidationError::InvalidRelationReference,
                "invalid-relation-reference",
            ),
            (ValidationError::SelfRelation, "self-relation"),
            (
                ValidationError::DuplicateCandidateKey,
                "duplicate-candidate-key",
            ),
            (ValidationError::DuplicateRelation, "duplicate-relation"),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }
}
