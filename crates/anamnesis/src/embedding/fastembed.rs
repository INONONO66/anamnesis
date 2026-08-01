//! FastEmbed-based embedding provider (requires `embed` feature).
//!
//! Wraps the [`fastembed`] crate to implement [`EmbeddingProvider`].
//! Model initialization downloads weights on first use; see
//! [`FastEmbedProvider::new`] for details.

use std::sync::Mutex;
use std::{env, path::PathBuf};

pub use fastembed::EmbeddingModel;
use fastembed::{InitOptions, RerankInitOptions, RerankerModel, TextEmbedding, TextRerank};

use crate::embedding::{EmbeddingProvider, RerankScore, RerankingProvider};
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
    uses_e5_query_passage_protocol: bool,
}

/// Local FastEmbed cross-encoder used by the production reranked-recall path.
///
/// The production default is `BAAI/bge-reranker-base`, selected for the
/// latency-sensitive product-path LoCoMo profile. Model weights are loaded once
/// and shared safely across recall calls.
pub struct FastEmbedReranker {
    model: Mutex<TextRerank>,
    name: String,
    batch_size: usize,
}

/// Default local reranker for latency-sensitive production recall.
pub const DEFAULT_RERANKER_MODEL: &str = "BAAI/bge-reranker-base";

const E5_QUERY_PASSAGE_PROTOCOL: &str = "query-passage-v1";

#[derive(Clone, Copy)]
enum PrefixKind {
    Query,
    Passage,
}

fn is_multilingual_e5(model_code: &str) -> bool {
    model_code.to_ascii_lowercase().contains("multilingual-e5")
}

fn legacy_identity_already_used_e5_protocol(model_code: &str) -> bool {
    model_code
        .to_ascii_lowercase()
        .starts_with("intfloat/multilingual-e5")
}

fn embedding_space_name(model_code: &str) -> String {
    if is_multilingual_e5(model_code) && !legacy_identity_already_used_e5_protocol(model_code) {
        format!("{model_code}+{E5_QUERY_PASSAGE_PROTOCOL}")
    } else {
        model_code.to_string()
    }
}

fn e5_prefix(uses_e5_query_passage_protocol: bool, kind: PrefixKind, text: &str) -> String {
    if uses_e5_query_passage_protocol {
        match kind {
            PrefixKind::Query => format!("query: {text}"),
            PrefixKind::Passage => format!("passage: {text}"),
        }
    } else {
        text.to_string()
    }
}

pub fn embed_model_from_name(name: &str) -> Result<EmbeddingModel, Error> {
    let normalized = name.trim().to_ascii_lowercase();
    let model_name = normalized
        .strip_suffix(&format!("+{E5_QUERY_PASSAGE_PROTOCOL}"))
        .unwrap_or(&normalized);
    match model_name {
        "multilingual-e5-small"
        | "intfloat/multilingual-e5-small"
        | "qdrant/multilingual-e5-small-onnx" => Ok(EmbeddingModel::MultilingualE5Small),
        "multilingual-e5-base"
        | "intfloat/multilingual-e5-base"
        | "qdrant/multilingual-e5-base-onnx" => Ok(EmbeddingModel::MultilingualE5Base),
        "multilingual-e5-large"
        | "intfloat/multilingual-e5-large"
        | "qdrant/multilingual-e5-large-onnx" => Ok(EmbeddingModel::MultilingualE5Large),
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
        let uses_e5_query_passage_protocol = is_multilingual_e5(&info.model_code);
        // Query/passage formatting is part of an embedding space, not merely
        // tokenization detail. Version model codes that the legacy detector
        // missed so a previously stored raw E5 space is migrated before it is
        // queried. Preserve intfloat E5 identities: those already received the
        // same protocol and do not need a no-op full re-embedding.
        let name = embedding_space_name(&info.model_code);
        let embedding = TextEmbedding::try_new(InitOptions::new(model))
            .map_err(|e| Error::InvalidInput(format!("model init failed: {e}")))?;

        Ok(Self {
            model: Mutex::new(embedding),
            dim,
            name,
            uses_e5_query_passage_protocol,
        })
    }
}

impl FastEmbedReranker {
    /// Create the default local quality reranker.
    pub fn new() -> Result<Self, Error> {
        Self::with_model_name(DEFAULT_RERANKER_MODEL)
    }

    /// Create a local reranker from a FastEmbed model identifier.
    pub fn with_model_name(model_name: &str) -> Result<Self, Error> {
        let normalized = model_name.trim();
        let model = normalized.parse::<RerankerModel>().map_err(|error| {
            Error::InvalidInput(format!(
                "unsupported reranker model {normalized:?}: {error}"
            ))
        })?;
        let cache_dir = env::var_os("FASTEMBED_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".fastembed_cache"));
        let reranker = TextRerank::try_new(RerankInitOptions::new(model).with_cache_dir(cache_dir))
            .map_err(|error| Error::InvalidInput(format!("reranker init failed: {error}")))?;
        Ok(Self {
            model: Mutex::new(reranker),
            name: normalized.to_owned(),
            batch_size: 32,
        })
    }

    /// Override the inference batch size.
    pub fn with_batch_size(mut self, batch_size: usize) -> Result<Self, Error> {
        if batch_size == 0 {
            return Err(Error::InvalidInput(
                "reranker batch size must be greater than zero".to_owned(),
            ));
        }
        self.batch_size = batch_size;
        Ok(self)
    }
}

impl RerankingProvider for FastEmbedReranker {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<RerankScore>, Error> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let model = self
            .model
            .lock()
            .map_err(|error| Error::InvalidInput(format!("reranker mutex poisoned: {error}")))?;
        model
            .rerank(
                query.to_owned(),
                documents.to_vec(),
                false,
                Some(self.batch_size),
            )
            .map(|rows| {
                rows.into_iter()
                    .map(|row| RerankScore {
                        index: row.index,
                        score: f64::from(row.score),
                    })
                    .collect()
            })
            .map_err(|error| Error::InvalidInput(format!("reranking failed: {error}")))
    }

    fn model_name(&self) -> &str {
        &self.name
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
        self.embed_single(&e5_prefix(
            self.uses_e5_query_passage_protocol,
            PrefixKind::Query,
            text,
        ))
    }

    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prefixed: Vec<_> = texts
            .iter()
            .map(|text| e5_prefix(self.uses_e5_query_passage_protocol, PrefixKind::Query, text))
            .collect();
        let borrowed: Vec<_> = prefixed.iter().map(String::as_str).collect();
        self.embed(&borrowed)
    }

    fn embed_passage(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.embed_single(&e5_prefix(
            self.uses_e5_query_passage_protocol,
            PrefixKind::Passage,
            text,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e5_prefix_applies_query_and_passage_roles() {
        assert_eq!(e5_prefix(true, PrefixKind::Query, "안녕"), "query: 안녕");
        assert_eq!(
            e5_prefix(true, PrefixKind::Passage, "안녕"),
            "passage: 안녕"
        );
        assert_eq!(e5_prefix(false, PrefixKind::Query, "hi"), "hi");
    }

    #[test]
    fn qdrant_e5_model_codes_get_versioned_embedding_space_names() {
        let raw = "Qdrant/multilingual-e5-large-onnx";
        assert!(is_multilingual_e5(raw));
        assert_eq!(
            embedding_space_name(raw),
            "Qdrant/multilingual-e5-large-onnx+query-passage-v1"
        );
        assert_eq!(
            embedding_space_name("intfloat/multilingual-e5-small"),
            "intfloat/multilingual-e5-small"
        );
        assert_eq!(
            embedding_space_name("BAAI/bge-base-en-v1.5"),
            "BAAI/bge-base-en-v1.5"
        );
    }

    #[test]
    fn versioned_qdrant_e5_identity_resolves_to_the_same_weights() {
        assert!(matches!(
            embed_model_from_name("Qdrant/multilingual-e5-large-onnx+query-passage-v1"),
            Ok(EmbeddingModel::MultilingualE5Large)
        ));
    }

    #[test]
    fn production_reranker_default_is_the_fast_profile() {
        assert_eq!(DEFAULT_RERANKER_MODEL, "BAAI/bge-reranker-base");
        assert!(DEFAULT_RERANKER_MODEL.parse::<RerankerModel>().is_ok());
    }
}
