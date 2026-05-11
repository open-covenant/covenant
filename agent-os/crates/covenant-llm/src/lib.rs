//! LLM provider abstraction for Covenant.
//!
//! Four implementations of the [`Provider`] trait: [`MockProvider`]
//! (tests, no I/O), [`OllamaProvider`] (local at
//! `http://localhost:11434`, no key), [`AnthropicProvider`], and an
//! OpenAI-compatible provider that also covers downstream-compatible
//! services such as DeepSeek, Together, and Groq.
//!
//! Provider selection is the caller's responsibility.
//! [`ProviderConfig`] parses `~/.covenant/secrets.toml` and
//! [`pick_provider`] returns the highest-priority configured backend,
//! falling back to Ollama if reachable and to [`MockProvider`]
//! otherwise.

#![deny(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("provider returned no content")]
    Empty,
    #[error("provider error ({status}): {body}")]
    Status { status: u16, body: String },
    #[error("missing api key for provider {0}")]
    MissingKey(&'static str),
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider tag for logs / metrics.
    fn name(&self) -> &'static str;

    /// Send messages, return the assistant's reply text.
    async fn complete(&self, messages: &[ChatMessage]) -> Result<String, ProviderError>;
}

// ---------- Mock ----------

pub struct MockProvider {
    pub canned: String,
}

impl MockProvider {
    pub fn new(canned: impl Into<String>) -> Self {
        Self {
            canned: canned.into(),
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }
    async fn complete(&self, _messages: &[ChatMessage]) -> Result<String, ProviderError> {
        Ok(self.canned.clone())
    }
}

// ---------- Ollama ----------

pub struct OllamaProvider {
    pub endpoint: String,
    pub model: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            client,
        }
    }

    pub fn local(model: impl Into<String>) -> Self {
        Self::new("http://localhost:11434", model)
    }
}

#[derive(Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
}

#[derive(Deserialize)]
struct OllamaMessage {
    content: String,
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    async fn complete(&self, messages: &[ChatMessage]) -> Result<String, ProviderError> {
        let url = format!("{}/api/chat", self.endpoint.trim_end_matches('/'));
        let body = OllamaChatRequest {
            model: &self.model,
            messages,
            stream: false,
        };
        let resp = self.client.post(&url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: OllamaChatResponse = resp.json().await?;
        if parsed.message.content.is_empty() {
            return Err(ProviderError::Empty);
        }
        Ok(parsed.message.content)
    }
}

// ---------- Anthropic ----------

pub struct AnthropicProvider {
    pub api_key: String,
    pub model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client,
        }
    }
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: Option<&'a str>,
    messages: Vec<AnthropicMessage<'a>>,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    text: String,
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn complete(&self, messages: &[ChatMessage]) -> Result<String, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::MissingKey("anthropic"));
        }
        let mut system_buf = String::new();
        let mut chat = Vec::with_capacity(messages.len());
        for m in messages {
            match m.role {
                Role::System => {
                    if !system_buf.is_empty() {
                        system_buf.push_str("\n\n");
                    }
                    system_buf.push_str(&m.content);
                }
                Role::User => chat.push(AnthropicMessage {
                    role: "user",
                    content: &m.content,
                }),
                Role::Assistant => chat.push(AnthropicMessage {
                    role: "assistant",
                    content: &m.content,
                }),
            }
        }
        let system = (!system_buf.is_empty()).then_some(system_buf.as_str());
        let body = AnthropicRequest {
            model: &self.model,
            max_tokens: 1024,
            system,
            messages: chat,
        };
        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: AnthropicResponse = resp.json().await?;
        let text = parsed
            .content
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return Err(ProviderError::Empty);
        }
        Ok(text)
    }
}

// ---------- OpenAI-compatible ----------

pub struct OpenAiProvider {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            model: model.into(),
            client,
        }
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.openai.com", model)
    }

    pub fn deepseek(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.deepseek.com", model)
    }
}

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn complete(&self, messages: &[ChatMessage]) -> Result<String, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::MissingKey("openai"));
        }
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let body = OpenAiRequest {
            model: &self.model,
            messages,
        };
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: OpenAiResponse = resp.json().await?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();
        if text.is_empty() {
            return Err(ProviderError::Empty);
        }
        Ok(text)
    }
}

// ---------- secrets.toml + auto pick ----------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub llm: Option<LlmSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmSection {
    pub provider: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

impl ProviderConfig {
    pub fn from_path(p: &Path) -> Result<Self, ProviderError> {
        if !p.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(p)?;
        let cfg: Self = toml::from_str(&s)?;
        Ok(cfg)
    }
}

/// Build a provider from a parsed config. Returns `None` if the config is
/// empty or its `provider` value is unknown — caller falls back.
pub fn provider_from_config(cfg: &ProviderConfig) -> Option<Box<dyn Provider>> {
    let llm = cfg.llm.as_ref()?;
    match llm.provider.as_str() {
        "mock" => Some(Box::new(MockProvider::new("mock from config"))),
        "ollama" => {
            let endpoint = llm
                .endpoint
                .clone()
                .unwrap_or_else(|| "http://localhost:11434".into());
            let model = llm.model.clone().unwrap_or_else(|| "llama3.1".into());
            Some(Box::new(OllamaProvider::new(endpoint, model)))
        }
        "anthropic" => {
            let model = llm
                .model
                .clone()
                .unwrap_or_else(|| "claude-haiku-4-5".into());
            let key = llm.api_key.clone().unwrap_or_default();
            Some(Box::new(AnthropicProvider::new(key, model)))
        }
        "openai" => {
            let model = llm.model.clone().unwrap_or_else(|| "gpt-4o-mini".into());
            let key = llm.api_key.clone().unwrap_or_default();
            Some(Box::new(OpenAiProvider::openai(key, model)))
        }
        "deepseek" => {
            let model = llm.model.clone().unwrap_or_else(|| "deepseek-chat".into());
            let key = llm.api_key.clone().unwrap_or_default();
            Some(Box::new(OpenAiProvider::deepseek(key, model)))
        }
        other => {
            debug!(provider = %other, "unknown llm provider in config");
            None
        }
    }
}

/// Best-effort auto-pick: configured provider → reachable Ollama → mock.
/// `secrets_path` is `~/.covenant/secrets.toml` by convention.
pub async fn pick_provider(secrets_path: &Path) -> Box<dyn Provider> {
    if let Ok(cfg) = ProviderConfig::from_path(secrets_path) {
        if let Some(p) = provider_from_config(&cfg) {
            return p;
        }
    }
    if ollama_reachable("http://localhost:11434").await {
        return Box::new(OllamaProvider::local("llama3.1"));
    }
    Box::new(MockProvider::new(
        "covenant-llm: no provider configured; using stub response",
    ))
}

// ---------- Embeddings ----------

#[async_trait]
pub trait Embedder: Send + Sync {
    fn name(&self) -> &'static str;
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ProviderError>;
}

pub struct MockEmbedder {
    pub dim: usize,
}

impl MockEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    fn name(&self) -> &'static str {
        "mock"
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ProviderError> {
        // Deterministic per-text vector via a tiny FNV-1a hash → seed → LCG.
        // Good enough for tests; not for retrieval quality.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        let mut out = Vec::with_capacity(self.dim);
        let mut state = h.max(1);
        for _ in 0..self.dim {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let f = ((state >> 32) as f32 / u32::MAX as f32) * 2.0 - 1.0;
            out.push(f);
        }
        Ok(out)
    }
}

pub struct OllamaEmbedder {
    pub endpoint: String,
    pub model: String,
    client: reqwest::Client,
}

impl OllamaEmbedder {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            client,
        }
    }

    pub fn local(model: impl Into<String>) -> Self {
        Self::new("http://localhost:11434", model)
    }
}

#[derive(Serialize)]
struct OllamaEmbedRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embedding: Vec<f32>,
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    fn name(&self) -> &'static str {
        "ollama"
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, ProviderError> {
        let url = format!("{}/api/embeddings", self.endpoint.trim_end_matches('/'));
        let body = OllamaEmbedRequest {
            model: &self.model,
            prompt: text,
        };
        let resp = self.client.post(&url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: OllamaEmbedResponse = resp.json().await?;
        if parsed.embedding.is_empty() {
            return Err(ProviderError::Empty);
        }
        Ok(parsed.embedding)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbedderConfig {
    #[serde(default)]
    pub embed: Option<EmbedSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbedSection {
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

impl EmbedderConfig {
    pub fn from_path(p: &Path) -> Result<Self, ProviderError> {
        if !p.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(p)?;
        Ok(toml::from_str(&s)?)
    }
}

pub fn embedder_from_config(cfg: &EmbedderConfig) -> Option<Box<dyn Embedder>> {
    let e = cfg.embed.as_ref()?;
    match e.provider.as_str() {
        "mock" => Some(Box::new(MockEmbedder::new(768))),
        "ollama" => {
            let endpoint = e
                .endpoint
                .clone()
                .unwrap_or_else(|| "http://localhost:11434".into());
            let model = e.model.clone().unwrap_or_else(|| "nomic-embed-text".into());
            Some(Box::new(OllamaEmbedder::new(endpoint, model)))
        }
        _ => None,
    }
}

/// Auto-pick an embedder. Same fallback ladder as `pick_provider`: configured
/// → reachable Ollama at `nomic-embed-text` → 768-dim `MockEmbedder`.
pub async fn pick_embedder(secrets_path: &Path) -> Box<dyn Embedder> {
    if let Ok(cfg) = EmbedderConfig::from_path(secrets_path) {
        if let Some(e) = embedder_from_config(&cfg) {
            return e;
        }
    }
    if ollama_reachable("http://localhost:11434").await {
        return Box::new(OllamaEmbedder::local("nomic-embed-text"));
    }
    Box::new(MockEmbedder::new(768))
}

async fn ollama_reachable(endpoint: &str) -> bool {
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_provider_returns_canned_text() {
        let p = MockProvider::new("hi");
        let r = p.complete(&[ChatMessage::user("anything")]).await.unwrap();
        assert_eq!(r, "hi");
        assert_eq!(p.name(), "mock");
    }

    #[tokio::test]
    async fn anthropic_without_key_returns_missing_key() {
        let p = AnthropicProvider::new("", "claude-haiku-4-5");
        let r = p.complete(&[ChatMessage::user("hi")]).await;
        assert!(matches!(r, Err(ProviderError::MissingKey("anthropic"))));
    }

    #[tokio::test]
    async fn openai_without_key_returns_missing_key() {
        let p = OpenAiProvider::openai("", "gpt-4o-mini");
        let r = p.complete(&[ChatMessage::user("hi")]).await;
        assert!(matches!(r, Err(ProviderError::MissingKey("openai"))));
    }

    #[test]
    fn provider_config_parses_anthropic_block() {
        let toml_src = r#"
[llm]
provider = "anthropic"
api_key = "sk-test"
model = "claude-haiku-4-5"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_src).unwrap();
        let p = provider_from_config(&cfg).unwrap();
        assert_eq!(p.name(), "anthropic");
    }

    #[test]
    fn provider_config_falls_back_to_default_model() {
        let toml_src = r#"
[llm]
provider = "ollama"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_src).unwrap();
        let p = provider_from_config(&cfg).unwrap();
        assert_eq!(p.name(), "ollama");
    }

    #[test]
    fn provider_config_with_unknown_provider_returns_none() {
        let toml_src = r#"
[llm]
provider = "made-up"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_src).unwrap();
        assert!(provider_from_config(&cfg).is_none());
    }

    #[tokio::test]
    async fn pick_provider_with_no_config_returns_some_provider() {
        // No file → either ollama (if local), or mock fallback. Either way,
        // we get a Provider instance.
        let dir = std::env::temp_dir();
        let nope = dir.join(format!("covenant-no-secrets-{}.toml", uuid_like()));
        let p = pick_provider(&nope).await;
        assert!(p.name() == "mock" || p.name() == "ollama");
    }

    fn uuid_like() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[tokio::test]
    async fn mock_embedder_is_deterministic_for_same_input() {
        let e = MockEmbedder::new(64);
        let a = e.embed("hello").await.unwrap();
        let b = e.embed("hello").await.unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(a, b);
        let c = e.embed("world").await.unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn embedder_config_parses_ollama_block() {
        let toml_src = r#"
[embed]
provider = "ollama"
model = "nomic-embed-text"
"#;
        let cfg: EmbedderConfig = toml::from_str(toml_src).unwrap();
        let e = embedder_from_config(&cfg).unwrap();
        assert_eq!(e.name(), "ollama");
    }
}
