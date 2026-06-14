//! Attraction mechanics — cosine similarity and edge creation scoring.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equations
//! - (2) Similarity: sigma_ij = max(0, cosine(e_i, e_j))
//! - (3) Attraction: A_ij = sigma_ij * tau_type(i, j)
//!
//! Attraction is a *candidate-selection* affinity only — similarity scaled by type
//! affinity. There is no mass or gravity boost: importance is emergent, not a
//! separate force (overview.md / conductance.md "Importance is emergent ... without
//! a separate gravity or mass force"). The strength a created edge carries comes
//! from the cold-start coupling seed (a calibrated log-LR prior), not from this
//! affinity; see `conductance.md` and `Engine::cold_start_conductance`.

use crate::graph::KnowledgeType;

/// Computes cosine similarity between two embedding vectors.
///
/// Equation (2): sigma_ij = max(0, cosine(e_i, e_j))
///
/// Returns 0.0 for:
/// - Empty slices
/// - Vectors of different lengths
/// - Zero-magnitude vectors (avoids division by zero)
/// - Negative cosine similarity (clamped to 0)
pub fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }

    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let mag_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }

    (dot / (mag_a * mag_b)).max(0.0)
}

/// Returns the type affinity multiplier (tau) for a pair of knowledge types.
///
/// Used in equation (3) to boost or dampen attraction based on node types.
pub fn tau_type(a: &KnowledgeType, b: &KnowledgeType) -> f64 {
    let a_is_identity = matches!(
        a,
        KnowledgeType::IdentityCore | KnowledgeType::IdentityLearned | KnowledgeType::IdentityState
    );
    let b_is_identity = matches!(
        b,
        KnowledgeType::IdentityCore | KnowledgeType::IdentityLearned | KnowledgeType::IdentityState
    );
    let a_is_knowledge =
        !a_is_identity && !matches!(a, KnowledgeType::Episodic | KnowledgeType::Event);
    let b_is_knowledge =
        !b_is_identity && !matches!(b, KnowledgeType::Episodic | KnowledgeType::Event);

    // Identity <-> Knowledge: 1.25
    if (a_is_identity && b_is_knowledge) || (b_is_identity && a_is_knowledge) {
        return 1.25;
    }

    // Entity <-> Entity (same type): 1.15
    if matches!(a, KnowledgeType::Entity) && matches!(b, KnowledgeType::Entity) {
        return 1.15;
    }

    // Episodic <-> Episodic: 0.70
    if matches!(a, KnowledgeType::Episodic) && matches!(b, KnowledgeType::Episodic) {
        return 0.70;
    }

    // Default
    1.00
}

/// Computes the attraction score between two nodes — the candidate-selection affinity.
///
/// Equation (3): A_ij = sigma_ij * tau_type(i, j)
///
/// - `similarity`: cosine similarity between embeddings [0, 1]
/// - `tau`: type affinity multiplier from `tau_type()`
///
/// There is no mass / gravity term: importance is emergent (overview.md /
/// conductance.md). This score only gates *which* candidates become edges; the
/// edge's actual strength is its cold-start coupling seed.
pub fn attraction_score(similarity: f64, tau: f64) -> f64 {
    similarity * tau
}

/// Returns the edge creation threshold for a pair of knowledge types.
///
/// Identity pairs use a lower threshold (0.65) to encourage identity-knowledge linking.
/// All other pairs use the standard threshold (0.72).
pub fn edge_threshold(a: &KnowledgeType, b: &KnowledgeType) -> f64 {
    let either_is_identity = matches!(
        a,
        KnowledgeType::IdentityCore | KnowledgeType::IdentityLearned | KnowledgeType::IdentityState
    ) || matches!(
        b,
        KnowledgeType::IdentityCore | KnowledgeType::IdentityLearned | KnowledgeType::IdentityState
    );

    if either_is_identity { 0.65 } else { 0.72 }
}

/// Returns true if an edge should be created between two nodes.
///
/// Uses type-specific thresholds: 0.65 for Identity pairs, 0.72 for all others.
pub fn should_create_edge(score: f64, a: &KnowledgeType, b: &KnowledgeType) -> bool {
    score >= edge_threshold(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Cosine similarity ────────────────────────────────────────────────────

    #[test]
    fn cosine_identical_vectors() {
        assert!((cosine_similarity(&[1.0, 0.0, 0.0], &[1.0, 0.0, 0.0]) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    #[test]
    fn cosine_empty_slice() {
        assert_eq!(cosine_similarity(&[], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_zero_magnitude() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_different_lengths() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_negative_clamped_to_zero() {
        // Opposite vectors would give -1.0 cosine, should be clamped to 0
        let result = cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]);
        assert_eq!(result, 0.0);
    }

    #[test]
    fn cosine_partial_similarity() {
        let a = &[1.0_f64, 0.0];
        let b = &[0.707_f64, 0.707];
        let result = cosine_similarity(a, b);
        assert!(result > 0.0 && result < 1.0);
        assert!((result - 0.707).abs() < 0.001);
    }

    // ── Tau type ─────────────────────────────────────────────────────────────

    #[test]
    fn tau_identity_knowledge() {
        assert_eq!(
            tau_type(&KnowledgeType::IdentityCore, &KnowledgeType::Semantic),
            1.25
        );
        assert_eq!(
            tau_type(&KnowledgeType::Semantic, &KnowledgeType::IdentityLearned),
            1.25
        );
    }

    #[test]
    fn tau_entity_entity() {
        assert_eq!(
            tau_type(&KnowledgeType::Entity, &KnowledgeType::Entity),
            1.15
        );
    }

    #[test]
    fn tau_episodic_episodic() {
        assert_eq!(
            tau_type(&KnowledgeType::Episodic, &KnowledgeType::Episodic),
            0.70
        );
    }

    #[test]
    fn tau_default() {
        assert_eq!(
            tau_type(&KnowledgeType::Semantic, &KnowledgeType::Semantic),
            1.00
        );
        assert_eq!(
            tau_type(&KnowledgeType::Decision, &KnowledgeType::Gotcha),
            1.00
        );
    }

    // ── Edge creation ────────────────────────────────────────────────────────

    #[test]
    fn threshold_identity_pair_lower() {
        assert_eq!(
            edge_threshold(&KnowledgeType::IdentityCore, &KnowledgeType::Semantic),
            0.65
        );
    }

    #[test]
    fn threshold_general_pair() {
        assert_eq!(
            edge_threshold(&KnowledgeType::Semantic, &KnowledgeType::Semantic),
            0.72
        );
    }

    #[test]
    fn should_create_edge_above_threshold() {
        assert!(should_create_edge(
            0.73,
            &KnowledgeType::Semantic,
            &KnowledgeType::Semantic
        ));
    }

    #[test]
    fn should_not_create_edge_below_threshold() {
        assert!(!should_create_edge(
            0.71,
            &KnowledgeType::Semantic,
            &KnowledgeType::Semantic
        ));
    }

    #[test]
    fn identity_threshold_allows_lower_score() {
        // 0.66 is below general threshold (0.72) but above identity threshold (0.65)
        assert!(should_create_edge(
            0.66,
            &KnowledgeType::IdentityCore,
            &KnowledgeType::Semantic
        ));
        assert!(!should_create_edge(
            0.66,
            &KnowledgeType::Semantic,
            &KnowledgeType::Semantic
        ));
    }

    // ── Attraction (similarity * type affinity, no mass) ──────────────────────

    #[test]
    fn attraction_is_similarity_times_tau() {
        // No mass term: A_ij = sigma_ij * tau.
        assert!((attraction_score(0.8, 1.25) - 1.0).abs() < 1e-10);
        assert!((attraction_score(0.5, 1.0) - 0.5).abs() < 1e-10);
    }

    // ── Property tests ───────────────────────────────────────────────────────

    proptest! {
        #[test]
        fn cosine_output_in_bounds(
            a0 in -1.0f64..=1.0,
            a1 in -1.0f64..=1.0,
            b0 in -1.0f64..=1.0,
            b1 in -1.0f64..=1.0,
        ) {
            let result = cosine_similarity(&[a0, a1], &[b0, b1]);
            prop_assert!(result >= 0.0, "cosine negative: {result}");
            prop_assert!(result <= 1.0 + 1e-10, "cosine > 1: {result}");
        }

        #[test]
        fn attraction_nonnegative(
            sim in 0.0f64..=1.0,
            tau in 0.0f64..=1.30,
        ) {
            let result = attraction_score(sim, tau);
            prop_assert!(result >= 0.0, "attraction negative: {result}");
        }
    }
}
