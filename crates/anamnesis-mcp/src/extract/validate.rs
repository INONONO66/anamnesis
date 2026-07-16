#![cfg_attr(
    not(test),
    allow(dead_code, reason = "Task 7 consumes validation APIs")
)]

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::extract::types::{
    CandidateKind, ExtractionSource, ExtractionSourceRef, RelationKind, ValidatedCandidate,
    ValidatedExtraction, ValidatedRelation,
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_ITEMS: usize = 10;
const MAX_ITEM_ID_CHARS: usize = 64;
const MAX_CONTENT_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidationError {
    InvalidUtf8,
    InvalidJson,
    SchemaReject,
    TooManyItems,
    InvalidItemId,
    DuplicateItemId,
    InvalidContent,
    InvalidConfidence,
    InvalidSourceReference,
    InvalidRelationReference,
    SelfRelation,
    DuplicateCandidateKey,
    DuplicateRelation,
}

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
    content: String,
    kind: CandidateKind,
    confidence: f64,
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
    let mut item_ids = HashSet::new();
    let mut items = Vec::with_capacity(raw.items.len());

    for raw_item in raw.items {
        let item_local_id = normalize(&raw_item.item_local_id);
        if item_local_id.is_empty() || item_local_id.chars().count() > MAX_ITEM_ID_CHARS {
            return Err(ValidationError::InvalidItemId);
        }
        if !item_ids.insert(item_local_id.clone()) {
            return Err(ValidationError::DuplicateItemId);
        }

        let content = normalize(&raw_item.content);
        if content.is_empty() || content.chars().count() > MAX_CONTENT_CHARS {
            return Err(ValidationError::InvalidContent);
        }
        if !raw_item.confidence.is_finite() || !(0.0..=1.0).contains(&raw_item.confidence) {
            return Err(ValidationError::InvalidConfidence);
        }
        if raw_item.source_node_ids.is_empty() {
            return Err(ValidationError::InvalidSourceReference);
        }

        let mut source_refs = Vec::with_capacity(raw_item.source_node_ids.len());
        let mut source_ids = HashSet::new();
        for node_id in raw_item.source_node_ids {
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

        let idempotency_key = candidate_key(profile_id, &source_refs, &raw_item.kind, &content);
        items.push(ValidatedCandidate {
            item_local_id,
            content,
            kind: raw_item.kind,
            confidence: raw_item.confidence,
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

fn candidate_key(
    profile_id: &str,
    sources: &[ExtractionSourceRef],
    kind: &CandidateKind,
    content: &str,
) -> String {
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
        validate_output(bytes, &batch(), PROFILE_ID)
    }

    fn valid_output(items: &str, relations: &str) -> Vec<u8> {
        format!(r#"{{"items":[{items}],"relations":[{relations}]}}"#).into_bytes()
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
        let eleven_items = (0..11)
            .map(|index| item(&format!("item-{index}"), "content", SOURCE_A))
            .collect::<Vec<_>>()
            .join(",");
        let cjk_501 = "界".repeat(501);
        let cases = vec![
            (
                "eleven items",
                valid_output(&eleven_items, ""),
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
            validate_output(&first, &batch(), PROFILE_ID).expect("first valid"),
            validate_output(
                &second,
                &[
                    source(9, "turn-c", "hash-c"),
                    source(8, "turn-b", "hash-b"),
                    source(7, "turn-a", "hash-a"),
                ],
                PROFILE_ID,
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

        let extraction = validate_output(&bytes, &aligned_batch, PROFILE_ID)
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
            validate_output(&provider_reference_fields, &aligned_batch, PROFILE_ID),
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
}
