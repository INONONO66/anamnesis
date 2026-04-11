//! Perception mechanics — observation gating.
//!
//! All functions are pure: no side effects, no storage access.
//!
//! The perception gate filters what enters the graph based on:
//! - Confidence: observation must meet minimum confidence threshold
//! - Budget: graph must not be at capacity
//! - Novelty: observation must be sufficiently different from existing knowledge

/// Checks whether an observation should be admitted to the graph.
///
/// Returns `Ok(())` if the observation passes all checks.
/// Returns `Err(reason)` with a human-readable reason if rejected.
///
/// # Parameters
/// - `confidence`: observation confidence [0, 1]
/// - `confidence_threshold`: minimum required confidence
/// - `current_node_count`: current number of nodes in the graph
/// - `max_nodes`: maximum allowed nodes
/// - `max_similarity`: highest cosine similarity to any existing node (0.0 if no embeddings)
/// - `novelty_threshold`: minimum required novelty (novelty = 1.0 - max_similarity)
pub fn gate_observation(
    confidence: f64,
    confidence_threshold: f64,
    current_node_count: usize,
    max_nodes: usize,
    max_similarity: f64,
    novelty_threshold: f64,
) -> Result<(), String> {
    if !confidence.is_finite() || !novelty_threshold.is_finite() || !max_similarity.is_finite() {
        return Err("non-finite input value".to_string());
    }

    if confidence < confidence_threshold {
        return Err(format!(
            "confidence {:.2} below threshold {:.2}",
            confidence, confidence_threshold
        ));
    }

    if current_node_count >= max_nodes {
        return Err(format!(
            "graph at capacity: {} >= {} nodes",
            current_node_count, max_nodes
        ));
    }

    let novelty = 1.0 - max_similarity;
    if novelty < novelty_threshold {
        return Err(format!(
            "observation too similar to existing knowledge: novelty {:.2} < threshold {:.2}",
            novelty, novelty_threshold
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn valid_observation_passes() {
        assert!(gate_observation(0.9, 0.5, 10, 100, 0.3, 0.3).is_ok());
    }

    #[test]
    fn low_confidence_rejected() {
        let result = gate_observation(0.3, 0.5, 10, 100, 0.0, 0.3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("confidence"));
    }

    #[test]
    fn over_budget_rejected() {
        let result = gate_observation(0.9, 0.5, 100, 100, 0.0, 0.3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("capacity"));
    }

    #[test]
    fn low_novelty_rejected() {
        // max_similarity = 0.8 → novelty = 0.2 < threshold 0.3
        let result = gate_observation(0.9, 0.5, 10, 100, 0.8, 0.3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("similar"));
    }

    #[test]
    fn exact_threshold_passes() {
        // confidence exactly at threshold
        assert!(gate_observation(0.5, 0.5, 10, 100, 0.0, 0.3).is_ok());
        // novelty exactly at threshold (max_sim = 0.7 → novelty = 0.3)
        assert!(gate_observation(0.9, 0.5, 10, 100, 0.7, 0.3).is_ok());
    }

    #[test]
    fn nan_confidence_rejected() {
        let result = gate_observation(f64::NAN, 0.5, 10, 100, 0.0, 0.3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("non-finite"));
    }

    #[test]
    fn nan_similarity_rejected() {
        let result = gate_observation(0.9, 0.5, 10, 100, f64::NAN, 0.3);
        assert!(result.is_err());
    }

    #[test]
    fn infinity_rejected() {
        let result = gate_observation(f64::INFINITY, 0.5, 10, 100, 0.0, 0.3);
        assert!(result.is_err());
    }

    #[test]
    fn no_existing_nodes_always_novel() {
        // max_similarity = 0.0 when no existing nodes → novelty = 1.0
        assert!(gate_observation(0.9, 0.5, 0, 100, 0.0, 0.3).is_ok());
    }

    proptest! {
        #[test]
        fn valid_observation_always_passes_with_extreme_budget(
            confidence in 0.5f64..=1.0,
            count in 0usize..=999,
            max_sim in 0.0f64..=0.69,
        ) {
            let result = gate_observation(confidence, 0.5, count, 1000, max_sim, 0.3);
            prop_assert!(result.is_ok(), "valid observation should pass: {:?}", result);
        }

        #[test]
        fn low_confidence_always_rejected(
            confidence in 0.0f64..0.5,
        ) {
            let result = gate_observation(confidence, 0.5, 0, 1000, 0.0, 0.3);
            prop_assert!(result.is_err(), "low confidence should be rejected");
        }

        #[test]
        fn over_budget_always_rejected(
            count in 100usize..=200,
        ) {
            let result = gate_observation(0.9, 0.5, count, 100, 0.0, 0.3);
            prop_assert!(result.is_err(), "over-budget should be rejected");
        }
    }
}
