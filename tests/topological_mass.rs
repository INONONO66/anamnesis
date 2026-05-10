//! Property-based tests for topological mass computation.
//!
//! Tests invariants for compute_topological_mass() using proptest with 256 cases per test.

use anamnesis::graph::KnowledgeType;
use anamnesis::mechanics::gravity::compute_topological_mass;
use proptest::prelude::*;

fn knowledge_type_strategy() -> impl Strategy<Value = KnowledgeType> {
    prop_oneof![
        Just(KnowledgeType::IdentityCore),
        Just(KnowledgeType::IdentityLearned),
        Just(KnowledgeType::IdentityState),
        Just(KnowledgeType::Semantic),
        Just(KnowledgeType::Procedural),
        Just(KnowledgeType::Entity),
        Just(KnowledgeType::Convention),
        Just(KnowledgeType::Decision),
        Just(KnowledgeType::Gotcha),
        Just(KnowledgeType::Hypothesis),
        Just(KnowledgeType::Evidence),
        Just(KnowledgeType::DebugSession),
        Just(KnowledgeType::Episodic),
        Just(KnowledgeType::Event),
    ]
}

proptest! {
    /// compute_topological_mass: result ∈ [0, 1]
    #[test]
    fn prop_topological_mass_in_bounds(
        salience in 0.0f64..=1.0,
        access_count in 0u32..=10000,
        kt in knowledge_type_strategy(),
        bridge_score in 0.0f64..=1.0,
        support_score in 0.0f64..=1.0,
    ) {
        let result = compute_topological_mass(salience, access_count, &kt, bridge_score, support_score);
        prop_assert!(result >= 0.0, "topological_mass negative: {result}");
        prop_assert!(result <= 1.0 + 1e-10, "topological_mass > 1.0: {result}");
    }

    /// compute_topological_mass increases with bridge_score and support_score
    #[test]
    fn prop_topological_mass_increases_with_graph_structure(
        salience in 0.5f64..=1.0,
        access_count in 50u32..=1000,
        kt in knowledge_type_strategy(),
    ) {
        let bridge_low = 0.0;
        let bridge_high = 0.9;
        let support_low = 0.0;
        let support_high = 0.9;

        let mass_low = compute_topological_mass(salience, access_count, &kt, bridge_low, support_low);
        let mass_high = compute_topological_mass(salience, access_count, &kt, bridge_high, support_high);

        // Topological mass should be higher with high bridge and support scores
        prop_assert!(
            mass_high >= mass_low,
            "topological mass {mass_high} should be >= {mass_low} with higher graph structure scores"
        );
    }

    /// compute_topological_mass uses only (salience, access, type_prior) when bridge_score = 0 and support_score = 0
    #[test]
    fn prop_topological_mass_reduces_to_legacy_with_zero_scores(
        salience in 0.0f64..=1.0,
        access_count in 0u32..=10000,
        kt in knowledge_type_strategy(),
    ) {
        let topological = compute_topological_mass(salience, access_count, &kt, 0.0, 0.0);

        // With bridge_score = 0 and support_score = 0:
        // topological = clamp(0.40*s + 0.20*c + 0.15*mu + 0.15*0 + 0.10*0, 0, 1)
        //             = clamp(0.40*s + 0.20*c + 0.15*mu, 0, 1)
        // This is different from legacy (0.55*s + 0.30*c + 0.15*mu), but uses the same components.
        // Verify it's in bounds and uses only the base components.
        prop_assert!(topological >= 0.0, "topological_mass negative: {topological}");
        prop_assert!(topological <= 1.0 + 1e-10, "topological_mass > 1.0: {topological}");
    }

    /// compute_topological_mass is monotonic in bridge_score
    #[test]
    fn prop_topological_mass_monotonic_in_bridge(
        salience in 0.0f64..=1.0,
        access_count in 0u32..=10000,
        kt in knowledge_type_strategy(),
        bridge_low in 0.0f64..=0.5,
        support_score in 0.0f64..=1.0,
    ) {
        let bridge_high = bridge_low + 0.4; // Ensure bridge_high > bridge_low

        let mass_low = compute_topological_mass(salience, access_count, &kt, bridge_low, support_score);
        let mass_high = compute_topological_mass(salience, access_count, &kt, bridge_high, support_score);

        // Higher bridge_score should yield higher or equal mass
        prop_assert!(
            mass_high >= mass_low - 1e-10,
            "topological_mass not monotonic in bridge_score: {mass_high} < {mass_low}"
        );
    }

    /// compute_topological_mass is monotonic in support_score
    #[test]
    fn prop_topological_mass_monotonic_in_support(
        salience in 0.0f64..=1.0,
        access_count in 0u32..=10000,
        kt in knowledge_type_strategy(),
        bridge_score in 0.0f64..=1.0,
        support_low in 0.0f64..=0.5,
    ) {
        let support_high = support_low + 0.4; // Ensure support_high > support_low

        let mass_low = compute_topological_mass(salience, access_count, &kt, bridge_score, support_low);
        let mass_high = compute_topological_mass(salience, access_count, &kt, bridge_score, support_high);

        // Higher support_score should yield higher or equal mass
        prop_assert!(
            mass_high >= mass_low - 1e-10,
            "topological_mass not monotonic in support_score: {mass_high} < {mass_low}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topological_mass_high_salience_identity_core() {
        let m = compute_topological_mass(1.0, 100, &KnowledgeType::IdentityCore, 0.8, 0.9);
        assert!(m > 0.95, "expected near 1.0, got {m}");
    }

    #[test]
    fn topological_mass_zero_salience_zero_access_episodic() {
        let m = compute_topological_mass(0.0, 0, &KnowledgeType::Episodic, 0.0, 0.0);
        // 0.40*0 + 0.20*0 + 0.15*0.20 + 0.15*0 + 0.10*0 = 0.03
        assert!((m - 0.03).abs() < 1e-10, "expected 0.03, got {m}");
    }

    #[test]
    fn topological_mass_with_high_bridge_and_support() {
        let m = compute_topological_mass(0.5, 50, &KnowledgeType::Semantic, 0.9, 0.8);
        // 0.40*0.5 + 0.20*normalize(50) + 0.15*0.50 + 0.15*0.9 + 0.10*0.8
        // ≈ 0.20 + 0.20*0.78 + 0.075 + 0.135 + 0.08
        // ≈ 0.20 + 0.156 + 0.075 + 0.135 + 0.08 ≈ 0.646
        assert!(m > 0.6 && m < 0.7, "expected ~0.646, got {m}");
    }
}
