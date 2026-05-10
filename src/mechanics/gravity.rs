//! Gravity mechanics — node mass and gravity boost.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equations
//! - (1) Mass: m_i = clamp(0.55 * s_i + 0.30 * c_i + 0.15 * mu_i, 0, 1)
//! - (6) Gravity boost: 1 + 0.20 * m_i

use crate::graph::KnowledgeType;

/// Returns the mass prior (mu) for a knowledge type.
///
/// Higher prior = more inherent importance regardless of salience or access count.
pub fn mass_prior(kt: &KnowledgeType) -> f64 {
    match kt {
        KnowledgeType::IdentityCore => 1.00,
        KnowledgeType::IdentityLearned => 0.80,
        KnowledgeType::IdentityState => 0.50,
        KnowledgeType::Convention | KnowledgeType::Decision => 0.60,
        KnowledgeType::Semantic | KnowledgeType::Procedural => 0.50,
        KnowledgeType::Entity => 0.50,
        KnowledgeType::Gotcha => 0.50,
        KnowledgeType::Hypothesis | KnowledgeType::Evidence | KnowledgeType::DebugSession => 0.10,
        KnowledgeType::Episodic => 0.20,
        KnowledgeType::Event => 0.30,
        KnowledgeType::Custom(_) => 0.50,
    }
}

/// Normalizes an access count to [0, 1] using log scaling.
///
/// 0 accesses → 0.0, ~100 accesses → ~1.0.
pub fn normalize_access_count(count: u32) -> f64 {
    if count == 0 {
        return 0.0;
    }
    // ln(1 + count) / ln(101) — 100 accesses maps to ~1.0
    let normalized = (1.0 + count as f64).ln() / (101.0_f64).ln();
    normalized.clamp(0.0, 1.0)
}

/// Computes the mass of a node.
///
/// Equation (1): m_i = clamp(0.55 * s_i + 0.30 * c_i + 0.15 * mu_i, 0, 1)
///
/// - `salience`: current salience [0, 1]
/// - `access_count`: number of times the node has been accessed
/// - `kt`: knowledge type (determines mass prior)
pub fn compute_mass(salience: f64, access_count: u32, kt: &KnowledgeType) -> f64 {
    let c = normalize_access_count(access_count);
    let mu = mass_prior(kt);
    (0.55 * salience + 0.30 * c + 0.15 * mu).clamp(0.0, 1.0)
}

/// Computes the topological mass of a node incorporating graph structure.
///
/// Topological mass extends the legacy formula with bridge and support scores:
/// m_topo = clamp(0.40 * s_i + 0.20 * c_i + 0.15 * mu_i + 0.15 * bridge + 0.10 * support, 0, 1)
///
/// - `salience`: current salience [0, 1]
/// - `access_count`: number of times the node has been accessed
/// - `kt`: knowledge type (determines mass prior)
/// - `bridge_score`: graph bridging score [0, 1]
/// - `support_score`: incoming supportive edge fraction [0, 1]
pub fn compute_topological_mass(
    salience: f64,
    access_count: u32,
    kt: &KnowledgeType,
    bridge_score: f64,
    support_score: f64,
) -> f64 {
    let c = normalize_access_count(access_count);
    let mu = mass_prior(kt);
    (0.40 * salience + 0.20 * c + 0.15 * mu + 0.15 * bridge_score + 0.10 * support_score)
        .clamp(0.0, 1.0)
}

/// Computes the gravity boost multiplier for a node.
///
/// Equation (6): gravity_boost = 1 + 0.20 * m_i
///
/// Used during spreading activation to amplify activation through high-mass nodes.
pub fn gravity_boost(mass: f64) -> f64 {
    1.0 + 0.20 * mass
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn identity_core_has_highest_prior() {
        assert_eq!(mass_prior(&KnowledgeType::IdentityCore), 1.00);
    }

    #[test]
    fn episodic_has_lowest_prior() {
        assert_eq!(mass_prior(&KnowledgeType::Episodic), 0.20);
    }

    #[test]
    fn normalize_zero_access() {
        assert_eq!(normalize_access_count(0), 0.0);
    }

    #[test]
    fn normalize_hundred_access_near_one() {
        let result = normalize_access_count(100);
        assert!(result > 0.99, "expected ~1.0, got {result}");
    }

    #[test]
    fn mass_high_salience_identity_core() {
        let m = compute_mass(1.0, 100, &KnowledgeType::IdentityCore);
        assert!(m > 0.95, "expected near 1.0, got {m}");
    }

    #[test]
    fn mass_zero_salience_zero_access_episodic() {
        let m = compute_mass(0.0, 0, &KnowledgeType::Episodic);
        // 0.55*0 + 0.30*0 + 0.15*0.20 = 0.03
        assert!((m - 0.03).abs() < 1e-10, "expected 0.03, got {m}");
    }

    #[test]
    fn gravity_boost_at_zero_mass() {
        assert_eq!(gravity_boost(0.0), 1.0);
    }

    #[test]
    fn gravity_boost_at_max_mass() {
        assert!((gravity_boost(1.0) - 1.20).abs() < 1e-10);
    }

    proptest! {
        #[test]
        fn mass_output_in_bounds(
            s in 0.0f64..=1.0,
            count in 0u32..=1000,
        ) {
            let m = compute_mass(s, count, &KnowledgeType::Semantic);
            prop_assert!(m >= 0.0, "mass negative: {m}");
            prop_assert!(m <= 1.0 + 1e-10, "mass > 1: {m}");
        }

        #[test]
        fn gravity_boost_in_range(mass in 0.0f64..=1.0) {
            let boost = gravity_boost(mass);
            prop_assert!(boost >= 1.0, "boost < 1: {boost}");
            prop_assert!(boost <= 1.20 + 1e-10, "boost > 1.20: {boost}");
        }
    }
}
