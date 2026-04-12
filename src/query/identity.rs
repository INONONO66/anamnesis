//! Identity prior computation for query initial activation.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! ## Equation
//! (9) I_i(a) = max_{k in Top3} [pi_tier(k) * sigma_ik]

use crate::graph::KnowledgeType;

/// Returns the tier weight (pi) for an identity node type.
///
/// L0 (IdentityCore) has the highest weight, L2 (IdentityState) the lowest.
pub fn pi_tier(kt: &KnowledgeType) -> f64 {
    match kt {
        KnowledgeType::IdentityCore => 1.0,
        KnowledgeType::IdentityLearned => 0.6,
        KnowledgeType::IdentityState => 0.3,
        _ => 0.0, // Non-identity types have no identity prior
    }
}

/// Computes the identity prior for a node given the agent's identity nodes.
///
/// Equation (9): I_i(a) = max_{k in Top3} [pi_tier(k) * sigma_ik]
///
/// Selects the top-3 identity nodes by salience, then returns the maximum
/// weighted similarity across those nodes.
///
/// # Parameters
/// - `node_embedding`: embedding of the node being scored
/// - `identity_nodes`: slice of (embedding, type, salience) for all agent identity nodes
/// - `similarity_fn`: cosine similarity function
///
/// Returns 0.0 if no identity nodes or no embeddings.
pub fn compute_identity_prior(
    node_embedding: &[f64],
    identity_nodes: &[(Vec<f64>, KnowledgeType, f64)],
    similarity_fn: fn(&[f64], &[f64]) -> f64,
) -> f64 {
    if identity_nodes.is_empty() || node_embedding.is_empty() {
        return 0.0;
    }

    let mut sorted: Vec<&(Vec<f64>, KnowledgeType, f64)> = identity_nodes
        .iter()
        .filter(|(emb, _, _)| !emb.is_empty())
        .collect();
    sorted.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    sorted.truncate(3);

    // Return max of pi_tier * similarity across top-3
    sorted
        .iter()
        .map(|(emb, kt, _salience)| {
            let sim = similarity_fn(node_embedding, emb);
            pi_tier(kt) * sim
        })
        .fold(0.0_f64, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mechanics::attraction::cosine_similarity;

    #[test]
    fn no_identity_nodes_returns_zero() {
        let result = compute_identity_prior(&[1.0, 0.0], &[], cosine_similarity);
        assert_eq!(result, 0.0);
    }

    #[test]
    fn empty_node_embedding_returns_zero() {
        let identity = vec![(vec![1.0, 0.0], KnowledgeType::IdentityCore, 0.9)];
        let result = compute_identity_prior(&[], &identity, cosine_similarity);
        assert_eq!(result, 0.0);
    }

    #[test]
    fn identity_core_identical_embedding_returns_one() {
        let identity = vec![(vec![1.0, 0.0], KnowledgeType::IdentityCore, 0.9)];
        let result = compute_identity_prior(&[1.0, 0.0], &identity, cosine_similarity);
        assert!((result - 1.0).abs() < 1e-10, "expected 1.0, got {result}");
    }

    #[test]
    fn identity_learned_has_lower_weight() {
        let identity_core = vec![(vec![1.0, 0.0], KnowledgeType::IdentityCore, 0.9)];
        let identity_learned = vec![(vec![1.0, 0.0], KnowledgeType::IdentityLearned, 0.9)];
        let node_emb = &[1.0, 0.0];

        let core_prior = compute_identity_prior(node_emb, &identity_core, cosine_similarity);
        let learned_prior = compute_identity_prior(node_emb, &identity_learned, cosine_similarity);

        assert!(
            core_prior > learned_prior,
            "IdentityCore ({core_prior}) should have higher prior than IdentityLearned ({learned_prior})"
        );
    }

    #[test]
    fn top_3_selection_by_salience() {
        // 5 identity nodes, only top-3 by salience should be used
        let identity = vec![
            (vec![0.0, 1.0], KnowledgeType::IdentityCore, 0.1), // low salience
            (vec![0.0, 1.0], KnowledgeType::IdentityCore, 0.2), // low salience
            (vec![1.0, 0.0], KnowledgeType::IdentityCore, 0.9), // high salience, matches node
            (vec![1.0, 0.0], KnowledgeType::IdentityCore, 0.8), // high salience, matches node
            (vec![1.0, 0.0], KnowledgeType::IdentityCore, 0.7), // high salience, matches node
        ];
        let node_emb = &[1.0, 0.0];
        let result = compute_identity_prior(node_emb, &identity, cosine_similarity);
        // Top-3 are the last 3 (salience 0.9, 0.8, 0.7), all match → result = 1.0
        assert!((result - 1.0).abs() < 1e-10);
    }

    #[test]
    fn empty_embedding_identity_nodes_filtered() {
        let identity = vec![
            (vec![], KnowledgeType::IdentityCore, 0.9),
            (vec![1.0, 0.0], KnowledgeType::IdentityLearned, 0.8),
        ];
        let result = compute_identity_prior(&[1.0, 0.0], &identity, cosine_similarity);
        assert!(
            (result - 0.6).abs() < 1e-10,
            "should use IdentityLearned (0.6 weight), got {result}"
        );
    }

    #[test]
    fn pi_tier_values() {
        assert_eq!(pi_tier(&KnowledgeType::IdentityCore), 1.0);
        assert_eq!(pi_tier(&KnowledgeType::IdentityLearned), 0.6);
        assert_eq!(pi_tier(&KnowledgeType::IdentityState), 0.3);
        assert_eq!(pi_tier(&KnowledgeType::Semantic), 0.0);
    }
}
