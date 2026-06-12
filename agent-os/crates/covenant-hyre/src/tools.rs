//! Generate MCP tools from the catalog.
//!
//! Every catalog endpoint becomes one [`HyreEndpointTool`]; the 23
//! structured endpoints expose their parameters as a JSON Schema, and
//! `/ask` surfaces as the single high-level `hyre.ask` tool for agents
//! that don't want the structured surface — both fall out of the same
//! generator.
//!
//! A tool never makes a network call itself. It marshals arguments
//! into a [`PaidRequest`] and hands it to a [`PaidExecutor`] the daemon
//! supplies. The executor owns the x402 client, the funding-key signer
//! (a sidecar), and the budget/settlement/audit accounting. Keeping
//! that behind a trait is what lets this crate stay free of funding
//! keys and of the daemon's subsystem types.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{Map, Value};

use crate::catalog::HyreCatalog;
use crate::config::HyreConfig;
use crate::manifest::{Endpoint, ParamIn};

/// Provider tag carried on every Hyre paid call, for audit and catalog
/// disambiguation.
pub const PROVIDER: &str = "hyre";

/// A resolved paid call the daemon executes and accounts for.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidRequest {
    pub provider: String,
    /// Endpoint slug, for audit (`trenches/new-tokens`).
    pub slug: String,
    /// Fully resolved URL — path parameters substituted, query string
    /// appended.
    pub url: String,
    pub method: String,
    pub body: Option<Value>,
    /// CAIP-2 settlement network.
    pub network: String,
    /// Payment asset (USDC mint).
    pub asset: String,
    /// Per-call ceiling in atomic USDC.
    pub per_call_cap: u128,
    /// USD-pegged budget credits to debit.
    pub credits: u64,
    /// Published price in atomic USDC — the discovery-time figure, used
    /// to record settlement when the live 402 amount isn't surfaced.
    pub price_micro_usdc: u128,
}

/// Outcome of a paid call.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidResponse {
    pub status: u16,
    /// Upstream response body, parsed as JSON when possible.
    pub body: Value,
    /// Settlement receipt id joining budget, settlement, and audit
    /// logs. `None` when the upstream returned a non-success status.
    pub receipt_id: Option<String>,
}

/// The daemon's paid-call seam. Implemented over the x402 client +
/// signer sidecar + settlement accounting; mocked in this crate's
/// tests.
#[async_trait]
pub trait PaidExecutor: Send + Sync {
    async fn execute(&self, req: PaidRequest) -> std::result::Result<PaidResponse, String>;
}

struct HyreEndpointTool {
    name: String,
    description: String,
    endpoint: Endpoint,
    url_template: String,
    network: String,
    asset: String,
    per_call_cap: u128,
    executor: Arc<dyn PaidExecutor>,
}

impl HyreEndpointTool {
    fn schema(&self) -> Value {
        let mut props = Map::new();
        let mut required = Vec::new();
        for p in &self.endpoint.params {
            props.insert(p.name.clone(), field_schema(&p.description));
            if p.required {
                required.push(Value::String(p.name.clone()));
            }
        }
        for b in &self.endpoint.body {
            props.insert(b.name.clone(), field_schema(&b.description));
            if b.required {
                required.push(Value::String(b.name.clone()));
            }
        }
        serde_json::json!({
            "type": "object",
            "properties": Value::Object(props),
            "required": required,
            "additionalProperties": false,
        })
    }

    fn resolve(&self, args: &Map<String, Value>) -> Result<(String, Option<Value>), ToolError> {
        let mut url = self.url_template.clone();
        let mut query: Vec<(String, String)> = Vec::new();
        for p in &self.endpoint.params {
            let value = arg_str(args, &p.name);
            match (&p.location, value) {
                (ParamIn::Path, Some(v)) => {
                    url = url.replace(&format!("{{{}}}", p.name), &encode(&v));
                }
                (ParamIn::Path, None) if p.required => {
                    return Err(ToolError::InvalidArguments(format!(
                        "missing path parameter {:?}",
                        p.name
                    )));
                }
                (ParamIn::Query, Some(v)) => query.push((p.name.clone(), v)),
                _ => {}
            }
        }
        if !query.is_empty() {
            let q: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
                .collect();
            url.push('?');
            url.push_str(&q.join("&"));
        }

        // Only POST endpoints take a JSON body. Hyre publishes a
        // mirrored requestBody on its GET endpoints too, but those are
        // satisfied by path + query; sending a body there would also
        // double-bind a name shared with a path parameter.
        if self.endpoint.method != "POST" {
            return Ok((url, None));
        }

        let mut body = Map::new();
        for b in &self.endpoint.body {
            match arg_str(args, &b.name) {
                Some(v) => {
                    body.insert(b.name.clone(), Value::String(v));
                }
                None if b.required => {
                    return Err(ToolError::InvalidArguments(format!(
                        "missing body field {:?}",
                        b.name
                    )));
                }
                None => {}
            }
        }
        let body = (!body.is_empty()).then_some(Value::Object(body));
        Ok((url, body))
    }
}

#[async_trait]
impl Tool for HyreEndpointTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> Value {
        self.schema()
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
        let (url, body) = self.resolve(args)?;

        let req = PaidRequest {
            provider: PROVIDER.to_string(),
            slug: self.endpoint.slug(),
            url,
            method: self.endpoint.method.clone(),
            body,
            network: self.network.clone(),
            asset: self.asset.clone(),
            per_call_cap: self.per_call_cap,
            credits: self.endpoint.credits(),
            price_micro_usdc: self.endpoint.price_micro_usdc,
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
                "hyre {} returned status {}",
                self.endpoint.slug(),
                resp.status
            ))),
            Err(e) => Ok(ToolCallResult::error(format!("hyre paid call failed: {e}"))),
        }
    }
}

/// Build one tool from one endpoint, wired to `executor`. The per-call
/// cap comes from the config when set, otherwise the endpoint's
/// published price (so the gateway accepts an at-price 402).
fn build_tool(
    catalog: &HyreCatalog,
    config: &HyreConfig,
    ep: &Endpoint,
    executor: Arc<dyn PaidExecutor>,
) -> Arc<dyn Tool> {
    let cap = if config.per_call_cap > 0 {
        config.per_call_cap
    } else {
        ep.price_micro_usdc
    };
    let base = if ep.summary.is_empty() {
        ep.description.clone()
    } else {
        ep.summary.clone()
    };
    let price = ep.price_micro_usdc as f64 / 1_000_000.0;
    Arc::new(HyreEndpointTool {
        name: ep.tool_name(),
        description: format!("{base} (paid: ${price} USDC via x402)"),
        url_template: catalog.endpoint_url(ep),
        endpoint: ep.clone(),
        network: config.network.clone(),
        asset: config.asset.clone(),
        per_call_cap: cap,
        executor,
    })
}

/// Generate one tool per catalog endpoint, wired to `executor`.
pub fn hyre_tools(
    catalog: &HyreCatalog,
    config: &HyreConfig,
    executor: Arc<dyn PaidExecutor>,
) -> Vec<Arc<dyn Tool>> {
    catalog
        .endpoints()
        .iter()
        .map(|ep| build_tool(catalog, config, ep, executor.clone()))
        .collect()
}

/// Build just the named tool, or `None` if no endpoint maps to it.
/// Lets the daemon bind a payer-scoped executor per call without
/// materialising the whole tool set.
pub fn hyre_tool(
    catalog: &HyreCatalog,
    config: &HyreConfig,
    name: &str,
    executor: Arc<dyn PaidExecutor>,
) -> Option<Arc<dyn Tool>> {
    catalog
        .endpoints()
        .iter()
        .find(|ep| ep.tool_name() == name)
        .map(|ep| build_tool(catalog, config, ep, executor))
}

/// Tool specs for discovery, without binding an executor — what a host
/// advertises in `tools/list` before any call binds a payer.
pub fn hyre_specs(catalog: &HyreCatalog, config: &HyreConfig) -> Vec<covenant_mcp::ToolSpec> {
    hyre_tools(catalog, config, Arc::new(SpecOnly))
        .iter()
        .map(|t| t.spec())
        .collect()
}

struct SpecOnly;

#[async_trait]
impl PaidExecutor for SpecOnly {
    async fn execute(&self, _req: PaidRequest) -> std::result::Result<PaidResponse, String> {
        Err("spec-only executor cannot make calls".into())
    }
}

fn field_schema(description: &str) -> Value {
    if description.is_empty() {
        serde_json::json!({ "type": "string" })
    } else {
        serde_json::json!({ "type": "string", "description": description })
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

/// Percent-encode everything outside the unreserved set so addresses,
/// mints, and free-text query values survive the URL intact.
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
    impl PaidExecutor for MockExecutor {
        async fn execute(&self, req: PaidRequest) -> Result<PaidResponse, String> {
            *self.last.lock().unwrap() = Some(req);
            Ok(PaidResponse {
                status: if self.status == 0 { 200 } else { self.status },
                body: serde_json::json!({ "ok": true }),
                receipt_id: Some("rcpt-1".into()),
            })
        }
    }

    fn tools_for(config: &HyreConfig, exec: Arc<dyn PaidExecutor>) -> Vec<Arc<dyn Tool>> {
        let cat = HyreCatalog::from_vendored(config).unwrap();
        hyre_tools(&cat, config, exec)
    }

    fn find<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
        tools
            .iter()
            .find(|t| t.name() == name)
            .expect("tool present")
    }

    #[test]
    fn generates_a_tool_per_endpoint_including_ask() {
        let tools = tools_for(&HyreConfig::default(), Arc::new(MockExecutor::default()));
        assert_eq!(tools.len(), 24);
        assert!(tools.iter().any(|t| t.name() == "hyre.ask"));
        assert!(tools.iter().any(|t| t.name() == "hyre.trenches.new-tokens"));
    }

    #[test]
    fn schema_marks_path_param_required_and_query_optional() {
        let tools = tools_for(&HyreConfig::default(), Arc::new(MockExecutor::default()));
        let schema = find(&tools, "hyre.trenches.curve.mint").input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "mint"));
        assert!(!required.iter().any(|v| v == "curve_key"));
        assert_eq!(schema["additionalProperties"], Value::Bool(false));
    }

    #[tokio::test]
    async fn substitutes_path_param_and_carries_pricing() {
        let exec = Arc::new(MockExecutor::default());
        let tools = tools_for(&HyreConfig::default(), exec.clone());
        let res = find(&tools, "hyre.trenches.curve.mint")
            .call(serde_json::json!({ "mint": "So11111111111111111111111111111111111111112" }))
            .await
            .unwrap();
        assert!(!res.is_error);
        let req = exec.last.lock().unwrap().clone().unwrap();
        assert_eq!(
            req.url,
            "https://mpp.hyreagent.fun/trenches/curve/So11111111111111111111111111111111111111112"
        );
        assert_eq!(req.method, "GET");
        assert_eq!(req.network, crate::config::SOLANA_NETWORK);
        assert_eq!(req.asset, crate::config::USDC_MINT);
        assert_eq!(req.per_call_cap, 10_000);
        assert_eq!(req.credits, 1);
        assert!(req.body.is_none());
    }

    #[tokio::test]
    async fn missing_required_path_param_is_invalid_arguments() {
        let tools = tools_for(&HyreConfig::default(), Arc::new(MockExecutor::default()));
        let err = find(&tools, "hyre.trenches.curve.mint")
            .call(serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn missing_required_body_field_is_invalid_arguments() {
        // hyre.ask is a POST whose "query" is a required body field. An
        // object that omits it must be rejected in resolve()'s body loop
        // (tools.rs:150-155) before the x402-billed executor runs. The
        // path-param sibling covers a GET path template — a separate code
        // path — so the POST body-field arm is otherwise unguarded.
        let tools = tools_for(&HyreConfig::default(), Arc::new(MockExecutor::default()));
        let err = find(&tools, "hyre.ask")
            .call(serde_json::json!({}))
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidArguments(m) => assert!(
                m.contains("missing body field") && m.contains("query"),
                "missing required body field must name the field: {m}"
            ),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_object_arguments_are_invalid_arguments() {
        // call() accepts only a JSON object or null (null => no args);
        // any other JSON type must be rejected at tools.rs:182 before
        // self.resolve() and the x402-billed executor run, or a malformed
        // tool-call payload would be forwarded as a billed upstream
        // request. The sibling missing-param test only reaches the
        // resolve() arm via {}, so this type arm is otherwise unguarded.
        let tools = tools_for(&HyreConfig::default(), Arc::new(MockExecutor::default()));
        for bad in [
            serde_json::json!("just a string"),
            serde_json::json!([1, 2, 3]),
            serde_json::json!(42),
        ] {
            let err = find(&tools, "hyre.ask").call(bad).await.unwrap_err();
            match err {
                ToolError::InvalidArguments(m) => assert!(
                    m.contains("arguments must be a JSON object"),
                    "non-object arguments must name the type requirement: {m}"
                ),
                other => panic!("expected InvalidArguments, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn spec_only_executor_call_surfaces_error_without_request() {
        // hyre_specs() binds SpecOnly purely to read tool .spec() for
        // tools/list discovery; no payer is attached. If a spec-only
        // tool is ever actually invoked, execute() must refuse rather
        // than make an unpaid upstream call — the refusal threads through
        // call()'s executor-Err arm (tools.rs:215) into a tool-level
        // error result, not a panic or a silent Ok.
        let tools = tools_for(&HyreConfig::default(), Arc::new(SpecOnly));
        let res = find(&tools, "hyre.ask")
            .call(serde_json::json!({ "query": "x" }))
            .await
            .unwrap();
        assert!(
            res.is_error,
            "a spec-only tool call must surface as an error result, not Ok"
        );
        let Content::Text { text } = &res.content[0] else {
            panic!("expected text content, got {:?}", res.content);
        };
        assert!(
            text.contains("spec-only executor cannot make calls"),
            "tool error must carry the spec-only refusal reason: {text}"
        );
    }

    #[tokio::test]
    async fn ask_sends_query_in_body() {
        let exec = Arc::new(MockExecutor::default());
        let tools = tools_for(&HyreConfig::default(), exec.clone());
        let res = find(&tools, "hyre.ask")
            .call(serde_json::json!({ "query": "top whales on bonk?" }))
            .await
            .unwrap();
        assert!(!res.is_error);
        let req = exec.last.lock().unwrap().clone().unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://mpp.hyreagent.fun/ask");
        assert_eq!(req.body.unwrap()["query"], "top whales on bonk?");
        assert_eq!(req.per_call_cap, 250_000);
    }

    #[tokio::test]
    async fn query_param_is_appended_and_encoded() {
        let exec = Arc::new(MockExecutor::default());
        let tools = tools_for(&HyreConfig::default(), exec.clone());
        find(&tools, "hyre.trenches.curve.mint")
            .call(serde_json::json!({ "mint": "Mint1", "curve_key": "a b/c" }))
            .await
            .unwrap();
        let req = exec.last.lock().unwrap().clone().unwrap();
        assert!(req
            .url
            .ends_with("/trenches/curve/Mint1?curve_key=a%20b%2Fc"));
    }

    #[tokio::test]
    async fn upstream_error_status_surfaces_as_tool_error() {
        let exec = Arc::new(MockExecutor {
            status: 502,
            ..Default::default()
        });
        let tools = tools_for(&HyreConfig::default(), exec);
        let res = find(&tools, "hyre.ask")
            .call(serde_json::json!({ "query": "x" }))
            .await
            .unwrap();
        assert!(res.is_error);
    }

    #[test]
    fn arg_str_coerces_scalar_arguments_to_strings() {
        // Tool arguments arrive as arbitrary JSON. A numeric id or a boolean
        // flag must reach the request as its string form, not be dropped as a
        // missing argument — otherwise a model that sends `{ "mint": 42 }`
        // gets a spurious "missing required" failure.
        let mut args = Map::new();
        args.insert("count".into(), serde_json::json!(42));
        args.insert("ratio".into(), serde_json::json!(3.5));
        args.insert("flag".into(), serde_json::json!(false));
        args.insert("name".into(), serde_json::json!("Mint1"));

        assert_eq!(arg_str(&args, "count").as_deref(), Some("42"));
        assert_eq!(arg_str(&args, "ratio").as_deref(), Some("3.5"));
        assert_eq!(arg_str(&args, "flag").as_deref(), Some("false"));
        assert_eq!(arg_str(&args, "name").as_deref(), Some("Mint1"));
    }

    #[test]
    fn arg_str_treats_empty_structured_and_absent_as_missing() {
        // An empty string, a non-scalar value, an explicit null, and an
        // absent key all read as "not provided" so a blank or structured
        // value never gets substituted into a path or query slot.
        let mut args = Map::new();
        args.insert("blank".into(), serde_json::json!(""));
        args.insert("nested".into(), serde_json::json!({ "x": 1 }));
        args.insert("list".into(), serde_json::json!([1, 2]));
        args.insert("nullish".into(), Value::Null);

        assert_eq!(arg_str(&args, "blank"), None, "empty string is absent");
        assert_eq!(arg_str(&args, "nested"), None);
        assert_eq!(arg_str(&args, "list"), None);
        assert_eq!(arg_str(&args, "nullish"), None);
        assert_eq!(arg_str(&args, "absent"), None);
    }
}
