//! Perception mechanics: input gating

/// Perception gating for filtering what enters the graph
pub struct Perception;

impl Perception {
    /// Compute novelty score (how different from existing knowledge)
    /// Returns a score between 0.0 and 1.0
    pub fn novelty_score(
        observation_embedding: &[f64],
        existing_embeddings: &[&[f64]],
        _similarity_threshold: f64,
    ) -> f64 {
        if existing_embeddings.is_empty() {
            return 1.0; // Completely novel
        }

        let max_similarity = existing_embeddings
            .iter()
            .map(|emb| Self::cosine_similarity(observation_embedding, emb))
            .fold(0.0, f64::max);

        (1.0 - max_similarity).max(0.0)
    }

    /// Confidence filtering — check if observation meets confidence threshold
    pub fn passes_confidence(confidence: f64, threshold: f64) -> bool {
        confidence >= threshold
    }

    /// Budget constraint — check if we have room for new nodes
    pub fn passes_budget(current_nodes: usize, max_nodes: usize) -> bool {
        current_nodes < max_nodes
    }

    /// Combined gating decision
    pub fn should_ingest(
        observation_embedding: &[f64],
        existing_embeddings: &[&[f64]],
        confidence: f64,
        current_nodes: usize,
        novelty_threshold: f64,
        confidence_threshold: f64,
        max_nodes: usize,
    ) -> bool {
        let novelty = Self::novelty_score(observation_embedding, existing_embeddings, novelty_threshold);
        let passes_novelty = novelty >= novelty_threshold;
        let passes_conf = Self::passes_confidence(confidence, confidence_threshold);
        let passes_bud = Self::passes_budget(current_nodes, max_nodes);

        passes_novelty && passes_conf && passes_bud
    }

    fn cosine_similarity(emb1: &[f64], emb2: &[f64]) -> f64 {
        if emb1.is_empty() || emb2.is_empty() {
            return 0.0;
        }

        let dot_product: f64 = emb1.iter().zip(emb2.iter()).map(|(a, b)| a * b).sum();
        let norm1: f64 = emb1.iter().map(|x| x * x).sum::<f64>().sqrt();
        let norm2: f64 = emb2.iter().map(|x| x * x).sum::<f64>().sqrt();

        if norm1 == 0.0 || norm2 == 0.0 {
            return 0.0;
        }

        ((dot_product / (norm1 * norm2)) + 1.0) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_novelty_empty() {
        let obs = vec![1.0, 0.0];
        let novelty = Perception::novelty_score(&obs, &[], 0.5);
        assert!((novelty - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_confidence_filtering() {
        assert!(Perception::passes_confidence(0.8, 0.5));
        assert!(!Perception::passes_confidence(0.3, 0.5));
    }

    #[test]
    fn test_budget_constraint() {
        assert!(Perception::passes_budget(5, 10));
        assert!(!Perception::passes_budget(10, 10));
    }
}
