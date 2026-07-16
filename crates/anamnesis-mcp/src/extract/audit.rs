use serde::{Deserialize, Serialize};

use crate::extract::types::{
    AuditSupport, CandidateKind, ContaminationCategory, RelationKind, RelationVerdict,
};

/// Stored extraction-audit snapshots, enriched by dispatch with live source data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct ExtractionAuditResult {
    pub candidates: Vec<ExtractionAuditCandidateRow>,
    pub relations: Vec<ExtractionAuditRelationRow>,
}

/// A staged candidate and its audit provenance. Source identity is persisted; source
/// content is populated only while rendering a live audit response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ExtractionAuditCandidateRow {
    pub id: u64,
    pub run_id: u64,
    pub profile_id: String,
    pub item_local_id: String,
    pub content: String,
    pub kind: CandidateKind,
    pub confidence: f64,
    pub source_turn_keys: Vec<String>,
    pub source_content_hashes: Vec<String>,
    pub source_session_id: String,
    pub source_scope: String,
    pub source_node_ids: Vec<u64>,
    #[serde(rename = "audit_support")]
    pub support: Option<AuditSupport>,
    #[serde(rename = "contamination_category")]
    pub contamination: Option<ContaminationCategory>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<u64>,
    #[serde(default)]
    pub sources: Vec<ExtractionAuditSource>,
}

/// A staged relation and its audit provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ExtractionAuditRelationRow {
    pub id: u64,
    pub run_id: u64,
    pub profile_id: String,
    pub candidate_from: u64,
    pub candidate_to: u64,
    pub from_item_local_id: String,
    pub to_item_local_id: String,
    pub relation_type: RelationKind,
    #[serde(rename = "audit_status")]
    pub verdict: Option<RelationVerdict>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<u64>,
}

/// Identity and live availability of a source cited by a staged candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtractionAuditSource {
    pub node_id: u64,
    pub turn_key: String,
    pub session_id: String,
    pub scope: String,
    pub content_hash: String,
    pub content: Option<String>,
    pub availability: ExtractionAuditSourceAvailability,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ExtractionAuditSourceAvailability {
    Available,
    SourceUnavailable,
    SourceMismatch,
}

impl ExtractionAuditSourceAvailability {
    fn unavailable_message(self) -> Option<&'static str> {
        match self {
            Self::Available => None,
            Self::SourceUnavailable => Some("source-unavailable"),
            Self::SourceMismatch => Some("source-mismatch"),
        }
    }
}

/// Choose the audit reviewer without retaining unbounded or whitespace-only input.
pub(crate) fn resolve_reviewer(explicit: Option<&str>) -> String {
    fn normalize(reviewer: &str) -> Option<String> {
        let reviewer = reviewer.trim();
        (!reviewer.is_empty()).then(|| reviewer.chars().take(128).collect())
    }

    explicit
        .and_then(normalize)
        .or_else(|| {
            std::env::var("ANAMNESIS_AUDIT_REVIEWER")
                .ok()
                .as_deref()
                .and_then(normalize)
        })
        .or_else(|| std::env::var("USER").ok().as_deref().and_then(normalize))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Render staged candidates beside their current live source text.
pub(crate) fn render_audit_report(result: &ExtractionAuditResult) -> String {
    let mut report = String::new();

    for candidate in &result.candidates {
        report.push_str(&format!(
            "CANDIDATE {} [{} {:?} {:.3}]: {}\n",
            candidate.id,
            candidate.item_local_id,
            candidate.kind,
            candidate.confidence,
            candidate.content,
        ));
        for source in &candidate.sources {
            report.push_str(&format!(
                "  SOURCE {} [{} {}]: ",
                source.node_id, source.turn_key, source.content_hash,
            ));
            match source.availability.unavailable_message() {
                Some(availability) => {
                    report.push_str("AUDIT UNAVAILABLE: ");
                    report.push_str(availability);
                }
                None => report.push_str(
                    source
                        .content
                        .as_deref()
                        .unwrap_or("AUDIT UNAVAILABLE: source-unavailable"),
                ),
            }
            report.push('\n');
        }
    }

    for relation in &result.relations {
        report.push_str(&format!(
            "RELATION {} [{} -> {} {:?}]: {} -> {}\n",
            relation.id,
            relation.candidate_from,
            relation.candidate_to,
            relation.relation_type,
            relation.from_item_local_id,
            relation.to_item_local_id,
        ));
    }

    report
}

#[cfg(test)]
mod tests {
    use super::resolve_reviewer;

    struct EnvRestore {
        audit_reviewer: Option<std::ffi::OsString>,
        user: Option<std::ffi::OsString>,
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            unsafe {
                match &self.audit_reviewer {
                    Some(value) => std::env::set_var("ANAMNESIS_AUDIT_REVIEWER", value),
                    None => std::env::remove_var("ANAMNESIS_AUDIT_REVIEWER"),
                }
                match &self.user {
                    Some(value) => std::env::set_var("USER", value),
                    None => std::env::remove_var("USER"),
                }
            }
        }
    }

    #[test]
    fn reviewer_uses_explicit_then_env_then_user_then_unknown_and_normalizes() {
        let explicit = format!("  {}  ", "x".repeat(129));
        assert_eq!(resolve_reviewer(Some(&explicit)), "x".repeat(128));

        let _restore = EnvRestore {
            audit_reviewer: std::env::var_os("ANAMNESIS_AUDIT_REVIEWER"),
            user: std::env::var_os("USER"),
        };
        unsafe {
            std::env::set_var("ANAMNESIS_AUDIT_REVIEWER", "  audit reviewer  ");
            std::env::set_var("USER", "  shell user  ");
        }
        assert_eq!(resolve_reviewer(None), "audit reviewer");

        unsafe { std::env::remove_var("ANAMNESIS_AUDIT_REVIEWER") };
        assert_eq!(resolve_reviewer(None), "shell user");

        unsafe { std::env::set_var("USER", "   ") };
        assert_eq!(resolve_reviewer(None), "unknown");
    }
}
