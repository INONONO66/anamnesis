// Task 1 stages these domain APIs; remove this allowance as Tasks 2-8 wire consumers.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "Task 1 staged APIs are consumed by Tasks 2-8")
)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtractionSource {
    pub node_id: u64,
    pub turn_key: String,
    pub session_id: String,
    pub scope: String,
    pub content: String,
    pub content_hash: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ValidatedCandidate {
    pub item_local_id: String,
    pub content: String,
    pub kind: CandidateKind,
    pub confidence: f64,
    pub sources: Vec<ExtractionSourceRef>,
    pub idempotency_key: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ExtractionScanResult {
    pub profile_id: String,
    pub sources: Vec<ExtractionSource>,
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
            r#"{"items":[{"item_local_id":"item","content":"candidate","kind":"decision","confidence":0.75,"sources":[{"node_id":7,"turn_key":"turn","content_hash":"content-hash"}],"idempotency_key":"candidate-key"}],"relations":[{"from_item_local_id":"item","to_item_local_id":"other-item","relation_type":"supports","idempotency_key":"relation-key"}]}"#,
        );
        assert_round_trip(
            ExtractionScanResult {
                profile_id: "profile".into(),
                sources: vec![source],
            },
            r#"{"profile_id":"profile","sources":[{"node_id":7,"turn_key":"turn","session_id":"session","scope":"scope","content":"content","content_hash":"content-hash","at_ms":8}]}"#,
        );
    }
}
