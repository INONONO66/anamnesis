//! HTTP client for local OpenAI-style chat backends with Ollama-compatible routing.

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub struct LocalLlmClient {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl LocalLlmClient {
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
            .map_err(|e| format!("failed to connect to local LLM at {}: {e}", self.base_url))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "local LLM health check failed with status: {}",
                response.status()
            ))
        }
    }

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
            .map(str::to_string)
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
