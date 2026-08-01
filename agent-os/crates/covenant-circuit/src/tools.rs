//! Circuit as native Covenant tools.
//!
//! Each tool is a thin marshal of arguments into an x402-paid Circuit call, plus a
//! `circuit` result block carrying the reported settling signature and CIRC spent. These
//! are explicit library surfaces; covenantd neither advertises nor invokes them. A tool
//! never holds a funding key itself — payment goes through the
//! [`CircPayer`](crate::CircPayer) baked into the shared clients.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::circ;
use crate::config::CircuitConfig;
use crate::data::DataClient;
use crate::inference::{ChatMessage, ChatParams, Inference};

pub const INFERENCE_TOOL: &str = "circuit.inference";
pub const DATA_QUERY_TOOL: &str = "circuit.data.query";
pub const TOKEN_PRICE_TOOL: &str = "circuit.data.token_price";
pub const MARKET_OVERVIEW_TOOL: &str = "circuit.data.market_overview";

type CircuitOutput = (Value, Option<String>, Option<u64>, Option<String>);

/// Build an explicit Circuit tool set permitted by `cfg.allow`, over shared inference +
/// data clients. Empty when the config is disabled. This does not register the tools with
/// covenantd.
pub fn circuit_tools(
    inference: Arc<Inference>,
    data: Arc<DataClient>,
    cfg: &CircuitConfig,
) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if cfg.allows(INFERENCE_TOOL) {
        tools.push(Arc::new(InferenceTool {
            inference: inference.clone(),
        }));
    }
    if cfg.allows(DATA_QUERY_TOOL) {
        tools.push(Arc::new(DataQueryTool { data: data.clone() }));
    }
    if cfg.allows(TOKEN_PRICE_TOOL) {
        tools.push(Arc::new(TokenPriceTool { data: data.clone() }));
    }
    if cfg.allows(MARKET_OVERVIEW_TOOL) {
        tools.push(Arc::new(MarketOverviewTool { data }));
    }
    tools
}

fn require_str(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidArguments(format!("missing string `{key}`")))
}

/// The `circuit` provenance block appended to every tool result.
fn payment_block(
    endpoint: &str,
    payment_tx: Option<&str>,
    spent_raw: Option<u64>,
    token: Option<&str>,
) -> Value {
    json!({ "circuit": {
        "endpoint": endpoint,
        "paymentTx": payment_tx,
        "spentRaw": spent_raw,
        "token": token.unwrap_or(circ::MINT),
    }})
}

/// A capability rejection is a result the agent should see, not a transport failure; a
/// real network break is a hard tool error.
fn to_result(endpoint: &str, r: crate::Result<CircuitOutput>) -> ToolCallResult {
    match r {
        Ok((body, tx, spent, token)) => ToolCallResult::ok(vec![
            Content::json(body),
            Content::json(payment_block(
                endpoint,
                tx.as_deref(),
                spent,
                token.as_deref(),
            )),
        ]),
        Err(e) => ToolCallResult::error(e.to_string()),
    }
}

struct InferenceTool {
    inference: Arc<Inference>,
}

#[async_trait]
impl Tool for InferenceTool {
    fn name(&self) -> &str {
        INFERENCE_TOOL
    }
    fn description(&self) -> &str {
        "Ask the Circuit decentralized 72B (OpenAI-compatible), paid per call in CIRC. Pass `messages` or a single `prompt`."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "A single user message. Ignored if `messages` is given." },
                "messages": {
                    "type": "array",
                    "description": "OpenAI-style chat messages.",
                    "items": {
                        "type": "object",
                        "properties": { "role": { "type": "string" }, "content": { "type": "string" } },
                        "required": ["role", "content"]
                    }
                },
                "model": { "type": "string" },
                "max_tokens": { "type": "integer" },
                "temperature": { "type": "number" }
            },
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let messages: Vec<ChatMessage> =
            if let Some(arr) = args.get("messages").and_then(Value::as_array) {
                arr.iter()
                    .filter_map(|m| {
                        Some(ChatMessage {
                            role: m.get("role")?.as_str()?.to_string(),
                            content: m.get("content")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            } else if let Some(prompt) = args.get("prompt").and_then(Value::as_str) {
                vec![ChatMessage::user(prompt)]
            } else {
                return Err(ToolError::InvalidArguments(
                    "provide `messages` (array) or `prompt` (string)".into(),
                ));
            };
        if messages.is_empty() {
            return Err(ToolError::InvalidArguments("no messages".into()));
        }

        let params = ChatParams {
            messages,
            model: args.get("model").and_then(Value::as_str).map(String::from),
            max_tokens: args
                .get("max_tokens")
                .and_then(Value::as_u64)
                .map(|n| n as u32),
            temperature: args
                .get("temperature")
                .and_then(Value::as_f64)
                .map(|n| n as f32),
        };

        let out = self.inference.chat(params).await.map(|r| {
            let body = json!({ "content": r.content, "usage": r.usage });
            (body, r.payment_tx, r.paid_raw, r.token)
        });
        Ok(to_result(INFERENCE_TOOL, out))
    }
}

struct DataQueryTool {
    data: Arc<DataClient>,
}

#[async_trait]
impl Tool for DataQueryTool {
    fn name(&self) -> &str {
        DATA_QUERY_TOOL
    }
    fn description(&self) -> &str {
        "Call any Circuit Data API endpoint by path, paid per call in CIRC. e.g. path `/api/token-price`, query `{\"mint\":\"...\"}`."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Endpoint path, e.g. /api/market-overview." },
                "query": { "type": "object", "description": "Query params (string/number/bool values)." }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let path = require_str(&args, "path")?;
        if !path.starts_with("/api/") {
            return Err(ToolError::InvalidArguments(
                "path must start with /api/".into(),
            ));
        }
        let query: Vec<(&str, String)> = args
            .get("query")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.as_str(), qs_value(v)?)))
                    .collect()
            })
            .unwrap_or_default();

        let out = self.data.get_paid(&path, &query).await.map(|p| {
            let (spent, token) = p.quote.map(|q| (q.amount_raw, q.token)).unzip();
            (p.body, p.payment_tx, spent, token)
        });
        Ok(to_result(DATA_QUERY_TOOL, out))
    }
}

struct TokenPriceTool {
    data: Arc<DataClient>,
}

#[async_trait]
impl Tool for TokenPriceTool {
    fn name(&self) -> &str {
        TOKEN_PRICE_TOOL
    }
    fn description(&self) -> &str {
        "Get the aggregated price for a Solana token mint from Circuit, paid in CIRC."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "mint": { "type": "string", "description": "SPL token mint address." } },
            "required": ["mint"],
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let mint = require_str(&args, "mint")?;
        let out = self
            .data
            .get_paid("/api/token-price", &[("mint", mint)])
            .await
            .map(|p| {
                let (spent, token) = p.quote.map(|q| (q.amount_raw, q.token)).unzip();
                (p.body, p.payment_tx, spent, token)
            });
        Ok(to_result(TOKEN_PRICE_TOOL, out))
    }
}

struct MarketOverviewTool {
    data: Arc<DataClient>,
}

#[async_trait]
impl Tool for MarketOverviewTool {
    fn name(&self) -> &str {
        MARKET_OVERVIEW_TOOL
    }
    fn description(&self) -> &str {
        "Get the Circuit market overview, paid in CIRC."
    }
    async fn call(&self, _args: Value) -> Result<ToolCallResult, ToolError> {
        let out = self
            .data
            .get_paid("/api/market-overview", &[])
            .await
            .map(|p| {
                let (spent, token) = p.quote.map(|q| (q.amount_raw, q.token)).unzip();
                (p.body, p.payment_tx, spent, token)
            });
        Ok(to_result(MARKET_OVERVIEW_TOOL, out))
    }
}

fn qs_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
