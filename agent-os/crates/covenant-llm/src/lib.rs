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

    #[test]
    fn role_serde_and_chat_message_constructors_pin_wire_contract() {
        // The wire form of Role flows into OpenAiRequest and
        // OllamaChatRequest bodies via embedded &[ChatMessage]. OpenAI,
        // DeepSeek, and Ollama all expect the lowercase slugs; a
        // titlecase regression would silently break every chat call.
        for (variant, slug) in [
            (Role::System, "system"),
            (Role::User, "user"),
            (Role::Assistant, "assistant"),
        ] {
            let wire = serde_json::to_string(&variant).unwrap();
            assert_eq!(wire, format!("\"{slug}\""));
            let back: Role = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, variant);
        }

        // Each constructor must bind its variant to the role field; a
        // copy-paste swap between ::user and ::assistant would silently
        // mis-tag prompts so the model sees user turns as assistant.
        let sys = ChatMessage::system("you are a covenant agent");
        assert_eq!(sys.role, Role::System);
        assert_eq!(sys.content, "you are a covenant agent");

        let usr = ChatMessage::user("hello");
        assert_eq!(usr.role, Role::User);
        assert_eq!(usr.content, "hello");

        let ast = ChatMessage::assistant("acknowledged");
        assert_eq!(ast.role, Role::Assistant);
        assert_eq!(ast.content, "acknowledged");

        // Titlecase slugs must fail loud — the rename_all = lowercase
        // contract stays a whitelist so a future #[serde(other)] arm
        // cannot silently absorb mis-cased upstream payloads.
        assert!(serde_json::from_str::<Role>("\"System\"").is_err());
        assert!(serde_json::from_str::<Role>("\"USER\"").is_err());
    }

    #[test]
    fn chat_message_serde_pins_two_required_fields_and_embedded_role_slug() {
        // ChatMessage is the public struct embedded into every outbound
        // chat-API request body via `messages: &[ChatMessage]` —
        // OllamaChatRequest, OpenAiRequest, and the Anthropic flow all
        // serialise this exact shape. Two strictly required fields
        // (role: Role, content: String) with no serde attributes; a
        // stray #[serde(default)] on content would silently let an
        // upstream API echo back an empty message, a rename of either
        // key would break every provider without a compile error, and
        // an added #[serde(skip_serializing_if = ...)] field would
        // mutate the wire shape that strict APIs reject.
        let msg = ChatMessage::user("hello");

        let wire = serde_json::to_value(&msg).unwrap();
        let obj = wire
            .as_object()
            .expect("ChatMessage serialises as a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> = ["role", "content"].into_iter().collect();
        assert_eq!(
            keys, expected,
            "ChatMessage wire form must be exactly two keys (role, \
             content); a skip_serializing_if on either field would \
             silently shift the envelope and strict chat APIs may \
             reject the truncated body",
        );

        // Cross-bind to role_serde_and_chat_message_constructors_pin_wire_contract:
        // the embedded Role enum still serialises as a lowercase slug.
        assert_eq!(obj.get("role"), Some(&serde_json::json!("user")));
        assert_eq!(obj.get("content"), Some(&serde_json::json!("hello")));

        // Round-trip pins the PartialEq + Eq derive contract callers
        // (and tests like mock_provider_returns_canned_text) rely on.
        let back: ChatMessage = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(back, msg);

        // Each strictly-required field must reject when omitted. A
        // #[serde(default)] regression on either would let a malformed
        // upstream payload decode with an empty string / default Role
        // and silently bypass the boundary.
        for required in ["role", "content"] {
            let mut missing = obj.clone();
            missing.remove(required);
            assert!(
                serde_json::from_value::<ChatMessage>(serde_json::Value::Object(missing)).is_err(),
                "ChatMessage wire form must reject a payload missing {required:?}",
            );
        }

        // Empty content must still surface on the wire — pinning that
        // String::is_empty is NOT skipped. The provider may legitimately
        // send an assistant turn with empty content (the parser maps
        // that to ProviderError::Empty), and a skip_serializing_if =
        // String::is_empty would silently drop the field for the
        // empty-body path so the receiver sees a missing-field error
        // instead of the intended empty-string semantic.
        let empty = ChatMessage::assistant("");
        let empty_wire = serde_json::to_value(&empty).unwrap();
        let empty_obj = empty_wire.as_object().unwrap();
        assert!(
            empty_obj.contains_key("content"),
            "empty content must remain present on the wire — a \
             skip_serializing_if = String::is_empty would silently \
             drop the field for the empty-body path",
        );
        assert_eq!(empty_obj.get("content").unwrap(), &serde_json::json!(""));
    }

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
    fn provider_error_display_messages_pin_three_string_variant_format_strings() {
        // ProviderError (lib.rs lines 58-74) has seven variants. Five
        // wrap external errors via #[from]; the three string-literal
        // variants (Empty, Status, MissingKey) emit operator-facing
        // format strings that no existing test inspects.
        // anthropic_without_key_returns_missing_key (line 781) and
        // openai_without_key_returns_missing_key (line 788) assert
        // MissingKey via `matches!` which ignores the Display rendering;
        // Empty and Status have no test at all. A thiserror format-
        // string typo or a swapped {status}/{body} binding would
        // silently degrade operator diagnostics with no parse-time
        // signal — operators reading daemon logs would see a different
        // shape than the dashboards that grep for the documented
        // wording grep for.

        let empty = ProviderError::Empty;
        assert_eq!(
            format!("{empty}"),
            "provider returned no content",
            "ProviderError::Empty Display must remain the literal \
             'provider returned no content' — fires when the wire \
             response decoded successfully but the content array was \
             empty (Anthropic line 288, OpenAI line 392, Ollama line \
             175). A refactor that rewrote it to 'provider returned \
             no body' under a 'match the Status body slot wording' \
             rationale would silently shift dashboards that grep for \
             'no content' to the new phrasing"
        );

        let status = ProviderError::Status {
            status: 401,
            body: "Unauthorized".into(),
        };
        let status_message = format!("{status}");
        assert!(
            status_message.contains("(401)"),
            "ProviderError::Status Display must parenthesize the status \
             code as '(401)' — the documented format 'provider error \
             ({{status}}): {{body}}' puts the status in parentheses so \
             dashboards can grep for '\\(401\\)' to filter by HTTP code; \
             a swap to 'provider error: 401, Unauthorized' under an \
             'alphabetize template variables' rationale would silently \
             break the parenthesized-status convention: {status_message}"
        );
        assert!(
            status_message.contains("Unauthorized"),
            "ProviderError::Status must surface the body string — the \
             body carries the upstream provider's error reason, and a \
             refactor that dropped the {{body}} slot under a 'too \
             noisy' pass would strip the operator's only diagnostic \
             into the upstream provider's reasoning: {status_message}"
        );
        assert!(
            status_message.contains("provider error"),
            "ProviderError::Status must keep the 'provider error' \
             prefix — distinguishes this variant from Empty and \
             MissingKey in dashboards that group by message prefix: \
             {status_message}"
        );
        assert!(
            !status_message.contains("(Unauthorized)"),
            "ProviderError::Status body must NOT appear in the \
             parenthesized slot — a #[error] format swap binding {{body}} \
             to the parenthesized slot and {{status}} to the suffix \
             would emit 'provider error (Unauthorized): 401' instead of \
             'provider error (401): Unauthorized'. Operators reading \
             the swapped form would scan for an HTTP code in the parens \
             and find the body text; pinning that the body does NOT \
             appear in parentheses anchors the slot ordering: \
             {status_message}"
        );

        let missing = ProviderError::MissingKey("anthropic");
        assert_eq!(
            format!("{missing}"),
            "missing api key for provider anthropic",
            "ProviderError::MissingKey Display must remain the literal \
             'missing api key for provider <slug>' — the 'for provider' \
             prefix is the semantic anchor distinguishing 'missing api \
             key' from other missing-thing errors; dashboards parse the \
             slug to identify which secrets.toml entry the operator \
             must populate. A rewrite to 'missing api key anthropic' \
             under a 'less verbose' pass would silently break the parse \
             contract"
        );

        // Anchor that the slug parameter is bound verbatim so a refactor
        // that wrapped or transformed the slug in the format string
        // (e.g., uppercasing it) surfaces here as well.
        let missing_openai = ProviderError::MissingKey("openai");
        assert_eq!(
            format!("{missing_openai}"),
            "missing api key for provider openai",
            "ProviderError::MissingKey must bind the slug verbatim \
             without case transformation — a refactor that uppercased \
             the slug (e.g., '{{0:upper}}' if thiserror supported it, \
             or a manual Display impl) would silently shift dashboards \
             grep for 'provider openai' to 'provider OPENAI'"
        );
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

    #[test]
    fn provider_from_config_pins_mock_openai_and_deepseek_discriminator_mapping() {
        // provider_from_config matches LlmSection.provider against five
        // exact slugs (mock, ollama, anthropic, openai, deepseek). The
        // existing tests pin two arms (anthropic via
        // provider_config_parses_anthropic_block; ollama via
        // provider_config_falls_back_to_default_model) and the
        // unknown-provider rejection arm (provider_config_with_unknown_provider_returns_none),
        // but three exact-match arms — mock, openai, and deepseek — are
        // not pinned. A refactor that renamed any of the three slugs
        // would silently send configured deployments through the
        // unknown-provider arm into the documented Ollama/mock fallback
        // ladder; the operator's secrets.toml would still parse, but
        // the configured provider would never be selected and the
        // daemon would silently degrade to the auto-detect ladder on
        // every restart with no parse-time signal.
        //
        // openai and deepseek both dispatch to OpenAiProvider (with
        // distinct base URLs supplied by ::openai and ::deepseek
        // constructors). Both .name() returns are therefore "openai" —
        // the deepseek arm is pinned by asserting the slug returns
        // Some(_) (i.e., does NOT fall through to the unknown arm),
        // not by name-distinction. This keeps the test deterministic
        // and offline-friendly without calling .complete() on any
        // provider.
        let mock_toml = r#"
[llm]
provider = "mock"
"#;
        let cfg: ProviderConfig = toml::from_str(mock_toml).unwrap();
        let p = provider_from_config(&cfg).expect(
            "the mock arm must dispatch to MockProvider; a slug rename \
             (e.g., to 'stub') would silently fall through into the \
             auto-detect ladder and pick a real Ollama provider in \
             environments where Ollama is reachable, breaking \
             deterministic test-fixture deployments",
        );
        assert_eq!(
            p.name(),
            "mock",
            "mock provider must surface name='mock' so operator dashboards \
             can identify deterministic test-fixture deployments at a glance",
        );

        let openai_toml = r#"
[llm]
provider = "openai"
api_key = "sk-test"
model = "gpt-4o-mini"
"#;
        let cfg: ProviderConfig = toml::from_str(openai_toml).unwrap();
        let p = provider_from_config(&cfg).expect(
            "the openai arm must dispatch to OpenAiProvider; a slug \
             rename or merge into a generic 'openai-compat' arm would \
             silently fall through into the auto-detect ladder",
        );
        assert_eq!(
            p.name(),
            "openai",
            "openai-configured provider must surface name='openai'",
        );

        let deepseek_toml = r#"
[llm]
provider = "deepseek"
api_key = "sk-test"
model = "deepseek-chat"
"#;
        let cfg: ProviderConfig = toml::from_str(deepseek_toml).unwrap();
        let p = provider_from_config(&cfg).expect(
            "the deepseek arm must dispatch to OpenAiProvider via the \
             ::deepseek constructor; a slug rename (e.g., to \
             'deepseek-chat' for parity with the model name) or a merge \
             into a generic 'openai-compat' arm would silently fall \
             through into the unknown-provider arm and the daemon's \
             auto-detect ladder would pick Ollama or mock with no \
             parse-time signal",
        );
        // OpenAiProvider::name() is the type name, not the constructor
        // name; both ::openai and ::deepseek constructors share name
        // == "openai". The deepseek arm's pin is the Some(_) above —
        // distinguishing the base URL would require downcasting the
        // Box<dyn Provider> back to OpenAiProvider, which the trait
        // object does not expose. Keep the assertion at the level the
        // public surface supports.
        assert_eq!(
            p.name(),
            "openai",
            "deepseek-configured provider also surfaces name='openai' \
             because both arms share OpenAiProvider; the deepseek arm's \
             distinct identity is the base URL bound by \
             OpenAiProvider::deepseek (https://api.deepseek.com), not \
             the provider name",
        );
    }

    #[test]
    fn ollama_provider_and_embedder_local_pin_canonical_endpoint() {
        // OllamaProvider::local (line 130) and OllamaEmbedder::local
        // (line 547) both hardcode http://localhost:11434 — the
        // canonical Ollama API port. pick_provider (line 476) and
        // pick_embedder (line 641) probe ollama_reachable against the
        // same URL string before dispatching to local(). If the
        // constructor's hardcoded endpoint diverged from the probe URL,
        // auto-detect would succeed against the probe but the
        // constructed provider/embedder would point at a stale port —
        // completions/embeddings fail with connection refused on the
        // new port with no signal that the constructor diverged from
        // the probe.
        //
        // Existing tests pin the endpoint via TOML config decoding
        // (line 1183 reads endpoint from a parsed [llm] block) but
        // never directly assert the local() constructor's hardcoded
        // string. Mirrors openai_provider_constructors_pin_canonical_base_urls
        // for the OpenAi pair.
        let p = OllamaProvider::local("llama3.1");
        assert_eq!(
            p.endpoint, "http://localhost:11434",
            "OllamaProvider::local must bind http://localhost:11434 — \
             the same URL pick_provider/ollama_reachable probe; a port \
             bump in the constructor without a matching bump in the \
             probe would let auto-detect succeed against the probe and \
             then construct a provider pointing at a stale port"
        );
        assert_eq!(
            p.model, "llama3.1",
            "OllamaProvider::local must pass model through verbatim; a \
             refactor that defaulted the model would silently send a \
             different identifier than the operator configured"
        );
        assert_eq!(
            p.name(),
            "ollama",
            "OllamaProvider must surface name='ollama' — cross-binds \
             the auto-detect ladder's identity contract"
        );

        let e = OllamaEmbedder::local("nomic-embed-text");
        assert_eq!(
            e.endpoint, "http://localhost:11434",
            "OllamaEmbedder::local must bind the same canonical port — \
             a divergence between Provider::local and Embedder::local \
             (one bumped to 11435, the other left at 11434) would \
             silently split auto-detect dispatch: completions succeed \
             on one port, embeddings fail on the other, and \
             search_similar drops everything below threshold with no \
             signal that the embedder constructor diverged"
        );
        assert_eq!(
            e.model, "nomic-embed-text",
            "OllamaEmbedder::local must pass model through verbatim; \
             same regression class as the Provider arm"
        );
        assert_eq!(
            e.name(),
            "ollama",
            "OllamaEmbedder must surface name='ollama' — parallel to \
             the Provider arm's identity contract"
        );

        assert_eq!(
            p.endpoint, e.endpoint,
            "the two local() constructors must bind the same canonical \
             endpoint; pick_provider and pick_embedder both probe one \
             URL and dispatch to one constructor each — a divergence \
             between them is the regression class this assertion \
             catches loud"
        );
    }

    #[test]
    fn openai_provider_constructors_pin_canonical_base_urls() {
        // OpenAiProvider::openai (line 322) and OpenAiProvider::deepseek
        // (line 326) are convenience constructors that hardcode the
        // canonical API base URL for each provider. The existing
        // provider_from_config pin observes that distinguishing the
        // base URL via the Box<dyn Provider> trait object is not
        // possible — but the struct's base_url is pub String, so direct
        // construction lets a unit test pin the URL drift surface.
        //
        // A URL drift regression (mistyped tld, regional split,
        // accidental version-path duplication) would silently break
        // every openai- or deepseek-configured deployment at runtime
        // with no parse-time signal — the daemon's ProviderError::Status
        // arm fires with whatever status the new host returns and the
        // operator must correlate the runtime failure back to the
        // constructor.
        let openai = OpenAiProvider::openai("k-openai", "gpt-4o");
        assert_eq!(
            openai.base_url, "https://api.openai.com",
            "openai constructor must bind https://api.openai.com; a \
             refactor that pointed it elsewhere (e.g., https://api.openai.io \
             for an experimental endpoint, or https://api.openai.com/v1 \
             redundantly prepending the version path that complete() \
             appends) would break every openai-configured deployment at \
             runtime with no parse-time signal"
        );
        assert_eq!(
            openai.api_key, "k-openai",
            "openai constructor must pass api_key through verbatim; a \
             refactor that base64-encoded or rewrote the key inside the \
             constructor would silently send a malformed Bearer token \
             on every request and surface a generic 401 to operators"
        );
        assert_eq!(
            openai.model, "gpt-4o",
            "openai constructor must pass model through verbatim; a \
             refactor that defaulted or rewrote the model name would \
             silently send a different model identifier than the \
             operator configured"
        );
        assert_eq!(
            openai.name(),
            "openai",
            "openai constructor surfaces name='openai' — cross-binds the \
             provider_from_config_pins_mock_openai_and_deepseek_discriminator_mapping \
             pin that documents this shared identity contract"
        );

        let deepseek = OpenAiProvider::deepseek("k-deepseek", "deepseek-chat");
        assert_eq!(
            deepseek.base_url, "https://api.deepseek.com",
            "deepseek constructor must bind https://api.deepseek.com; a \
             refactor that pointed it elsewhere (e.g., \
             https://api.deepseek.ai for a regional split, or \
             https://deepseek.com/api/v1) would silently break every \
             deepseek-configured deployment at runtime; the daemon's \
             ProviderError::Status fires with whatever the new host \
             returns and operators must correlate the failure back to \
             the constructor"
        );
        assert_eq!(
            deepseek.api_key, "k-deepseek",
            "deepseek constructor must pass api_key through verbatim; \
             same regression class as the openai arm"
        );
        assert_eq!(
            deepseek.model, "deepseek-chat",
            "deepseek constructor must pass model through verbatim; \
             same regression class as the openai arm"
        );
        assert_eq!(
            deepseek.name(),
            "openai",
            "deepseek constructor also surfaces name='openai' since \
             both arms share the OpenAiProvider trait impl — cross-binds \
             the existing provider_from_config pin comment that \
             documents this shared identity (the deepseek arm's distinct \
             identity is the base URL, not the provider name)"
        );

        assert_ne!(
            openai.base_url, deepseek.base_url,
            "the two constructors must bind distinct URLs; a refactor \
             that merged openai and deepseek into a single canonical \
             host would silently route deepseek-configured deployments \
             to openai's API (and 401 on the wrong key) or vice versa"
        );
    }

    #[test]
    fn provider_config_serde_pins_optional_llm_section_default() {
        // ProviderConfig is the top-level secrets.toml decoder for the
        // [llm] section. The struct carries a single field — llm:
        // Option<LlmSection> — with field-level #[serde(default)] and a
        // derived Default impl. ProviderConfig::from_path returns
        // Self::default() when the file is missing, and pick_provider
        // treats llm.is_none() as the fall-through to the documented
        // Ollama/mock ladder.
        //
        // LlmSection itself is pinned by
        // llm_section_serde_pins_required_provider_and_option_defaults
        // but no test pins the wrapper's field name, the None default
        // on an empty TOML payload, or the parse contract when [llm] is
        // omitted but other root keys exist. A refactor that renamed
        // ProviderConfig::llm (or applied #[serde(rename)] in either
        // direction) would silently make every operator's secrets.toml
        // decode into ProviderConfig::default(), pick_provider would
        // fall through to Ollama/mock with no parse signal, and
        // configured Anthropic/OpenAI/DeepSeek deployments would
        // silently lose their provider on next daemon restart.
        let empty: ProviderConfig = toml::from_str("").unwrap();
        assert!(
            empty.llm.is_none(),
            "ProviderConfig must decode an empty TOML payload as \
             ProviderConfig::default() with llm == None — \
             ProviderConfig::from_path relies on this path to return \
             Self::default() for missing secrets.toml, and pick_provider \
             relies on llm.is_none() to fall through to the Ollama/mock \
             ladder"
        );

        // No [llm] section but other unknown root keys present — the
        // top-level deserializer must still produce llm == None rather
        // than rejecting the unknown key (the struct does not carry
        // #[serde(deny_unknown_fields)]).
        let other_root = r#"
[unrelated]
key = "value"
"#;
        let cfg: ProviderConfig = toml::from_str(other_root).unwrap();
        assert!(
            cfg.llm.is_none(),
            "ProviderConfig must tolerate unknown root sections without \
             refusing to parse — operators write [embed]/[search] blocks \
             into the same secrets.toml and a refactor that added \
             #[serde(deny_unknown_fields)] would break every co-resident \
             secrets.toml at daemon start"
        );

        // [llm] fully populated — wrapper round-trips into the
        // LlmSection. Pin one field from the inner section so a refactor
        // that broke the wrapper's deserialization path (e.g., renamed
        // the wrapper field) would fail this assertion rather than
        // landing on a misleading inner-field error.
        let full = r#"
[llm]
provider = "anthropic"
api_key = "sk-test"
model = "claude-haiku-4-5"
endpoint = "https://api.example/v1"
"#;
        let cfg: ProviderConfig = toml::from_str(full).unwrap();
        let llm = cfg
            .llm
            .as_ref()
            .expect("[llm] block must surface through ProviderConfig.llm");
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.api_key.as_deref(), Some("sk-test"));
        assert_eq!(llm.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(llm.endpoint.as_deref(), Some("https://api.example/v1"));

        // [llm] with only the strictly-required provider field — the
        // inner section's Option defaults must surface through the
        // wrapper so partial secrets.toml stays forward-compatible.
        let minimal = r#"
[llm]
provider = "ollama"
"#;
        let cfg: ProviderConfig = toml::from_str(minimal).unwrap();
        let llm = cfg.llm.as_ref().unwrap();
        assert_eq!(llm.provider, "ollama");
        assert!(llm.api_key.is_none());
        assert!(llm.model.is_none());
        assert!(llm.endpoint.is_none());

        // ProviderConfig::default() must match the empty-TOML decode —
        // pin this so a refactor that diverged the derived Default impl
        // from the empty-TOML path would fail loud.
        let default_cfg = ProviderConfig::default();
        assert!(
            default_cfg.llm.is_none(),
            "ProviderConfig::default() must have llm == None to match \
             the empty-TOML decode path that ProviderConfig::from_path \
             relies on for missing secrets.toml"
        );
    }

    #[test]
    fn llm_section_serde_pins_required_provider_and_option_defaults() {
        // LlmSection is the [llm] block operators write to secrets.toml
        // to pick the chat provider. The contract is asymmetric: provider
        // is strictly required (no serde default) while api_key, model,
        // and endpoint each carry #[serde(default)] so a stale or minimal
        // secrets.toml stays forward-compatible.
        //
        // The existing tests cover provider-selection paths but not the
        // strict-required-provider contract: a refactor that added
        // #[serde(default)] to provider would silently parse blocks with
        // no provider as provider='', provider_from_config would land on
        // the unknown-provider arm and return None, and pick_provider
        // would fall back to MockProvider — operator-facing LLM workflows
        // would silently use the stub instead of the configured provider.
        let full = r#"
[llm]
provider = "anthropic"
api_key = "sk-test"
model = "claude-haiku-4-5"
endpoint = "https://api.example/v1"
"#;
        let cfg: ProviderConfig = toml::from_str(full).unwrap();
        let llm = cfg
            .llm
            .as_ref()
            .expect("[llm] block must surface as LlmSection");
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.api_key.as_deref(), Some("sk-test"));
        assert_eq!(llm.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(llm.endpoint.as_deref(), Some("https://api.example/v1"));

        let minimal = r#"
[llm]
provider = "ollama"
"#;
        let cfg: ProviderConfig = toml::from_str(minimal).unwrap();
        let llm = cfg.llm.as_ref().unwrap();
        assert_eq!(llm.provider, "ollama");
        assert!(
            llm.api_key.is_none(),
            "LlmSection::api_key must decode as None when omitted; a \
             refactor that dropped #[serde(default)] would silently fail \
             parse for every operator deployment running on the default \
             Ollama endpoint with no api key"
        );
        assert!(
            llm.model.is_none(),
            "LlmSection::model must decode as None when omitted"
        );
        assert!(
            llm.endpoint.is_none(),
            "LlmSection::endpoint must decode as None when omitted"
        );

        // Each Option field's omission must be tolerated independently —
        // operators write partial blocks all the time (model only,
        // endpoint only, etc.). Pin each in isolation so a refactor that
        // dropped the default on one but not the others would fail loud
        // on the specific arm rather than silently masquerading as a
        // generic parse error.
        let no_api_key = r#"
[llm]
provider = "ollama"
model = "llama3.1"
endpoint = "http://localhost:11434"
"#;
        let cfg: ProviderConfig = toml::from_str(no_api_key).unwrap();
        let llm = cfg.llm.as_ref().unwrap();
        assert!(llm.api_key.is_none());
        assert_eq!(llm.model.as_deref(), Some("llama3.1"));
        assert_eq!(llm.endpoint.as_deref(), Some("http://localhost:11434"));

        let no_model = r#"
[llm]
provider = "anthropic"
api_key = "sk"
endpoint = "https://api.example"
"#;
        let cfg: ProviderConfig = toml::from_str(no_model).unwrap();
        let llm = cfg.llm.as_ref().unwrap();
        assert!(llm.model.is_none());

        let no_endpoint = r#"
[llm]
provider = "openai"
api_key = "sk"
model = "gpt-4o-mini"
"#;
        let cfg: ProviderConfig = toml::from_str(no_endpoint).unwrap();
        let llm = cfg.llm.as_ref().unwrap();
        assert!(llm.endpoint.is_none());

        // Provider is the only strictly-required field; omitting it must
        // fail parse so a future #[serde(default)] regression on provider
        // does not silently let a misconfigured secrets.toml fall back to
        // the mock provider.
        let no_provider = r#"
[llm]
api_key = "sk"
model = "x"
"#;
        assert!(
            toml::from_str::<ProviderConfig>(no_provider).is_err(),
            "LlmSection::provider must remain strictly required; a \
             #[serde(default)] regression would silently parse blocks with \
             no provider as provider='' and pick_provider would fall back \
             to MockProvider — operator-facing LLM workflows would silently \
             use the stub instead of the configured provider"
        );
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
    async fn mock_embedder_output_components_lie_in_minus_one_to_one_inclusive_range() {
        // covenant_llm::MockEmbedder::embed (line 507-525) produces
        // deterministic per-text embedding vectors used by tests that
        // exercise cosine similarity over memory records. Line 521 is
        // the component formula:
        //
        //   let f = ((state >> 32) as f32 / u32::MAX as f32) * 2.0 - 1.0;
        //
        // The upper 32 bits of the LCG state scale into [0.0, 1.0],
        // then the linear remap '* 2.0 - 1.0' shifts to [-1.0, 1.0].
        // The bounded zero-centered range is what makes cosine
        // similarity over MockEmbedder vectors well-defined and
        // numerically stable.
        //
        // mock_embedder_is_deterministic_for_same_input (just below)
        // pins the length and same-input determinism. Neither it nor
        // any other test pins the COMPONENT RANGE. A refactor that
        // dropped '* 2.0 - 1.0' (leaving values in [0.0, 1.0]) under
        // a 'simplify to non-negative' rationale, or that expanded to
        // '* 4.0 - 2.0' under an 'expand range for stronger gradient'
        // rationale, would silently shift cosine-similarity score
        // distributions across every test that consumes MockEmbedder
        // without a parse-time signal.

        let e = MockEmbedder::new(256);
        let first = e
            .embed("sample text for range pin")
            .await
            .expect("MockEmbedder::embed must not fail on a non-empty input");
        assert_eq!(
            first.len(),
            256,
            "MockEmbedder::new(256) must produce a 256-component vector — \
             a length mismatch here would indicate the dim wiring \
             changed and the range assertions below are operating on \
             unexpected vector shape",
        );

        for (i, f) in first.iter().enumerate() {
            assert!(
                (-1.0..=1.0).contains(f),
                "first sample component {i} = {f} must lie in \
                 [-1.0, 1.0] inclusive — a refactor that dropped the \
                 '* 2.0 - 1.0' remap would silently leave components \
                 in [0.0, 1.0], or that widened to '* 4.0 - 2.0' \
                 would silently produce values outside this range",
            );
        }

        let second = e
            .embed("another anchor for range pin")
            .await
            .expect("MockEmbedder::embed must not fail on a non-empty input");
        for (i, f) in second.iter().enumerate() {
            assert!(
                (-1.0..=1.0).contains(f),
                "second sample component {i} = {f} must lie in \
                 [-1.0, 1.0] inclusive — anchors that the bound is \
                 not accidentally specific to the first sample's hash \
                 path; a refactor where the range invariant only held \
                 for some hash seeds would surface here",
            );
        }

        let union: Vec<f32> = first.iter().chain(second.iter()).copied().collect();
        let any_strictly_negative = union.iter().any(|&f| f < 0.0);
        let any_strictly_positive = union.iter().any(|&f| f > 0.0);
        assert!(
            any_strictly_negative,
            "at least one component across the two samples must be \
             strictly negative (< 0.0) — a refactor that dropped the \
             '* 2.0 - 1.0' linear remap would silently leave every \
             component in [0.0, 1.0]; the range-inclusive assertions \
             above would still pass because [0.0, 1.0] is a subset of \
             [-1.0, 1.0], but pinning the zero-crossing here catches \
             the dropped-remap regression directly",
        );
        assert!(
            any_strictly_positive,
            "at least one component across the two samples must be \
             strictly positive (> 0.0) — anchors that the linear \
             remap produces both signs, not just negative; a refactor \
             that flipped the remap to '1.0 - * 2.0' would silently \
             invert the sign distribution while still satisfying the \
             range bound",
        );
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

    #[test]
    fn embedder_from_config_pins_mock_arm_and_unknown_provider_rejection() {
        // embedder_from_config matches EmbedSection.provider against
        // two exact slugs (mock, ollama) and falls through to None for
        // any other value. The existing test pins the ollama arm; the
        // mock arm and the unknown-provider rejection arm are not
        // pinned. Mirrors the just-added
        // provider_from_config_pins_mock_openai_and_deepseek_discriminator_mapping
        // coverage shape and the existing
        // search_config_unknown_provider_returns_none rejection pin.
        //
        // A refactor that renamed the 'mock' slug would silently send
        // mock-configured deployments through the unknown-provider arm;
        // pick_embedder would then fall through to the
        // Ollama-or-MockEmbedder ladder and bind a real Ollama
        // embedder against test data in environments where Ollama is
        // reachable, breaking deterministic test-fixture deployments.
        // A refactor that relaxed the unknown-provider rejection to
        // return Some(MockEmbedder::default()) would silently mask
        // configuration typos as successful mock-binding.
        let mock_toml = r#"
[embed]
provider = "mock"
"#;
        let cfg: EmbedderConfig = toml::from_str(mock_toml).unwrap();
        let e = embedder_from_config(&cfg).expect(
            "the mock arm must dispatch to MockEmbedder; a slug rename \
             (e.g., to 'stub' or 'fake') would silently fall through \
             into the auto-detect ladder and bind a real Ollama \
             embedder against test data in environments where Ollama \
             is reachable",
        );
        assert_eq!(
            e.name(),
            "mock",
            "mock embedder must surface name='mock' so vector-search \
             dashboards can identify deterministic test-fixture \
             deployments at a glance",
        );

        let unknown_toml = r#"
[embed]
provider = "made-up"
"#;
        let cfg: EmbedderConfig = toml::from_str(unknown_toml).unwrap();
        assert!(
            embedder_from_config(&cfg).is_none(),
            "embedder_from_config must reject an unknown provider slug \
             with None so misspellings ('olama' / 'ollam') surface as \
             explicit None at the parse boundary; a refactor that \
             relaxed this arm to fail-open with MockEmbedder::default() \
             would silently bind a 768-dim mock embedder for typos and \
             vector search would regress to constant-distance results \
             with no parse-time signal",
        );
    }

    #[tokio::test]
    async fn from_config_mock_arms_pin_canonical_default_canned_string_and_dim() {
        // provider_from_config (line 434) and embedder_from_config
        // (line 620) both carry parallel hardcoded mock-arm defaults
        // that the existing discriminator pins
        // (provider_from_config_pins_mock_openai_and_deepseek_discriminator_mapping
        // and embedder_from_config_pins_mock_arm_and_unknown_provider_rejection)
        // do not observe: MockProvider::new("mock from config") and
        // MockEmbedder::new(768). The existing tests only assert
        // .name() == "mock" through the Box<dyn _> trait object, so a
        // refactor that bumped the dim to 384 to match a model rename,
        // or that emptied the canned string while wiring through a
        // richer breadcrumb in pick_provider, would silently shift
        // operator-facing semantics with no parse-time or compile-time
        // signal.
        //
        // Both values are observable through the trait object without
        // downcasting: MockProvider::complete (line 100-108) returns
        // self.canned.clone() regardless of messages, so the canned
        // string surfaces verbatim through Provider::complete();
        // MockEmbedder::embed (line 507-524) returns a Vec<f32> whose
        // .len() equals self.dim regardless of input text, so the dim
        // surfaces verbatim through Embedder::embed().len(). The same
        // hardcoded values appear in pick_provider (line 479-481) and
        // pick_embedder (line 644) auto-detect fallbacks — a divergence
        // between config-driven mock and auto-detect-fallback mock
        // would silently split operator dashboards (different
        // breadcrumbs depending on secrets.toml presence) and split
        // memory-store geometry (different dims depending on whether
        // [embed] provider="mock" is present vs the section being
        // absent).
        let provider_mock_toml = r#"
[llm]
provider = "mock"
"#;
        let cfg: ProviderConfig = toml::from_str(provider_mock_toml).unwrap();
        let p = provider_from_config(&cfg).expect(
            "the mock arm must dispatch to MockProvider; cross-binds the \
             existing provider_from_config_pins_mock_openai_and_deepseek_discriminator_mapping \
             pin that documents this dispatch contract",
        );
        assert_eq!(
            p.name(),
            "mock",
            "mock provider must surface name='mock' — cross-binds the \
             existing discriminator pin's identity contract",
        );
        let canned = p.complete(&[ChatMessage::user("ignored")]).await.expect(
            "MockProvider::complete must succeed regardless of \
                 message contents — the canned response is the entire \
                 contract surface and a future refactor that gated \
                 complete() behind a non-empty messages check would \
                 break the mock-mode test-fixture deployments \
                 documented in the discriminator pin",
        );
        assert_eq!(
            canned, "mock from config",
            "provider_from_config mock arm must construct \
             MockProvider::new(\"mock from config\") so the canned \
             breadcrumb surfaces verbatim through complete(); a \
             refactor that emptied the string or rewrote it to a \
             generic 'stub' value would silently shift the operator-\
             facing breadcrumb that distinguishes config-driven mock \
             (provider=\"mock\" in secrets.toml) from auto-detect-\
             fallback mock (pick_provider with no config and no \
             reachable Ollama, which carries the distinct \"covenant-\
             llm: no provider configured; using stub response\" \
             breadcrumb at line 479-481) — operator dashboards that \
             surface complete() output would lose the diagnostic that \
             tells them which mock path they landed on",
        );

        let embedder_mock_toml = r#"
[embed]
provider = "mock"
"#;
        let cfg: EmbedderConfig = toml::from_str(embedder_mock_toml).unwrap();
        let e = embedder_from_config(&cfg).expect(
            "the mock arm must dispatch to MockEmbedder; cross-binds \
             the existing embedder_from_config_pins_mock_arm_and_unknown_provider_rejection \
             pin that documents this dispatch contract",
        );
        assert_eq!(
            e.name(),
            "mock",
            "mock embedder must surface name='mock' — cross-binds the \
             existing discriminator pin's identity contract",
        );
        let vector = e.embed("any").await.expect(
            "MockEmbedder::embed must succeed for arbitrary text — the \
             FNV-1a hash + LCG path has no failure branch and a future \
             refactor that introduced an empty-text rejection would \
             break the deterministic mock-mode embedding contract \
             documented in mock_embedder_is_deterministic_for_same_input",
        );
        assert_eq!(
            vector.len(),
            768,
            "embedder_from_config mock arm must construct \
             MockEmbedder::new(768) so embed() returns a 768-dim \
             vector; a refactor that bumped the dim to 384 or 1536 to \
             match a model-default rename or to mirror a separate \
             Ollama embedding dimensionality would silently shift the \
             geometry of mock-mode memory storage — existing memory-\
             store data written under the 768-dim mock embedder \
             becomes geometrically incompatible with new mock-mode \
             reads, covenant-memory search_similar produces near-zero \
             relevance for stored vectors, and the only signal an \
             operator gets is the empty-result UX; pick_embedder's \
             auto-detect fallback at line 644 also hardcodes 768, so a \
             divergence between the two hardcoded sites would split \
             memory geometry based on whether [embed] provider=\"mock\" \
             is present in secrets.toml vs the section being absent",
        );
    }

    #[test]
    fn embedder_config_serde_pins_optional_embed_section_default() {
        // EmbedderConfig is the top-level secrets.toml decoder for the
        // [embed] section. The struct carries a single field — embed:
        // Option<EmbedSection> — with field-level #[serde(default)] and
        // a derived Default impl. EmbedderConfig::from_path returns
        // Self::default() when the file is missing, and pick_embedder
        // treats embed.is_none() as the fall-through to the Ollama or
        // MockEmbedder ladder.
        //
        // EmbedSection itself is pinned by
        // embed_section_serde_pins_required_provider_and_option_defaults
        // but no test pins the wrapper's field name, the None default
        // on an empty TOML payload, or the parse contract when [embed]
        // is omitted but other root keys exist. A refactor that renamed
        // EmbedderConfig::embed would silently make every operator's
        // secrets.toml decode into EmbedderConfig::default(),
        // pick_embedder would fall through to Ollama or MockEmbedder
        // with no parse signal, and memory-relevance live tests would
        // start flipping silently — exactly the failure mode the
        // embedder-pin-in-live-tests guidance was written to catch.
        let empty: EmbedderConfig = toml::from_str("").unwrap();
        assert!(
            empty.embed.is_none(),
            "EmbedderConfig must decode an empty TOML payload as \
             EmbedderConfig::default() with embed == None — \
             EmbedderConfig::from_path relies on this path for missing \
             secrets.toml, and pick_embedder relies on embed.is_none() \
             to fall through to the Ollama/MockEmbedder ladder"
        );

        // No [embed] section but other unknown root keys present — the
        // top-level deserializer must still produce embed == None
        // rather than rejecting the unknown key. The wrapper does not
        // carry #[serde(deny_unknown_fields)] and co-resident [llm]/
        // [search] blocks live in the same secrets.toml.
        let other_root = r#"
[llm]
provider = "anthropic"
api_key = "sk"
"#;
        let cfg: EmbedderConfig = toml::from_str(other_root).unwrap();
        assert!(
            cfg.embed.is_none(),
            "EmbedderConfig must tolerate unknown root sections without \
             refusing to parse — operators co-locate [llm]/[search] \
             blocks in the same secrets.toml and a refactor that added \
             #[serde(deny_unknown_fields)] would break every co-resident \
             secrets.toml at daemon start"
        );

        // [embed] fully populated — wrapper round-trips into the
        // EmbedSection. Pin every inner field so a refactor that broke
        // the wrapper's deserialization path would fail this assertion
        // rather than landing on a misleading inner-field error.
        let full = r#"
[embed]
provider = "ollama"
model = "nomic-embed-text"
endpoint = "http://localhost:11434"
"#;
        let cfg: EmbedderConfig = toml::from_str(full).unwrap();
        let embed = cfg
            .embed
            .as_ref()
            .expect("[embed] block must surface through EmbedderConfig.embed");
        assert_eq!(embed.provider, "ollama");
        assert_eq!(embed.model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(embed.endpoint.as_deref(), Some("http://localhost:11434"));

        // [embed] with only the strictly-required provider field — the
        // inner section's Option defaults must surface through the
        // wrapper so partial secrets.toml stays forward-compatible.
        let minimal = r#"
[embed]
provider = "ollama"
"#;
        let cfg: EmbedderConfig = toml::from_str(minimal).unwrap();
        let embed = cfg.embed.as_ref().unwrap();
        assert_eq!(embed.provider, "ollama");
        assert!(embed.model.is_none());
        assert!(embed.endpoint.is_none());

        // EmbedderConfig::default() must match the empty-TOML decode —
        // pin this so a refactor that diverged the derived Default impl
        // from the empty-TOML path would fail loud.
        let default_cfg = EmbedderConfig::default();
        assert!(
            default_cfg.embed.is_none(),
            "EmbedderConfig::default() must have embed == None to match \
             the empty-TOML decode path that EmbedderConfig::from_path \
             relies on for missing secrets.toml"
        );
    }

    #[test]
    fn embed_section_serde_pins_required_provider_and_option_defaults() {
        // EmbedSection is the [embed] block operators write to
        // secrets.toml to pick the embedding provider. Mirrors
        // LlmSection's asymmetric contract: provider is strictly
        // required (no serde default) while model and endpoint each
        // carry #[serde(default)] so a stale or minimal secrets.toml
        // stays forward-compatible.
        //
        // The existing embedder_config_parses_ollama_block test
        // exercises a happy path but no test pins the strict-required-
        // provider contract: a refactor that added #[serde(default)] to
        // provider would silently parse blocks with no provider as
        // provider='', embedder_from_config would return None,
        // pick_embedder would fall back to MockEmbedder, and operator-
        // driven memory retrieval would silently use the deterministic
        // FNV hash stub instead of the configured provider's real
        // embeddings — semantic search would degrade silently with no
        // error or breadcrumb.
        let full = r#"
[embed]
provider = "ollama"
model = "nomic-embed-text"
endpoint = "http://localhost:11434"
"#;
        let cfg: EmbedderConfig = toml::from_str(full).unwrap();
        let embed = cfg
            .embed
            .as_ref()
            .expect("[embed] block must surface as EmbedSection");
        assert_eq!(embed.provider, "ollama");
        assert_eq!(embed.model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(embed.endpoint.as_deref(), Some("http://localhost:11434"));

        let minimal = r#"
[embed]
provider = "ollama"
"#;
        let cfg: EmbedderConfig = toml::from_str(minimal).unwrap();
        let embed = cfg.embed.as_ref().unwrap();
        assert_eq!(embed.provider, "ollama");
        assert!(
            embed.model.is_none(),
            "EmbedSection::model must decode as None when omitted; a \
             refactor that dropped #[serde(default)] would silently fail \
             parse for every operator deployment relying on the default \
             'nomic-embed-text' model"
        );
        assert!(
            embed.endpoint.is_none(),
            "EmbedSection::endpoint must decode as None when omitted; a \
             refactor that dropped #[serde(default)] would silently fail \
             parse for every operator deployment relying on the default \
             Ollama endpoint"
        );

        // Each Option field's omission must be tolerated independently.
        let no_model = r#"
[embed]
provider = "ollama"
endpoint = "http://localhost:11434"
"#;
        let cfg: EmbedderConfig = toml::from_str(no_model).unwrap();
        let embed = cfg.embed.as_ref().unwrap();
        assert!(embed.model.is_none());
        assert_eq!(embed.endpoint.as_deref(), Some("http://localhost:11434"));

        let no_endpoint = r#"
[embed]
provider = "ollama"
model = "nomic-embed-text"
"#;
        let cfg: EmbedderConfig = toml::from_str(no_endpoint).unwrap();
        let embed = cfg.embed.as_ref().unwrap();
        assert!(embed.endpoint.is_none());
        assert_eq!(embed.model.as_deref(), Some("nomic-embed-text"));

        // Provider is the only strictly-required field; omitting it must
        // fail parse so a future #[serde(default)] regression on
        // provider does not silently fall back to MockEmbedder and
        // degrade semantic search invisibly.
        let no_provider = r#"
[embed]
model = "nomic-embed-text"
"#;
        assert!(
            toml::from_str::<EmbedderConfig>(no_provider).is_err(),
            "EmbedSection::provider must remain strictly required; a \
             #[serde(default)] regression would silently parse blocks \
             with no provider as provider='' and pick_embedder would fall \
             back to MockEmbedder — operator memory retrieval would \
             silently use deterministic 768-dim stub embeddings instead \
             of the configured provider"
        );
    }

    #[test]
    fn provider_error_from_wrappers_display_messages_pin_prefixes_and_external_source_display_delegation(
    ) {
        // Pins the three directly-constructible #[from] wrappers (Io,
        // Serde, Toml). ProviderError::Http wraps reqwest::Error which
        // has no public constructor and can only be produced via an
        // actual transport invocation — pinning it would require either
        // adding a reqwest-internals test dep or running a network
        // probe, both of which exceed the scope of a Display-message
        // pin. The Http prefix is documented; if a reqwest fixture path
        // becomes available in a future reqwest release, this pin
        // should be extended.

        let io_err = ProviderError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "llm.toml missing",
        ));
        let io_message = format!("{io_err}");
        assert!(
            io_message.starts_with("io: "),
            "ProviderError::Io must surface the literal 'io: ' bootstrap-stage prefix so audit-log filters can distinguish provider IO faults from HTTP, JSON-decode, and TOML-parse faults (dropped-prefix regression class): {io_message}"
        );
        assert!(
            io_message.contains("llm.toml missing"),
            "ProviderError::Io must surface the inner std::io::Error Display rendering after the colon (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );
        assert!(
            !io_message.contains("Custom {") && !io_message.contains("Os {"),
            "ProviderError::Io must NOT surface the std::io::Error Debug rendering (Debug-vs-Display formatting regression class on the {{0}} interpolation): {io_message}"
        );

        let serde_source =
            serde_json::from_str::<serde_json::Value>("not json").expect_err("parse must fail");
        let serde_err = ProviderError::Serde(serde_source);
        let serde_message = format!("{serde_err}");
        assert!(
            serde_message.starts_with("serde: "),
            "ProviderError::Serde must surface the literal 'serde: ' bootstrap-stage prefix (dropped-prefix regression class): {serde_message}"
        );
        assert!(
            serde_message.contains("expected"),
            "ProviderError::Serde must surface the inner serde_json::Error Display rendering after the colon (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );
        assert!(
            !serde_message.contains("Error("),
            "ProviderError::Serde must NOT surface the serde_json::Error Debug rendering (Debug-vs-Display formatting regression class on the {{0}} interpolation): {serde_message}"
        );

        let toml_source =
            toml::from_str::<toml::Value>("= invalid =").expect_err("parse must fail");
        let toml_err = ProviderError::Toml(toml_source);
        let toml_message = format!("{toml_err}");
        assert!(
            toml_message.starts_with("toml: "),
            "ProviderError::Toml must surface the literal 'toml: ' bootstrap-stage prefix so audit-log filters can distinguish llm-config-toml-parse faults from HTTP, IO, and JSON-decode faults (dropped-prefix regression class): {toml_message}"
        );
        assert!(
            toml_message.len() > "toml: ".len(),
            "ProviderError::Toml must surface the inner toml::de::Error Display rendering after the colon (dropped-source-rendering regression class on the {{0}} interpolation): {toml_message}"
        );

        assert_ne!(
            io_message, serde_message,
            "ProviderError::Io and ProviderError::Serde Display must not converge (prefix-convergence regression class): io={io_message} serde={serde_message}"
        );
        assert_ne!(
            io_message, toml_message,
            "ProviderError::Io and ProviderError::Toml Display must not converge (prefix-convergence regression class): io={io_message} toml={toml_message}"
        );
        assert_ne!(
            serde_message, toml_message,
            "ProviderError::Serde and ProviderError::Toml Display must not converge (prefix-convergence regression class): serde={serde_message} toml={toml_message}"
        );
        for (name, message) in [
            ("Io", io_message.as_str()),
            ("Serde", serde_message.as_str()),
            ("Toml", toml_message.as_str()),
        ] {
            assert!(
                !message.starts_with("http: "),
                "ProviderError::{name} must not start with 'http: '; a sibling-prefix swap toward the Http wrapper would silently mis-route incident triage (sibling-prefix-swap regression class): {message}"
            );
            assert!(
                !message.starts_with("provider returned no content")
                    && !message.starts_with("provider error (")
                    && !message.starts_with("missing api key for provider"),
                "ProviderError::{name} must not converge with the three pinned string-variant surfaces (Empty, Status, MissingKey); a wrapper Display refactor must not collapse onto the operator-facing literal strings (string-surface-convergence regression class): {message}"
            );
        }
    }

    #[test]
    fn provider_error_io_source_delegation_pin_returns_inner_std_io_error_via_std_error_source() {
        use std::error::Error;

        let inner = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "secrets.toml: read denied",
        );
        let expected_display = format!("{inner}");
        let err = ProviderError::Io(inner);
        let source = err.source().expect(
            "covenant_llm::ProviderError::Io must surface the inner std::io::Error via std::error::Error::source so provider retry-policy classifiers can walk the error chain and downcast source() to std::io::Error to extract io::ErrorKind for distinct retry decisions on provider config/secrets IO (NotFound stops provider dispatch with operator-attention, PermissionDenied escalates, Interrupted retries immediately); a refactor that converted the variant from #[from] to a hand-written Error impl returning None (under a 'simpler error wrapping' rationale) would silently change source() to return None while leaving Display intact (dropped-source-attribute regression class)",
        );
        assert_eq!(
            format!("{source}"),
            expected_display,
            "covenant_llm::ProviderError::Io source() Display must match a direct format!() of the same std::io::Error verbatim; a refactor that swapped the inner field type to Box<dyn Error + Send + Sync> or any other wrapper would silently break daemon-side downcasts even though the wrapper's Display would continue to flow through {{0}} (concrete-source-type regression class)"
        );
        let kind = source.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "covenant_llm::ProviderError::Io source() must downcast_ref to std::io::Error so provider retry-policy classifiers can extract io::ErrorKind for retry decisions on provider config/secrets IO; a refactor that wrapped the inner in a project-local newtype (e.g., ProviderIoError(std::io::Error) under a 'tag provider IO failures distinctly from sibling Io variants in other crates' rationale) would silently break downcast_ref::<std::io::Error>() at every downstream callsite that classifies provider IO faults (concrete-source-type downcast regression class)"
        );
    }
}
