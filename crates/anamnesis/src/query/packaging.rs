//! Adaptive packaging policy for search result assembly.
//!
//! Determines the appropriate `PackagingMode` based on result characteristics:
//! - High tension (Contradicts edges) → KnowledgeWithProvenance
//! - Persona bias requested → PersonaWeighted
//! - Temporal keywords in query → Timeline
//! - Default → Balanced (preserve the readout's bucket shape; readout-scoring.md "Bucket Handling")

use crate::query::types::{PackagingMode, SearchPlan, Tension};

/// Decide the packaging mode based on result characteristics.
///
/// Rules (in priority order):
/// 1. If tensions are present → `KnowledgeWithProvenance`
/// 2. If persona bias is requested → `PersonaWeighted`
/// 3. If query contains temporal keywords → `Timeline`
/// 4. Default → `Balanced` (preserve the readout's bucket shape; readout-scoring.md "Bucket Handling")
pub(crate) fn decide_packaging(
    tensions: &[Tension],
    plan: &SearchPlan,
    query_text: &str,
) -> PackagingMode {
    if !tensions.is_empty() {
        return PackagingMode::KnowledgeWithProvenance;
    }

    if plan.use_persona_bias {
        return PackagingMode::PersonaWeighted;
    }

    if !plan.time_cues.is_empty() || has_temporal_packaging_cue(query_text) {
        return PackagingMode::Timeline;
    }

    PackagingMode::Balanced
}

fn has_temporal_packaging_cue(query_text: &str) -> bool {
    let q_lower = query_text.to_lowercase();
    if ["최근", "언제"]
        .iter()
        .any(|keyword| q_lower.contains(keyword))
    {
        return true;
    }
    q_lower
        .split(|character: char| !character.is_alphanumeric())
        .any(|token| {
            matches!(
                token,
                "when"
                    | "recent"
                    | "recently"
                    | "latest"
                    | "history"
                    | "timeline"
                    | "before"
                    | "after"
                    | "ago"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::has_temporal_packaging_cue;

    #[test]
    fn temporal_packaging_cues_use_word_boundaries() {
        for query in [
            "when did it happen?",
            "what changed recently",
            "events before launch",
            "언제 바뀌었어?",
            "최근 변경사항",
        ] {
            assert!(has_temporal_packaging_cue(query), "{query}");
        }
        for query in [
            "whenever the hook runs",
            "afterparty planning",
            "the latestRelease helper",
            "a beforehand check",
        ] {
            assert!(!has_temporal_packaging_cue(query), "{query}");
        }
    }
}
