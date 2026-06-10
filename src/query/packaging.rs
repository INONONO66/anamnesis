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

    let temporal_keywords = [
        "최근", "언제", "when", "recent", "latest", "history", "timeline", "before", "after", "ago",
    ];
    let q_lower = query_text.to_lowercase();
    if temporal_keywords
        .iter()
        .any(|keyword| q_lower.contains(keyword))
    {
        return PackagingMode::Timeline;
    }

    PackagingMode::Balanced
}
