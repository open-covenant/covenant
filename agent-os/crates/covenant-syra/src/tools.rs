//! MCP tools for Syra's market-intelligence endpoints.
//!
//! A curated catalog ([`ENDPOINTS`]) becomes one paid tool each. A tool
//! never signs or holds keys: it marshals args into a [`PaidRequest`] and
//! calls a [`SyraExecutor`] the daemon supplies (x402 loop + signer
//! sidecar + accounting). Param names come from each endpoint's live
//! Bazaar discovery schema.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use serde_json::{Map, Value};

use crate::config::SyraConfig;

/// Provider tag for audit.
pub const PROVIDER: &str = "syra";

/// One query parameter on a Syra endpoint.
struct Param {
    name: &'static str,
    required: bool,
    description: &'static str,
}

/// A curated Syra endpoint exposed as a paid tool.
struct Endpoint {
    /// Tool name suffix (`syra.<tool>`).
    tool: &'static str,
    /// URL path under the base (`/signal`, `/nansen/smart-money/netflow`).
    path: &'static str,
    description: &'static str,
    /// Published price in USD, for the tool description.
    price_usd: &'static str,
    params: &'static [Param],
}

/// The curated Phase-0 catalog. Params mirror Syra's live Bazaar schema.
const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        tool: "signal",
        path: "/signal",
        description: "Trading signal from OHLC candles for a token",
        price_usd: "0.10",
        params: &[
            Param {
                name: "token",
                required: false,
                description: "Token name (e.g. solana, bitcoin)",
            },
            Param {
                name: "source",
                required: false,
                description: "Venue (default binance)",
            },
            Param {
                name: "instId",
                required: false,
                description: "Override instrument, e.g. BTCUSDT",
            },
            Param {
                name: "bar",
                required: false,
                description: "Candle interval, e.g. 1m, 1h, 1d",
            },
            Param {
                name: "limit",
                required: false,
                description: "Number of candles (default 200)",
            },
        ],
    },
    Endpoint {
        tool: "news",
        path: "/news",
        description: "Latest crypto news and market updates",
        price_usd: "0.10",
        params: &[Param {
            name: "ticker",
            required: false,
            description: "Ticker (e.g. BTC, ETH) or 'general'",
        }],
    },
    Endpoint {
        tool: "sentiment",
        path: "/sentiment",
        description: "Market sentiment for a ticker",
        price_usd: "0.10",
        params: &[Param {
            name: "ticker",
            required: false,
            description: "Ticker or 'general'",
        }],
    },
    Endpoint {
        tool: "brain",
        path: "/brain",
        description: "Single-call AI market brain: ask a natural-language question",
        price_usd: "0.10",
        params: &[Param {
            name: "question",
            required: true,
            description: "Natural-language question (e.g. latest BTC news)",
        }],
    },
    Endpoint {
        tool: "smart_money",
        path: "/nansen/smart-money/netflow",
        description: "Nansen smart-money netflow",
        price_usd: "0.10",
        params: &[],
    },
];

/// A resolved paid call the daemon executes and accounts for.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidRequest {
    pub provider: String,
    /// Endpoint slug for audit (`signal`, `nansen/smart-money/netflow`).
    pub slug: String,
    /// Fully resolved URL with the query string appended.
    pub url: String,
    pub method: String,
    pub body: Option<Value>,
    /// CAIP-2 settlement network.
    pub network: String,
    /// Payment asset (USDC mint).
    pub asset: String,
    /// Per-call ceiling in atomic USDC.
    pub per_call_cap: u128,
}

/// Outcome of a paid call.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidResponse {
    pub status: u16,
    pub body: Value,
    pub receipt_id: Option<String>,
}

/// The daemon's paid-call seam. Implemented over the x402 loop + signer
/// sidecar + settlement accounting; mocked in this crate's tests.
#[async_trait]
pub trait SyraExecutor: Send + Sync {
    async fn execute(&self, req: PaidRequest) -> std::result::Result<PaidResponse, String>;
}

struct SyraEndpointTool {
    name: String,
    description: String,
    path: &'static str,
    slug: String,
    params: &'static [Param],
    base_url: String,
    network: String,
    asset: String,
    per_call_cap: u128,
    executor: Arc<dyn SyraExecutor>,
}

#[async_trait]
impl Tool for SyraEndpointTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> Value {
        let mut props = Map::new();
        let mut required = Vec::new();
        for p in self.params {
            props.insert(
                p.name.to_string(),
                serde_json::json!({ "type": "string", "description": p.description }),
            );
            if p.required {
                required.push(Value::String(p.name.to_string()));
            }
        }
        serde_json::json!({
            "type": "object",
            "properties": Value::Object(props),
            "required": required,
            "additionalProperties": false,
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let empty = Map::new();
        let args = match &arguments {
            Value::Object(m) => m,
            Value::Null => &empty,
            _ => {
                return Err(ToolError::InvalidArguments(
                    "arguments must be a JSON object".into(),
                ))
            }
        };

        let mut query: Vec<(String, String)> = Vec::new();
        for p in self.params {
            match arg_str(args, p.name) {
                Some(v) => query.push((p.name.to_string(), v)),
                None if p.required => {
                    return Err(ToolError::InvalidArguments(format!(
                        "missing required parameter {:?}",
                        p.name
                    )));
                }
                None => {}
            }
        }

        let mut url = format!("{}{}", self.base_url.trim_end_matches('/'), self.path);
        if !query.is_empty() {
            url.push('?');
            url.push_str(
                &query
                    .iter()
                    .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }

        let req = PaidRequest {
            provider: PROVIDER.into(),
            slug: self.slug.clone(),
            url,
            method: "GET".into(),
            body: None,
            network: self.network.clone(),
            asset: self.asset.clone(),
            per_call_cap: self.per_call_cap,
        };

        match self.executor.execute(req).await {
            Ok(resp) if (200..300).contains(&resp.status) => {
                let mut content = vec![Content::json(resp.body)];
                if let Some(rid) = resp.receipt_id {
                    content.push(Content::text(format!("x402 settled; receipt {rid}")));
                }
                Ok(ToolCallResult::ok(content))
            }
            Ok(resp) => Ok(ToolCallResult::error(format!(
                "syra {} returned status {}",
                self.slug, resp.status
            ))),
            Err(e) => Ok(ToolCallResult::error(format!("syra paid call failed: {e}"))),
        }
    }
}

fn build_tool(
    ep: &'static Endpoint,
    config: &SyraConfig,
    executor: Arc<dyn SyraExecutor>,
) -> Arc<dyn Tool> {
    Arc::new(SyraEndpointTool {
        name: format!("syra.{}", ep.tool),
        description: format!("{} (paid: ${} USDC via x402)", ep.description, ep.price_usd),
        path: ep.path,
        slug: ep.path.trim_start_matches('/').to_string(),
        params: ep.params,
        base_url: config.base_url.clone(),
        network: config.network.clone(),
        asset: config.asset.clone(),
        per_call_cap: config.per_call_cap,
        executor,
    })
}

/// Build the Syra tool set wired to `executor`. Empty when disabled.
pub fn syra_tools(config: &SyraConfig, executor: Arc<dyn SyraExecutor>) -> Vec<Arc<dyn Tool>> {
    if !config.enabled {
        return Vec::new();
    }
    ENDPOINTS
        .iter()
        .map(|ep| build_tool(ep, config, executor.clone()))
        .collect()
}

/// Build just the named tool, or `None` if no endpoint maps to it.
pub fn syra_tool(
    config: &SyraConfig,
    name: &str,
    executor: Arc<dyn SyraExecutor>,
) -> Option<Arc<dyn Tool>> {
    let mut cfg = config.clone();
    cfg.enabled = true;
    syra_tools(&cfg, executor)
        .into_iter()
        .find(|t| t.name() == name)
}

/// Tool specs for discovery without binding a payer.
pub fn syra_specs(config: &SyraConfig) -> Vec<ToolSpec> {
    let mut cfg = config.clone();
    cfg.enabled = true;
    syra_tools(&cfg, Arc::new(SpecOnly))
        .iter()
        .map(|t| t.spec())
        .collect()
}

struct SpecOnly;

#[async_trait]
impl SyraExecutor for SpecOnly {
    async fn execute(&self, _req: PaidRequest) -> std::result::Result<PaidResponse, String> {
        Err("spec-only executor cannot make calls".into())
    }
}

fn arg_str(args: &Map<String, Value>, key: &str) -> Option<String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

/// Percent-encode everything outside the unreserved set.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockExecutor {
        last: Mutex<Option<PaidRequest>>,
        status: u16,
    }

    #[async_trait]
    impl SyraExecutor for MockExecutor {
        async fn execute(&self, req: PaidRequest) -> Result<PaidResponse, String> {
            *self.last.lock().unwrap() = Some(req);
            Ok(PaidResponse {
                status: if self.status == 0 { 200 } else { self.status },
                body: serde_json::json!({ "ok": true, "paid": true }),
                receipt_id: Some("rcpt-1".into()),
            })
        }
    }

    fn enabled() -> SyraConfig {
        SyraConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn find<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
        tools
            .iter()
            .find(|t| t.name() == name)
            .expect("tool present")
    }

    #[test]
    fn disabled_registers_no_tools() {
        assert!(syra_tools(&SyraConfig::default(), Arc::new(MockExecutor::default())).is_empty());
    }

    #[test]
    fn enabled_registers_full_catalog() {
        let tools = syra_tools(&enabled(), Arc::new(MockExecutor::default()));
        assert_eq!(tools.len(), ENDPOINTS.len());
        for n in [
            "syra.signal",
            "syra.news",
            "syra.sentiment",
            "syra.brain",
            "syra.smart_money",
        ] {
            assert!(tools.iter().any(|t| t.name() == n), "missing {n}");
        }
    }

    #[test]
    fn brain_marks_question_required() {
        let tools = syra_tools(&enabled(), Arc::new(MockExecutor::default()));
        let schema = find(&tools, "syra.brain").input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "question"));
        assert_eq!(schema["additionalProperties"], Value::Bool(false));
    }

    #[tokio::test]
    async fn signal_builds_url_with_params() {
        let exec = Arc::new(MockExecutor::default());
        let tools = syra_tools(&enabled(), exec.clone());
        let res = find(&tools, "syra.signal")
            .call(serde_json::json!({ "token": "solana", "bar": "1h" }))
            .await
            .unwrap();
        assert!(!res.is_error);
        let req = exec.last.lock().unwrap().clone().unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.slug, "signal");
        assert!(req.url.starts_with("https://api.syraa.fun/signal?"));
        assert!(req.url.contains("token=solana"));
        assert!(req.url.contains("bar=1h"));
        assert_eq!(req.per_call_cap, 500_000);
    }

    #[tokio::test]
    async fn smart_money_has_no_query() {
        let exec = Arc::new(MockExecutor::default());
        let tools = syra_tools(&enabled(), exec.clone());
        find(&tools, "syra.smart_money")
            .call(serde_json::json!({}))
            .await
            .unwrap();
        let req = exec.last.lock().unwrap().clone().unwrap();
        assert_eq!(req.url, "https://api.syraa.fun/nansen/smart-money/netflow");
        assert_eq!(req.slug, "nansen/smart-money/netflow");
    }

    #[tokio::test]
    async fn brain_requires_question() {
        let tools = syra_tools(&enabled(), Arc::new(MockExecutor::default()));
        let err = find(&tools, "syra.brain")
            .call(serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn upstream_error_surfaces_as_tool_error() {
        let exec = Arc::new(MockExecutor {
            status: 502,
            ..Default::default()
        });
        let tools = syra_tools(&enabled(), exec);
        let res = find(&tools, "syra.news")
            .call(serde_json::json!({ "ticker": "BTC" }))
            .await
            .unwrap();
        assert!(res.is_error);
    }

    #[test]
    fn specs_list_full_catalog() {
        let specs = syra_specs(&SyraConfig::default());
        assert_eq!(specs.len(), ENDPOINTS.len());
    }
}
