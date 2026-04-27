#![cfg(feature = "embed")]

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::embedding::fastembed::FastEmbedProvider;

#[test]
fn provider_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FastEmbedProvider>();
}

// --- Integration tests below require model download (network I/O). ---
// Run with: cargo test --features embed --test fastembed_provider -- --ignored

#[test]
#[ignore]
fn default_model_dimensions() {
    let provider = FastEmbedProvider::new().expect("model init failed");
    assert_eq!(provider.dimensions(), 768);
}

#[test]
#[ignore]
fn default_model_name_contains_bge() {
    let provider = FastEmbedProvider::new().expect("model init failed");
    assert!(
        provider.model_name().contains("bge"),
        "expected model name containing 'bge', got: {}",
        provider.model_name()
    );
}

#[test]
#[ignore]
fn embed_single_returns_correct_dimension() {
    let provider = FastEmbedProvider::new().expect("model init failed");
    let embedding = provider.embed_single("hello world").expect("embed failed");
    assert_eq!(embedding.len(), 768);
}

#[test]
#[ignore]
fn embed_batch_returns_one_per_input() {
    let provider = FastEmbedProvider::new().expect("model init failed");
    let texts = &["first", "second", "third"];
    let embeddings = provider.embed(texts).expect("embed failed");
    assert_eq!(embeddings.len(), 3);
    for emb in &embeddings {
        assert_eq!(emb.len(), 768);
    }
}

#[test]
#[ignore]
fn embed_f64_widens_correctly() {
    let provider = FastEmbedProvider::new().expect("model init failed");
    let f64_embeddings = provider.embed_f64(&["test"]).expect("embed failed");
    assert_eq!(f64_embeddings.len(), 1);
    assert_eq!(f64_embeddings[0].len(), 768);
}
