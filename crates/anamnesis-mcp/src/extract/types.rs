use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtractorProfileComponents {
    pub provider_id: String,
    pub model_id: String,
    pub prompt_version: u32,
    pub schema_version: u32,
    pub normalization_version: u32,
    pub relation_policy_version: u32,
    pub command_hash: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtractionSource {
    pub node_id: u64,
    pub turn_key: String,
    pub session_id: String,
    pub scope: String,
    pub content: String,
    pub content_hash: String,
    pub at_ms: u64,
}
impl fmt::Debug for ExtractionSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtractionSource")
            .field("node_id", &self.node_id)
            .field("turn_key", &self.turn_key)
            .field("session_id", &self.session_id)
            .field("scope", &self.scope)
            .field("content", &"[REDACTED]")
            .field("content_hash", &self.content_hash)
            .field("at_ms", &self.at_ms)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ValidatedCandidate {
    pub item_local_id: String,
    pub content: String,
    pub kind: CandidateKind,
    pub confidence: f64,
    /// Canonical, non-pronominal subject extracted from the cited evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Short relation phrase joining `subject` to `object`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    /// Canonical, non-pronominal object used to assemble `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// Answer-bearing object copied verbatim from `evidence_span`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_object: Option<String>,
    /// Verbatim substring of one cited raw source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_span: Option<String>,
    /// Authoritative cited source that contains `evidence_span`.
    ///
    /// This is derived by validation rather than trusted from provider output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_source_node_id: Option<u64>,
    #[serde(default)]
    pub entity_tags: Vec<String>,
    #[serde(default)]
    pub valid_from_ms: Option<u64>,
    #[serde(default)]
    pub valid_until_ms: Option<u64>,
    pub sources: Vec<ExtractionSourceRef>,
    pub idempotency_key: String,
}

impl fmt::Debug for ValidatedCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let evidence_span = self.evidence_span.as_ref().map(|_| "[REDACTED]");
        formatter
            .debug_struct("ValidatedCandidate")
            .field("item_local_id", &self.item_local_id)
            .field("content", &self.content)
            .field("kind", &self.kind)
            .field("confidence", &self.confidence)
            .field("subject", &self.subject)
            .field("relation", &self.relation)
            .field("object", &self.object)
            .field("evidence_object", &self.evidence_object)
            .field("evidence_span", &evidence_span)
            .field("evidence_source_node_id", &self.evidence_source_node_id)
            .field("entity_tags", &self.entity_tags)
            .field("valid_from_ms", &self.valid_from_ms)
            .field("valid_until_ms", &self.valid_until_ms)
            .field("sources", &self.sources)
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtractionSourceRef {
    pub node_id: u64,
    pub turn_key: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CandidateKind {
    Fact,
    Entity,
    Event,
    Preference,
    Decision,
    Causal,
    Lesson,
    Convention,
    Gotcha,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RelationKind {
    Reason,
    Causal,
    Contradicts,
    Supports,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ValidatedRelation {
    pub from_item_local_id: String,
    pub to_item_local_id: String,
    pub relation_type: RelationKind,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ValidatedExtraction {
    pub items: Vec<ValidatedCandidate>,
    pub relations: Vec<ValidatedRelation>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ExtractionScanResult {
    pub profile_id: String,
    pub sources: Vec<ExtractionSource>,
}
impl fmt::Debug for ExtractionScanResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtractionScanResult")
            .field("profile_id", &self.profile_id)
            .field("sources_len", &self.sources.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AuditSupport {
    Supported,
    Partial,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ContaminationCategory {
    UnsupportedClaim,
    PromptInjection,
    SecretReexposure,
    ForeignScope,
    ContradictsSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RelationVerdict {
    Correct,
    WrongType,
    WrongDirection,
    Invalid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;

    fn assert_round_trip<T>(value: T, json: &str)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let encoded = serde_json::to_string(&value).expect("serialize test value");
        assert_eq!(encoded, json);
        let decoded = serde_json::from_str::<T>(&encoded).expect("deserialize test value");
        assert_eq!(decoded, value);
    }

    fn assert_unknown_rejected<T>()
    where
        T: DeserializeOwned,
    {
        assert!(serde_json::from_str::<T>("\"unknown\"").is_err());
    }

    #[test]
    fn candidate_kind_uses_kebab_case_and_rejects_unknown_values() {
        for (value, json) in [
            (CandidateKind::Fact, "\"fact\""),
            (CandidateKind::Entity, "\"entity\""),
            (CandidateKind::Event, "\"event\""),
            (CandidateKind::Preference, "\"preference\""),
            (CandidateKind::Decision, "\"decision\""),
            (CandidateKind::Causal, "\"causal\""),
            (CandidateKind::Lesson, "\"lesson\""),
            (CandidateKind::Convention, "\"convention\""),
            (CandidateKind::Gotcha, "\"gotcha\""),
        ] {
            assert_round_trip(value, json);
        }
        assert_unknown_rejected::<CandidateKind>();
    }

    #[test]
    fn relation_kind_uses_kebab_case_and_rejects_unknown_values() {
        for (value, json) in [
            (RelationKind::Reason, "\"reason\""),
            (RelationKind::Causal, "\"causal\""),
            (RelationKind::Contradicts, "\"contradicts\""),
            (RelationKind::Supports, "\"supports\""),
        ] {
            assert_round_trip(value, json);
        }
        assert_unknown_rejected::<RelationKind>();
    }

    #[test]
    fn audit_support_uses_kebab_case_and_rejects_unknown_values() {
        for (value, json) in [
            (AuditSupport::Supported, "\"supported\""),
            (AuditSupport::Partial, "\"partial\""),
            (AuditSupport::Unsupported, "\"unsupported\""),
        ] {
            assert_round_trip(value, json);
        }
        assert_unknown_rejected::<AuditSupport>();
    }

    #[test]
    fn contamination_category_uses_kebab_case_and_rejects_unknown_values() {
        for (value, json) in [
            (
                ContaminationCategory::UnsupportedClaim,
                "\"unsupported-claim\"",
            ),
            (
                ContaminationCategory::PromptInjection,
                "\"prompt-injection\"",
            ),
            (
                ContaminationCategory::SecretReexposure,
                "\"secret-reexposure\"",
            ),
            (ContaminationCategory::ForeignScope, "\"foreign-scope\""),
            (
                ContaminationCategory::ContradictsSource,
                "\"contradicts-source\"",
            ),
        ] {
            assert_round_trip(value, json);
        }
        assert_unknown_rejected::<ContaminationCategory>();
    }

    #[test]
    fn relation_verdict_uses_kebab_case_and_rejects_unknown_values() {
        for (value, json) in [
            (RelationVerdict::Correct, "\"correct\""),
            (RelationVerdict::WrongType, "\"wrong-type\""),
            (RelationVerdict::WrongDirection, "\"wrong-direction\""),
            (RelationVerdict::Invalid, "\"invalid\""),
        ] {
            assert_round_trip(value, json);
        }
        assert_unknown_rejected::<RelationVerdict>();
    }

    #[test]
    fn extraction_domain_structs_round_trip() {
        let profile = ExtractorProfileComponents {
            provider_id: "provider".into(),
            model_id: "model".into(),
            prompt_version: 1,
            schema_version: 2,
            normalization_version: 3,
            relation_policy_version: 4,
            command_hash: "command-hash".into(),
        };
        assert_round_trip(
            profile,
            r#"{"provider_id":"provider","model_id":"model","prompt_version":1,"schema_version":2,"normalization_version":3,"relation_policy_version":4,"command_hash":"command-hash"}"#,
        );

        let source = ExtractionSource {
            node_id: 7,
            turn_key: "turn".into(),
            session_id: "session".into(),
            scope: "scope".into(),
            content: "content".into(),
            content_hash: "content-hash".into(),
            at_ms: 8,
        };
        let source_ref = ExtractionSourceRef {
            node_id: source.node_id,
            turn_key: source.turn_key.clone(),
            content_hash: source.content_hash.clone(),
        };
        let candidate = ValidatedCandidate {
            item_local_id: "item".into(),
            content: "candidate".into(),
            kind: CandidateKind::Decision,
            confidence: 0.75,
            subject: None,
            relation: None,
            object: None,
            evidence_object: None,
            evidence_span: None,
            evidence_source_node_id: None,
            entity_tags: vec!["project".into()],
            valid_from_ms: Some(10),
            valid_until_ms: None,
            sources: vec![source_ref],
            idempotency_key: "candidate-key".into(),
        };
        let relation = ValidatedRelation {
            from_item_local_id: "item".into(),
            to_item_local_id: "other-item".into(),
            relation_type: RelationKind::Supports,
            idempotency_key: "relation-key".into(),
        };
        assert_round_trip(
            ValidatedExtraction {
                items: vec![candidate],
                relations: vec![relation],
            },
            r#"{"items":[{"item_local_id":"item","content":"candidate","kind":"decision","confidence":0.75,"entity_tags":["project"],"valid_from_ms":10,"valid_until_ms":null,"sources":[{"node_id":7,"turn_key":"turn","content_hash":"content-hash"}],"idempotency_key":"candidate-key"}],"relations":[{"from_item_local_id":"item","to_item_local_id":"other-item","relation_type":"supports","idempotency_key":"relation-key"}]}"#,
        );
        assert_round_trip(
            ExtractionScanResult {
                profile_id: "profile".into(),
                sources: vec![source],
            },
            r#"{"profile_id":"profile","sources":[{"node_id":7,"turn_key":"turn","session_id":"session","scope":"scope","content":"content","content_hash":"content-hash","at_ms":8}]}"#,
        );
    }
    #[test]
    fn source_debug_redacts_transcript_content() {
        let source = ExtractionSource {
            node_id: 7,
            turn_key: "turn".into(),
            session_id: "session".into(),
            scope: "scope".into(),
            content: "do not expose this transcript".into(),
            content_hash: "content-hash".into(),
            at_ms: 8,
        };
        let source_debug = format!("{source:?}");
        let scan_debug = format!(
            "{:?}",
            ExtractionScanResult {
                profile_id: "profile".into(),
                sources: vec![source],
            }
        );

        assert!(!source_debug.contains("do not expose this transcript"));
        assert!(source_debug.contains("[REDACTED]"));
        assert!(!scan_debug.contains("do not expose this transcript"));
        assert!(scan_debug.contains("sources_len: 1"));
    }

    #[test]
    fn candidate_debug_redacts_evidence_span() {
        let candidate = ValidatedCandidate {
            item_local_id: "item".into(),
            content: "Alice chose tea".into(),
            kind: CandidateKind::Decision,
            confidence: 0.9,
            subject: Some("Alice".into()),
            relation: Some("chose".into()),
            object: Some("tea".into()),
            evidence_object: Some("tea".into()),
            evidence_span: Some("raw private evidence must stay hidden".into()),
            evidence_source_node_id: Some(7),
            entity_tags: Vec::new(),
            valid_from_ms: None,
            valid_until_ms: None,
            sources: vec![ExtractionSourceRef {
                node_id: 7,
                turn_key: "turn".into(),
                content_hash: "content-hash".into(),
            }],
            idempotency_key: "candidate-key".into(),
        };

        let debug = format!("{candidate:?}");
        assert!(!debug.contains("raw private evidence must stay hidden"));
        assert!(debug.contains("[REDACTED]"));
    }
}
