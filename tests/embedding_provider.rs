//! Tests for the EmbeddingProvider trait.
//!
//! Verifies that custom embedding providers can be implemented,
//! and that utility functions like embed_single and widen work correctly.

use anamnesis::Error;
use anamnesis::embedding::EmbeddingProvider;

/// A simple mock embedding provider for testing.
struct MockEmbeddingProvider {
    dimension: usize,
}

impl MockEmbeddingProvider {
    fn new(dimension: usize) -> Self {
        Self { dimension }
    }
}

impl EmbeddingProvider for MockEmbeddingProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts
            .iter()
            .map(|text| vec![text.len() as f32; self.dimension])
            .collect())
    }

    fn dimensions(&self) -> usize {
        self.dimension
    }

    fn model_name(&self) -> &str {
        "mock-provider"
    }
}

#[test]
fn custom_provider_compiles() {
    let provider = MockEmbeddingProvider::new(384);
    assert_eq!(provider.dimension, 384);
}

#[test]
fn embed_single_success() {
    let provider = MockEmbeddingProvider::new(384);
    let result = provider.embed_single("hello world");
    assert!(result.is_ok());
    let embedding = result.unwrap();
    assert_eq!(embedding.len(), 384);
    assert_eq!(embedding[0], 11.0);
}

#[test]
fn embed_single_empty_text() {
    let provider = MockEmbeddingProvider::new(384);
    let result = provider.embed_single("");
    assert!(result.is_ok());
    let embedding = result.unwrap();
    assert_eq!(embedding.len(), 384);
    assert_eq!(embedding[0], 0.0);
}

#[test]
fn embed_f64_conversion() {
    let provider = MockEmbeddingProvider::new(384);
    let result = provider.embed_f64(&["test"]);
    assert!(result.is_ok());
    let embeddings = result.unwrap();
    assert_eq!(embeddings.len(), 1);
    assert_eq!(embeddings[0].len(), 384);
    assert_eq!(embeddings[0][0], 4.0);
}

#[test]
fn provider_dimensions() {
    let provider = MockEmbeddingProvider::new(384);
    assert_eq!(provider.dimensions(), 384);
}

#[test]
fn provider_model_name() {
    let provider = MockEmbeddingProvider::new(384);
    assert_eq!(provider.model_name(), "mock-provider");
}

#[test]
fn widen_f32_to_f64() {
    let f32_vec = vec![1.5f32, 2.5f32, 3.5f32];
    let f64_vec = anamnesis::embedding::widen(&f32_vec);
    assert_eq!(f64_vec.len(), 3);
    assert_eq!(f64_vec[0], 1.5);
    assert_eq!(f64_vec[1], 2.5);
    assert_eq!(f64_vec[2], 3.5);
}

#[test]
fn widen_empty_vector() {
    let f32_vec: Vec<f32> = vec![];
    let f64_vec = anamnesis::embedding::widen(&f32_vec);
    assert_eq!(f64_vec.len(), 0);
}

#[test]
fn widen_preserves_precision() {
    let f32_val = 0.123_456_79f32;
    let f32_vec = vec![f32_val];
    let f64_vec = anamnesis::embedding::widen(&f32_vec);
    assert_eq!(f64_vec[0], f32_val as f64);
}
