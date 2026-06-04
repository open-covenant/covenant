//! AceData generative tools.
//!
//! Three native tools — image, music, search — each a thin marshal of
//! arguments into an AceData request plus a [`Provenance`] record on the
//! way out. Every tool result carries two JSON blocks: the raw API
//! response, then `{ "provenance": { ... } }`. The tools share one
//! [`AceDataClient`]; the daemon builds it from config and registers
//! whichever tools the allowlist permits.
//!
//! A tool never holds a funding key — the API key lives in the client
//! and is a billing credential, exactly as a normal API client holds it.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::client::AceDataClient;
use crate::config::AceDataConfig;
use crate::provenance::Provenance;

pub use crate::provenance::PROVIDER;

/// Tool name for image generation (Flux).
pub const IMAGE_TOOL: &str = "acedata.image.generate";
/// Tool name for music generation (Suno).
pub const MUSIC_TOOL: &str = "acedata.music.generate";
/// Tool name for Google search (SERP).
pub const SEARCH_TOOL: &str = "acedata.search";

/// Build the AceData tool set permitted by `cfg.allow`, sharing one
/// client. Returns an empty vec when the config is disabled.
pub fn acedata_tools(client: Arc<AceDataClient>, cfg: &AceDataConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if cfg.allows(IMAGE_TOOL) {
        tools.push(Arc::new(ImageTool {
            client: client.clone(),
            default_model: cfg.image_model.clone(),
        }));
    }
    if cfg.allows(MUSIC_TOOL) {
        tools.push(Arc::new(MusicTool {
            client: client.clone(),
            default_model: cfg.music_model.clone(),
        }));
    }
    if cfg.allows(SEARCH_TOOL) {
        tools.push(Arc::new(SearchTool { client }));
    }
    tools
}

/// Extract a required, non-empty string argument.
fn require_str(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidArguments(format!("missing string `{key}`")))
}

/// Two-block result: raw response, then the provenance record.
fn ok_with_provenance(result: Value, prov: Provenance) -> ToolCallResult {
    ToolCallResult::ok(vec![
        Content::json(result),
        Content::json(json!({ "provenance": prov.to_json() })),
    ])
}

struct ImageTool {
    client: Arc<AceDataClient>,
    default_model: String,
}

#[async_trait]
impl Tool for ImageTool {
    fn name(&self) -> &str {
        IMAGE_TOOL
    }
    fn description(&self) -> &str {
        "Generate an image with AceData (Flux). Returns the asset URL and a provenance record."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "Image description." },
                "model": { "type": "string", "description": "Flux model; defaults to the configured model." },
                "size": { "type": "string", "description": "e.g. 1024x1024, 1792x1024, or an aspect ratio like 16:9." }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let prompt = require_str(&arguments, "prompt")?;
        let model = arguments
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&self.default_model)
            .to_string();
        let size = arguments
            .get("size")
            .and_then(Value::as_str)
            .unwrap_or("1024x1024");
        let body = json!({ "model": model, "prompt": prompt, "size": size });
        match self.client.post("/flux/images", body).await {
            Ok(resp) => {
                let prov = Provenance::from_response(IMAGE_TOOL, &model, &prompt, &resp);
                Ok(ok_with_provenance(resp, prov))
            }
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

struct MusicTool {
    client: Arc<AceDataClient>,
    default_model: String,
}

#[async_trait]
impl Tool for MusicTool {
    fn name(&self) -> &str {
        MUSIC_TOOL
    }
    fn description(&self) -> &str {
        "Generate music with AceData (Suno). Returns the audio asset URL and a provenance record."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "Song description or lyric theme." },
                "model": { "type": "string", "description": "Suno model; defaults to the configured model." }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let prompt = require_str(&arguments, "prompt")?;
        let model = arguments
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&self.default_model)
            .to_string();
        let body = json!({ "action": "generate", "prompt": prompt, "model": model });
        match self.client.post("/suno/audios", body).await {
            Ok(resp) => {
                let prov = Provenance::from_response(MUSIC_TOOL, &model, &prompt, &resp);
                Ok(ok_with_provenance(resp, prov))
            }
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

struct SearchTool {
    client: Arc<AceDataClient>,
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        SEARCH_TOOL
    }
    fn description(&self) -> &str {
        "Search Google via AceData. Returns organic results and a provenance record."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query (Google syntax supported)." },
                "type": { "type": "string", "description": "search | images | news (default: search)." }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }
    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let query = require_str(&arguments, "query")?;
        let search_type = arguments
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("search");
        let body = json!({ "query": query, "type": search_type });
        match self.client.post("/serp/google", body).await {
            Ok(resp) => {
                let prov = Provenance::from_response(SEARCH_TOOL, "", &query, &resp);
                Ok(ok_with_provenance(resp, prov))
            }
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn enabled_cfg(base: &str) -> AceDataConfig {
        AceDataConfig {
            enabled: true,
            base_url: base.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn disabled_config_registers_no_tools() {
        let client = Arc::new(AceDataClient::new("http://x", "k"));
        let cfg = AceDataConfig::default();
        assert!(acedata_tools(client, &cfg).is_empty());
    }

    #[test]
    fn enabled_config_registers_all_three_tools() {
        let client = Arc::new(AceDataClient::new("http://x", "k"));
        let cfg = enabled_cfg("http://x");
        let mut names: Vec<String> = acedata_tools(client, &cfg)
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                IMAGE_TOOL.to_string(),
                MUSIC_TOOL.to_string(),
                SEARCH_TOOL.to_string()
            ]
        );
    }

    #[test]
    fn allowlist_filters_registered_tools() {
        let client = Arc::new(AceDataClient::new("http://x", "k"));
        let mut cfg = enabled_cfg("http://x");
        cfg.allow = Some(vec![SEARCH_TOOL.to_string()]);
        let names: Vec<String> = acedata_tools(client, &cfg)
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(names, vec![SEARCH_TOOL.to_string()]);
    }

    #[tokio::test]
    async fn search_missing_query_is_invalid_arguments() {
        let client = Arc::new(AceDataClient::new("http://x", "k"));
        let tool = SearchTool { client };
        let err = tool.call(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn search_returns_results_then_provenance() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/serp/google"))
            .and(body_partial_json(json!({ "query": "covenant", "type": "search" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "organic": [{ "title": "t", "link": "https://e.com", "position": 1 }]
            })))
            .mount(&server)
            .await;

        let client = Arc::new(AceDataClient::new(server.uri(), "k"));
        let tool = SearchTool { client };
        let res = tool.call(json!({ "query": "covenant" })).await.unwrap();
        assert!(!res.is_error);
        assert_eq!(res.content.len(), 2);
        match &res.content[1] {
            Content::Json { value } => {
                let p = value.get("provenance").expect("provenance block");
                assert_eq!(p.get("provider").and_then(Value::as_str), Some("acedata"));
                assert_eq!(p.get("tool").and_then(Value::as_str), Some(SEARCH_TOOL));
                assert_eq!(
                    p.get("prompt_sha256").and_then(Value::as_str).map(str::len),
                    Some(64)
                );
                assert_eq!(
                    p.get("output_sha256").and_then(Value::as_str).map(str::len),
                    Some(64)
                );
            }
            other => panic!("expected provenance json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn image_api_error_becomes_error_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/flux/images"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": false,
                "error": { "code": "bad_request", "message": "size is required" }
            })))
            .mount(&server)
            .await;

        let client = Arc::new(AceDataClient::new(server.uri(), "k"));
        let tool = ImageTool {
            client,
            default_model: "flux-pro".into(),
        };
        let res = tool.call(json!({ "prompt": "a leaf" })).await.unwrap();
        assert!(res.is_error);
    }

    #[tokio::test]
    async fn image_succeeds_with_asset_and_provenance() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/flux/images"))
            .and(body_partial_json(json!({ "model": "flux-pro", "size": "1024x1024" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "task_id": "t-1",
                "data": [{ "image_url": "https://cdn/x.png" }]
            })))
            .mount(&server)
            .await;

        let client = Arc::new(AceDataClient::new(server.uri(), "k"));
        let tool = ImageTool {
            client,
            default_model: "flux-pro".into(),
        };
        let res = tool.call(json!({ "prompt": "a leaf" })).await.unwrap();
        assert!(!res.is_error);
        match &res.content[1] {
            Content::Json { value } => {
                let p = value.get("provenance").unwrap();
                assert_eq!(p.get("model").and_then(Value::as_str), Some("flux-pro"));
                assert_eq!(p.get("task_id").and_then(Value::as_str), Some("t-1"));
                let assets = p.get("assets").and_then(Value::as_array).unwrap();
                assert_eq!(assets[0].as_str(), Some("https://cdn/x.png"));
            }
            other => panic!("expected provenance json, got {other:?}"),
        }
    }
}
