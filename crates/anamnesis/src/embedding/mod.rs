//! Embedding provider abstraction for the Anamnesis engine.
//!
//! The `EmbeddingProvider` trait defines the interface for embedding backends.
//! Implementations can use any embedding model (FastEmbed, OpenAI, local models, etc.).
//! The core engine works with f64 embeddings; this module provides utilities for
//! converting from f32 (common in embedding libraries) to f64.

#[cfg(feature = "embed")]
pub mod fastembed;

use crate::error::Error;

/// One document score returned by a [`RerankingProvider`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct RerankScore {
    /// Index into the document slice supplied to [`RerankingProvider::rerank`].
    pub index: usize,
    /// Finite relevance score. Larger scores rank first.
    pub score: f64,
}

impl RerankScore {
    /// Create a document-index score.
    pub fn new(index: usize, score: f64) -> Self {
        Self { index, score }
    }
}

/// Synchronous, model-agnostic document reranker used by production recall.
///
/// Implementations score a query against the exact evidence documents owned by
/// a [`PreparedRerank`](crate::memory::PreparedRerank). The canonical
/// [`Memory::search_reranked`](crate::memory::Memory::search_reranked) path and
/// external prepare/complete workflow share that receipt, so source binding,
/// score validation, evidence selection, packaging, and reinforcement remain
/// owned by [`Memory`](crate::memory::Memory). The older document-compilation
/// methods remain available as diagnostic, unbound surfaces.
pub trait RerankingProvider: Send + Sync {
    /// Rank `documents` for `query`.
    ///
    /// Providers may return any number of rows, but every row must reference a
    /// unique in-bounds document index and carry a finite score.
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<RerankScore>, Error>;

    /// Model name or stable provider identifier.
    fn model_name(&self) -> &str;
}

/// Trait for embedding text into vectors.
///
/// Implementations must be synchronous and thread-safe (`Send + Sync`).
/// The core engine uses f64 embeddings; providers typically return f32.
/// Use `embed_f64()` or `widen()` to convert.
pub trait EmbeddingProvider: Send + Sync {
    /// Embed multiple texts into vectors.
    ///
    /// # Arguments
    /// * `texts` - Slice of text strings to embed
    ///
    /// # Returns
    /// A vector of embedding vectors (one per input text), each as `Vec<f32>`.
    /// All embeddings must have the same dimension.
    ///
    /// # Errors
    /// Returns `Error::InvalidInput` if the provider cannot embed the texts.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error>;

    /// Get the embedding dimension for this provider.
    fn dimensions(&self) -> usize;

    /// Get the model name or identifier for this provider.
    fn model_name(&self) -> &str;

    /// Embed a single text string.
    ///
    /// Convenience method that wraps `embed()` for a single text.
    /// Returns an error if the provider returns an empty result.
    fn embed_single(&self, text: &str) -> Result<Vec<f32>, Error> {
        let results = self.embed(&[text])?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidInput("provider returned empty embedding".to_string()))
    }

    /// Embed text for use as a retrieval query.
    ///
    /// Providers for asymmetric embedding models can override this to apply
    /// query-specific formatting. The default preserves the existing symmetric
    /// provider behavior.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.embed_single(text)
    }

    /// Embed multiple retrieval queries while preserving query-specific formatting.
    ///
    /// The default delegates to [`EmbeddingProvider::embed_query`] so existing
    /// asymmetric providers remain correct without implementing a batch path.
    /// Providers that support efficient batching should override this method.
    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        texts.iter().map(|text| self.embed_query(text)).collect()
    }

    /// Embed text for storage as retrievable content.
    ///
    /// Providers for asymmetric embedding models can override this to apply
    /// passage-specific formatting. The default preserves the existing
    /// symmetric provider behavior.
    fn embed_passage(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.embed_single(text)
    }

    /// Embed multiple texts and convert to f64.
    ///
    /// Convenience method that calls `embed()` and converts each f32 vector to f64.
    fn embed_f64(&self, texts: &[&str]) -> Result<Vec<Vec<f64>>, Error> {
        let f32_embeddings = self.embed(texts)?;
        Ok(f32_embeddings.into_iter().map(|v| widen(&v)).collect())
    }
}

/// Convert a slice of f32 values to a vector of f64.
///
/// Used to convert embeddings from f32 (common in embedding libraries)
/// to f64 (used internally by the Anamnesis engine).
pub fn widen(v: &[f32]) -> Vec<f64> {
    v.iter().map(|&x| x as f64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestProvider;

    impl EmbeddingProvider for TestProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            Ok(texts.iter().map(|t| vec![t.len() as f32; 10]).collect())
        }

        fn dimensions(&self) -> usize {
            10
        }

        fn model_name(&self) -> &str {
            "test"
        }
    }

    #[test]
    fn widen_basic() {
        let f32_vec = vec![1.5f32, 2.5f32];
        let f64_vec = widen(&f32_vec);
        assert_eq!(f64_vec.len(), 2);
        assert_eq!(f64_vec[0], 1.5);
        assert_eq!(f64_vec[1], 2.5);
    }

    #[test]
    fn widen_empty() {
        let f32_vec: Vec<f32> = vec![];
        let f64_vec = widen(&f32_vec);
        assert!(f64_vec.is_empty());
    }

    #[test]
    fn embed_single_works() {
        let provider = TestProvider;
        let result = provider.embed_single("test");
        assert!(result.is_ok());
        let embedding = result.unwrap();
        assert_eq!(embedding.len(), 10);
        assert_eq!(embedding[0], 4.0);
    }

    #[test]
    fn embed_f64_works() {
        let provider = TestProvider;
        let result = provider.embed_f64(&["hello", "world"]);
        assert!(result.is_ok());
        let embeddings = result.unwrap();
        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].len(), 10);
        assert_eq!(embeddings[0][0], 5.0);
        assert_eq!(embeddings[1][0], 5.0);
    }
}
