//! Ollama HTTP client for chat and embeddings.
//!
//! Provides [`OllamaClient`] for chat completion and [`OllamaEmbedder`] implementing
//! the [`EmbeddingProvider`] trait for vector embeddings via a local Ollama instance.

use anamnesis::Error;
use anamnesis::embedding::EmbeddingProvider;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub struct OllamaClient {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaClient {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            model,
        }
    }

    pub async fn health_check(&self) -> Result<(), String> {
        let url = format!("{}/api/tags", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("failed to connect to Ollama at {}: {e}", self.base_url))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "Ollama health check failed with status: {}",
                response.status()
            ))
        }
    }

    /// Send a chat completion request and return the assistant's response.
    ///
    /// Posts to `/api/chat` with `stream: false` and extracts `message.content`
    /// from the response JSON.
    pub async fn chat(&self, messages: &[ChatMessage]) -> Result<String, String> {
        let url = format!("{}/api/chat", self.base_url);

        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                })
            })
            .collect();

        let body = serde_json::json!({
            "model": self.model,
            "messages": msgs,
            "stream": false,
        });

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("chat failed with status {status}: {text}"));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse chat response: {e}"))?;

        json["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "missing message.content in chat response".to_string())
    }

    pub async fn chat_with_system(
        &self,
        system: &str,
        messages: &[ChatMessage],
    ) -> Result<String, String> {
        let mut all_messages = Vec::with_capacity(messages.len() + 1);
        all_messages.push(ChatMessage {
            role: "system".to_string(),
            content: system.to_string(),
        });
        all_messages.extend_from_slice(messages);
        self.chat(&all_messages).await
    }
}

/// Embedding provider backed by a local Ollama instance.
///
/// Implements [`EmbeddingProvider`] by calling Ollama's `POST /api/embed` endpoint.
/// The synchronous trait methods bridge to async HTTP calls via
/// [`tokio::task::block_in_place`], which requires a multi-threaded tokio runtime.
///
/// Dimensions default to 768 and are updated after the first successful `embed()` call
/// to reflect the actual model output.
pub struct OllamaEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
    dimensions: AtomicUsize,
}

impl OllamaEmbedder {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            model,
            dimensions: AtomicUsize::new(768),
        }
    }

    async fn embed_async(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        let url = format!("{}/api/embed", self.base_url);

        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::InvalidInput(format!("embed request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Error::InvalidInput(format!(
                "embed failed with status {status}: {text}"
            )));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::InvalidInput(format!("failed to parse embed response: {e}")))?;

        let embeddings = json["embeddings"]
            .as_array()
            .ok_or_else(|| Error::InvalidInput("missing embeddings in response".to_string()))?;

        let result: Vec<Vec<f32>> = embeddings
            .iter()
            .map(|emb| match emb.as_array() {
                Some(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect(),
                None => Vec::new(),
            })
            .collect();

        if let Some(first) = result.first() {
            self.dimensions.store(first.len(), Ordering::Relaxed);
        }

        Ok(result)
    }
}

impl EmbeddingProvider for OllamaEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.embed_async(texts))
        })
    }

    fn dimensions(&self) -> usize {
        self.dimensions.load(Ordering::Relaxed)
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
