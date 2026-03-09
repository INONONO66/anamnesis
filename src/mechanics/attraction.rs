//! Attraction mechanics: similarity-based clustering

/// Attraction scoring for similar/related nodes
pub struct Attraction;

impl Attraction {
    /// Compute similarity between two embeddings (cosine distance)
    /// Returns a score between 0.0 and 1.0
    pub fn similarity(embedding1: &[f64], embedding2: &[f64]) -> f64 {
        if embedding1.is_empty() || embedding2.is_empty() {
            return 0.0;
        }

        let dot_product: f64 = embedding1
            .iter()
            .zip(embedding2.iter())
            .map(|(a, b)| a * b)
            .sum();

        let norm1: f64 = embedding1.iter().map(|x| x * x).sum::<f64>().sqrt();
        let norm2: f64 = embedding2.iter().map(|x| x * x).sum::<f64>().sqrt();

        if norm1 == 0.0 || norm2 == 0.0 {
            return 0.0;
        }

        ((dot_product / (norm1 * norm2)) + 1.0) / 2.0
    }

    /// Identify merge candidates above a similarity threshold
    pub fn find_merge_candidates(
        embeddings: &[(u64, &[f64])],
        threshold: f64,
    ) -> Vec<(u64, u64, f64)> {
        let mut candidates = Vec::new();

        for i in 0..embeddings.len() {
            for j in (i + 1)..embeddings.len() {
                let (id1, emb1) = embeddings[i];
                let (id2, emb2) = embeddings[j];
                let similarity = Self::similarity(emb1, emb2);

                if similarity >= threshold {
                    candidates.push((id1, id2, similarity));
                }
            }
        }

        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_similarity_identical() {
        let emb = vec![1.0, 0.0, 0.0];
        let sim = Attraction::similarity(&emb, &emb);
        assert!((sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_similarity_orthogonal() {
        let emb1 = vec![1.0, 0.0];
        let emb2 = vec![0.0, 1.0];
        let sim = Attraction::similarity(&emb1, &emb2);
        assert!((sim - 0.5).abs() < 0.001);
    }
}
