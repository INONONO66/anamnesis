//! FastEmbed-based embedding provider (requires `embed` feature).
//!
//! Wraps the [`fastembed`] crate to implement [`EmbeddingProvider`].
//! Model initialization downloads weights on first use; see
//! [`FastEmbedProvider::new`] for details.

use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::embedding::EmbeddingProvider;
use crate::error::Error;

/// Embedding provider backed by [FastEmbed](https://crates.io/crates/fastembed).
///
/// Wraps `TextEmbedding` in a [`Mutex`] to satisfy the `&self`
/// [`EmbeddingProvider`] contract, since FastEmbed's `embed()` requires
/// `&mut self`.
///
/// # Default model
///
/// [`FastEmbedProvider::new`] uses **BAAI/bge-base-en-v1.5** (768 dimensions).
///
/// # Network I/O
///
/// The constructor downloads model weights on first use (~100-500 MB depending
/// on the model). Ensure network access is available, or pre-populate the
/// cache directory.
pub struct FastEmbedProvider {
    model: Mutex<TextEmbedding>,
    dim: usize,
    name: String,
}

impl FastEmbedProvider {
    /// Create a provider with the default model (BAAI/bge-base-en-v1.5, 768-d).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if model initialization fails
    /// (e.g. network error, invalid cache).
    pub fn new() -> Result<Self, Error> {
        Self::with_model(EmbeddingModel::BGEBaseENV15)
    }

    /// Create a provider with a specific FastEmbed model variant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if model info lookup or
    /// initialization fails.
    pub fn with_model(model: EmbeddingModel) -> Result<Self, Error> {
        let info = TextEmbedding::get_model_info(&model)
            .map_err(|e| Error::InvalidInput(format!("model info lookup failed: {e}")))?;
        let dim = info.dim;
        let name = info.model_code.clone();

        let embedding = TextEmbedding::try_new(InitOptions::new(model))
            .map_err(|e| Error::InvalidInput(format!("model init failed: {e}")))?;

        Ok(Self {
            model: Mutex::new(embedding),
            dim,
            name,
        })
    }
}

impl EmbeddingProvider for FastEmbedProvider {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        let model = self
            .model
            .lock()
            .map_err(|e| Error::InvalidInput(format!("mutex poisoned: {e}")))?;

        let owned: Vec<String> = texts.iter().map(|s| (*s).to_string()).collect();
        model
            .embed(owned, None)
            .map_err(|e| Error::InvalidInput(format!("embedding failed: {e}")))
    }

    fn dimensions(&self) -> usize {
        self.dim
    }

    fn model_name(&self) -> &str {
        &self.name
    }
}
