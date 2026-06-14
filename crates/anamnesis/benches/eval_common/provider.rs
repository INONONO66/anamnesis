use std::time::Duration;

use serde::{Deserialize, Serialize};

pub trait LlmProvider: Send + Sync {
    fn generate(&self, prompt: &str) -> Result<String, ProviderError>;
    fn name(&self) -> &str;
}

#[derive(Debug)]
pub enum ProviderError {
    Timeout,
    ConnectionFailed,
    InvalidResponse(String),
    Other(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Timeout => write!(f, "request timed out"),
            ProviderError::ConnectionFailed => write!(f, "connection failed"),
            ProviderError::InvalidResponse(msg) => write!(f, "invalid response: {msg}"),
            ProviderError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_secs: u64,
    pub max_retries: u32,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig {
            base_url: "http://localhost:11434".to_string(),
            model: "qwen2.5".to_string(),
            timeout_secs: 30,
            max_retries: 3,
        }
    }
}

pub struct OpenAiCompatibleProvider {
    pub client: reqwest::blocking::Client,
    pub config: ProviderConfig,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(OpenAiCompatibleProvider { client, config })
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        let base_url =
            std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
        let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "qwen2.5".to_string());
        let timeout_secs = std::env::var("LLM_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30u64);

        let config = ProviderConfig {
            base_url,
            model,
            timeout_secs,
            max_retries: 3,
        };
        Self::new(config)
    }
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn generate(&self, prompt: &str) -> Result<String, ProviderError> {
        let url = format!("{}/v1/chat/completions", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.0
        });

        let mut last_err = ProviderError::ConnectionFailed;
        for _ in 0..=self.config.max_retries {
            match self.client.post(&url).json(&body).send() {
                Ok(response) => {
                    let json: serde_json::Value = response
                        .json()
                        .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;
                    let content = json["choices"][0]["message"]["content"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::InvalidResponse("missing content".to_string())
                        })?;
                    return Ok(content.to_string());
                }
                Err(e) if e.is_timeout() => {
                    last_err = ProviderError::Timeout;
                }
                Err(_) => {
                    last_err = ProviderError::ConnectionFailed;
                }
            }
        }
        Err(last_err)
    }

    fn name(&self) -> &str {
        &self.config.model
    }
}

#[derive(Debug, Clone)]
pub struct MockProvider {
    pub response: String,
}

impl LlmProvider for MockProvider {
    fn generate(&self, _prompt: &str) -> Result<String, ProviderError> {
        Ok(self.response.clone())
    }

    fn name(&self) -> &str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_common_mock_provider_returns_expected_string() {
        let provider = MockProvider {
            response: "expected response".to_string(),
        };

        let response = provider.generate("prompt");

        assert!(matches!(response, Ok(ref value) if value == "expected response"));
    }

    #[test]
    fn eval_common_openai_provider_timeout_returns_error() {
        let config = ProviderConfig {
            base_url: "http://localhost:1".to_string(),
            model: "test".to_string(),
            timeout_secs: 1,
            max_retries: 0,
        };
        let provider = OpenAiCompatibleProvider::new(config).expect("should create");
        let result = provider.generate("test");
        assert!(result.is_err(), "should fail with connection error");
    }
}
