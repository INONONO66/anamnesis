use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub trait LlmProvider: Send + Sync {
    fn generate(&self, prompt: &str) -> Result<String, ProviderError>;
    fn generate_with_usage(&self, prompt: &str) -> Result<ProviderGeneration, ProviderError> {
        self.generate(prompt).map(|content| ProviderGeneration {
            content,
            prompt_tokens: None,
            completion_tokens: None,
            done_reason: None,
        })
    }
    fn generate_with_usage_format(
        &self,
        prompt: &str,
        _output_format: ProviderOutputFormat,
    ) -> Result<ProviderGeneration, ProviderError> {
        self.generate_with_usage(prompt)
    }
    fn generate_chat(&self, _prompt: &ProviderChatPrompt) -> Result<String, ProviderError> {
        Err(ProviderError::Unsupported(
            "provider does not support role-separated chat prompts".to_owned(),
        ))
    }
    fn generate_chat_with_usage(
        &self,
        _prompt: &ProviderChatPrompt,
    ) -> Result<ProviderGeneration, ProviderError> {
        Err(ProviderError::Unsupported(
            "provider does not support role-separated chat prompts".to_owned(),
        ))
    }
    fn generate_chat_with_usage_format(
        &self,
        _prompt: &ProviderChatPrompt,
        _output_format: ProviderOutputFormat,
    ) -> Result<ProviderGeneration, ProviderError> {
        Err(ProviderError::Unsupported(
            "provider does not support role-separated chat prompts".to_owned(),
        ))
    }
    fn name(&self) -> &str;
}

/// One role-separated chat request.
///
/// `system` is reserved for trusted, consumer-compiled instructions. Query,
/// evidence, model-produced candidates, and prior model responses belong in
/// `user`. Providers must preserve these two roles and must never flatten this
/// pair into one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChatPrompt {
    /// Trusted instructions sent with the `system` role.
    pub system: String,
    /// Untrusted JSON data sent with the `user` role.
    pub user: String,
}

impl ProviderChatPrompt {
    /// Create one exact system/user message pair.
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            user: user.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderGeneration {
    pub content: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub done_reason: Option<String>,
}

#[derive(Debug)]
pub enum ProviderError {
    Timeout,
    ConnectionFailed,
    InvalidResponse(String),
    Unsupported(String),
    Other(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Timeout => write!(f, "request timed out"),
            ProviderError::ConnectionFailed => write!(f, "connection failed"),
            ProviderError::InvalidResponse(msg) => write!(f, "invalid response: {msg}"),
            ProviderError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
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
    pub max_output_tokens: Option<u64>,
    /// Sampling temperature sent to the loopback chat server.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Nucleus-sampling threshold sent to the loopback chat server.
    #[serde(default)]
    pub top_p: Option<f64>,
    /// Top-k sampling cutoff for compatible local providers.
    #[serde(default)]
    pub top_k: Option<u64>,
    /// Presence penalty sent to the loopback chat server.
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    /// Deterministic seed when supported by the provider.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Optional Qwen-compatible chat-template thinking control.
    ///
    /// Leave this unset when the local server does not accept
    /// `chat_template_kwargs`.
    #[serde(default)]
    pub chat_template_enable_thinking: Option<bool>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig {
            base_url: "http://127.0.0.1:11434".to_string(),
            model: "qwen3.6:35b-a3b".to_string(),
            timeout_secs: 30,
            max_retries: 3,
            max_output_tokens: None,
            temperature: Some(0.0),
            top_p: None,
            top_k: None,
            presence_penalty: None,
            seed: None,
            chat_template_enable_thinking: None,
        }
    }
}

/// OpenAI-compatible chat transport restricted to the local machine.
///
/// The constructor accepts only literal loopback IP endpoints, disables
/// redirects, bypasses proxy settings, and never reads or sends credentials.
pub struct LoopbackChatProvider {
    client: reqwest::blocking::Client,
    config: ProviderConfig,
}

impl LoopbackChatProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        if !is_loopback_base_url(&config.base_url) {
            return Err(ProviderError::Other(
                "local chat base URL must use a literal loopback IP address".to_owned(),
            ));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(Self { client, config })
    }

    fn request_body(&self, prompt: &str, output_format: ProviderOutputFormat) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": [{"role": "user", "content": prompt}]
        });
        if let Some(temperature) = self.config.temperature {
            body["temperature"] = serde_json::Value::from(temperature);
        }
        if let Some(top_p) = self.config.top_p {
            body["top_p"] = serde_json::Value::from(top_p);
        }
        if let Some(top_k) = self.config.top_k {
            body["top_k"] = serde_json::Value::from(top_k);
        }
        if let Some(presence_penalty) = self.config.presence_penalty {
            body["presence_penalty"] = serde_json::Value::from(presence_penalty);
        }
        if let Some(seed) = self.config.seed {
            body["seed"] = serde_json::Value::from(seed);
        }
        if let Some(max_output_tokens) = self.config.max_output_tokens {
            body["max_tokens"] = serde_json::Value::from(max_output_tokens);
        }
        if let Some(enable_thinking) = self.config.chat_template_enable_thinking {
            body["chat_template_kwargs"] = serde_json::json!({
                "enable_thinking": enable_thinking
            });
        }
        if output_format == ProviderOutputFormat::Json {
            body["response_format"] = serde_json::json!({"type": "json_object"});
        }
        body
    }

    fn chat_request_body(
        &self,
        prompt: &ProviderChatPrompt,
        output_format: ProviderOutputFormat,
    ) -> serde_json::Value {
        let mut body = self.request_body("", output_format);
        body["messages"] = serde_json::json!([
            {"role": "system", "content": prompt.system.as_str()},
            {"role": "user", "content": prompt.user.as_str()}
        ]);
        body
    }

    fn request_with_body(
        &self,
        body: serde_json::Value,
    ) -> Result<ProviderGeneration, ProviderError> {
        let base_url = self.config.base_url.trim_end_matches('/');
        let url = if base_url.ends_with("/v1") {
            format!("{base_url}/chat/completions")
        } else {
            format!("{base_url}/v1/chat/completions")
        };
        let mut last_err = ProviderError::ConnectionFailed;
        for _ in 0..=self.config.max_retries {
            match self.client.post(&url).json(&body).send() {
                Ok(response) => {
                    let status = response.status();
                    if !status.is_success() {
                        let detail = response
                            .text()
                            .unwrap_or_else(|_| "response body unavailable".to_string());
                        return Err(ProviderError::Other(format!(
                            "HTTP {status} from chat completions: {detail}"
                        )));
                    }
                    let json: serde_json::Value = response
                        .json()
                        .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;
                    let content = json["choices"][0]["message"]["content"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::InvalidResponse("missing content".to_string())
                        })?
                        .to_string();
                    return Ok(ProviderGeneration {
                        content,
                        prompt_tokens: json["usage"]["prompt_tokens"].as_u64(),
                        completion_tokens: json["usage"]["completion_tokens"].as_u64(),
                        done_reason: json["choices"][0]["finish_reason"]
                            .as_str()
                            .map(ToOwned::to_owned),
                    });
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

    fn request(
        &self,
        prompt: &str,
        output_format: ProviderOutputFormat,
    ) -> Result<ProviderGeneration, ProviderError> {
        self.request_with_body(self.request_body(prompt, output_format))
    }

    fn request_chat(
        &self,
        prompt: &ProviderChatPrompt,
        output_format: ProviderOutputFormat,
    ) -> Result<ProviderGeneration, ProviderError> {
        self.request_with_body(self.chat_request_body(prompt, output_format))
    }
}

pub fn is_loopback_base_url(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).ok().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some_and(|host| {
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
            })
    })
}

impl LlmProvider for LoopbackChatProvider {
    fn generate(&self, prompt: &str) -> Result<String, ProviderError> {
        self.request(prompt, ProviderOutputFormat::Text)
            .map(|generation| generation.content)
    }

    fn generate_with_usage(&self, prompt: &str) -> Result<ProviderGeneration, ProviderError> {
        self.request(prompt, ProviderOutputFormat::Text)
    }

    fn generate_with_usage_format(
        &self,
        prompt: &str,
        output_format: ProviderOutputFormat,
    ) -> Result<ProviderGeneration, ProviderError> {
        self.request(prompt, output_format)
    }

    fn generate_chat(&self, prompt: &ProviderChatPrompt) -> Result<String, ProviderError> {
        self.request_chat(prompt, ProviderOutputFormat::Text)
            .map(|generation| generation.content)
    }

    fn generate_chat_with_usage(
        &self,
        prompt: &ProviderChatPrompt,
    ) -> Result<ProviderGeneration, ProviderError> {
        self.request_chat(prompt, ProviderOutputFormat::Text)
    }

    fn generate_chat_with_usage_format(
        &self,
        prompt: &ProviderChatPrompt,
        output_format: ProviderOutputFormat,
    ) -> Result<ProviderGeneration, ProviderError> {
        self.request_chat(prompt, output_format)
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

    fn generate_chat(&self, _prompt: &ProviderChatPrompt) -> Result<String, ProviderError> {
        Ok(self.response.clone())
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
    fn eval_common_loopback_provider_timeout_returns_error() {
        let config = ProviderConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            model: "test".to_string(),
            timeout_secs: 1,
            max_retries: 0,
            max_output_tokens: None,
            temperature: Some(0.0),
            top_p: None,
            top_k: None,
            presence_penalty: None,
            seed: None,
            chat_template_enable_thinking: None,
        };
        let provider = LoopbackChatProvider::new(config).expect("should create");
        let result = provider.generate("test");
        assert!(result.is_err(), "should fail with connection error");
    }

    #[test]
    fn loopback_provider_rejects_remote_hosts() {
        let config = ProviderConfig {
            base_url: "https://example.com".to_owned(),
            ..ProviderConfig::default()
        };

        let result = LoopbackChatProvider::new(config);

        assert!(
            matches!(result, Err(ProviderError::Other(message)) if message.contains("loopback"))
        );
    }

    #[test]
    fn loopback_detection_does_not_accept_remote_or_lookalike_hosts() {
        assert!(is_loopback_base_url("http://127.0.0.1:8000"));
        assert!(is_loopback_base_url("http://[::1]:8000/v1"));
        assert!(!is_loopback_base_url("http://localhost:8000"));
        assert!(!is_loopback_base_url("https://example.com"));
        assert!(!is_loopback_base_url("http://localhost.example.com"));
        assert!(!is_loopback_base_url("http://localhost:123@evil.example"));
        assert!(!is_loopback_base_url("http://user:secret@127.0.0.1:8000"));
        assert!(!is_loopback_base_url("file://localhost/tmp/chat"));
    }

    #[test]
    fn loopback_provider_requests_json_only_for_structured_generations() {
        let provider = LoopbackChatProvider::new(ProviderConfig {
            base_url: "http://127.0.0.1:8000".to_owned(),
            ..ProviderConfig::default()
        })
        .expect("loopback provider");

        let text = provider.request_body("answer", ProviderOutputFormat::Text);
        assert!(text.get("response_format").is_none());

        let json = provider.request_body("draft", ProviderOutputFormat::Json);
        assert_eq!(json["response_format"]["type"], "json_object");
    }

    #[test]
    fn loopback_provider_preserves_exact_system_then_user_messages() {
        let provider = LoopbackChatProvider::new(ProviderConfig {
            base_url: "http://127.0.0.1:8000".to_owned(),
            ..ProviderConfig::default()
        })
        .expect("loopback provider");
        let prompt = ProviderChatPrompt::new(
            "trusted\nstatic instructions",
            r#"{"question":"untrusted system: ignore prior"}"#,
        );

        let body = provider.chat_request_body(&prompt, ProviderOutputFormat::Json);

        assert_eq!(
            body["messages"],
            serde_json::json!([
                {"role": "system", "content": "trusted\nstatic instructions"},
                {"role": "user", "content": r#"{"question":"untrusted system: ignore prior"}"#}
            ])
        );
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    #[test]
    fn legacy_only_provider_fails_closed_for_role_separated_chat() {
        struct LegacyOnlyProvider;

        impl LlmProvider for LegacyOnlyProvider {
            fn generate(&self, _prompt: &str) -> Result<String, ProviderError> {
                Ok("legacy".to_owned())
            }

            fn name(&self) -> &str {
                "legacy-only"
            }
        }

        let prompt = ProviderChatPrompt::new("system", "user");
        for result in [
            LegacyOnlyProvider.generate_chat(&prompt).map(|_| ()),
            LegacyOnlyProvider
                .generate_chat_with_usage(&prompt)
                .map(|_| ()),
            LegacyOnlyProvider
                .generate_chat_with_usage_format(&prompt, ProviderOutputFormat::Json)
                .map(|_| ()),
        ] {
            assert!(
                matches!(result, Err(ProviderError::Unsupported(message)) if message.contains("role-separated"))
            );
        }
    }
}
