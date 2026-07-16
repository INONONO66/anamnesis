//! FastEmbed-based embedding provider (requires `embed` feature).
//!
//! Wraps the [`fastembed`] crate to implement [`EmbeddingProvider`].
//! Model initialization downloads weights on first use; see
//! [`FastEmbedProvider::new`] for details.

use std::sync::Mutex;

pub use fastembed::EmbeddingModel;
use fastembed::{InitOptions, TextEmbedding};

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

#[derive(Clone, Copy)]
enum PrefixKind {
    Query,
    Passage,
}

fn e5_prefix(model_code: &str, kind: PrefixKind, text: &str) -> String {
    if model_code.starts_with("intfloat/multilingual-e5") {
        match kind {
            PrefixKind::Query => format!("query: {text}"),
            PrefixKind::Passage => format!("passage: {text}"),
        }
    } else {
        text.to_string()
    }
}

pub fn embed_model_from_name(name: &str) -> Result<EmbeddingModel, Error> {
    match name.trim().to_ascii_lowercase().as_str() {
        "multilingual-e5-small" | "intfloat/multilingual-e5-small" => {
            Ok(EmbeddingModel::MultilingualE5Small)
        }
        "multilingual-e5-base" | "intfloat/multilingual-e5-base" => {
            Ok(EmbeddingModel::MultilingualE5Base)
        }
        "multilingual-e5-large" | "intfloat/multilingual-e5-large" => {
            Ok(EmbeddingModel::MultilingualE5Large)
        }
        "bge-base-en-v1.5" | "baai/bge-base-en-v1.5" => Ok(EmbeddingModel::BGEBaseENV15),
        other => Err(Error::InvalidInput(format!(
            "unsupported embedding model {other:?}; supported: multilingual-e5-small, \
             multilingual-e5-base, multilingual-e5-large, bge-base-en-v1.5"
        ))),
    }
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

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.embed_single(&e5_prefix(&self.name, PrefixKind::Query, text))
    }

    fn embed_passage(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.embed_single(&e5_prefix(&self.name, PrefixKind::Passage, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e5_prefix_applies_only_to_e5_models() {
        assert_eq!(
            e5_prefix("intfloat/multilingual-e5-small", PrefixKind::Query, "안녕"),
            "query: 안녕"
        );
        assert_eq!(
            e5_prefix(
                "intfloat/multilingual-e5-small",
                PrefixKind::Passage,
                "안녕"
            ),
            "passage: 안녕"
        );
        assert_eq!(
            e5_prefix("BAAI/bge-base-en-v1.5", PrefixKind::Query, "hi"),
            "hi"
        );
    }
}
