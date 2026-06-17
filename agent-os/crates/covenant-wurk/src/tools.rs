//! MCP tools for the WURK agent-to-human family.
//!
//! Three tools: `wurk.hire_humans` (paid create via the x402
//! PAYMENT-SIGNATURE loop), `wurk.job_status` (free view), and
//! `wurk.choose_winners` (free creator-mode selection). A tool never
//! signs or holds keys: it marshals args and calls a [`WurkExecutor`]
//! the daemon supplies (x402 client + signer sidecar + accounting).
//!
//! Only the agent-to-human endpoint families are constructible here.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError, ToolSpec};
use serde_json::{Map, Value};

use crate::config::WurkConfig;

/// Provider tag for audit.
pub const PROVIDER: &str = "wurk";

/// A resolved paid create call the daemon executes and accounts for.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidRequest {
    pub provider: String,
    /// Endpoint slug for audit (`agenttohuman` / `agenttohumanadvanced`).
    pub slug: String,
    /// Fully resolved URL with the query string appended.
    pub url: String,
    pub method: String,
    pub body: Option<Value>,
    /// CAIP-2 settlement network.
    pub network: String,
    /// Payment asset (USDC mint).
    pub asset: String,
    /// Per-call ceiling in atomic USDC (winners * perUser).
    pub per_call_cap: u128,
}

/// Outcome of a paid create call.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidResponse {
    pub status: u16,
    pub body: Value,
    pub receipt_id: Option<String>,
}

/// The daemon's seam: paid create + free reads. Implemented over the
/// x402 loop + signer sidecar; mocked in this crate's tests.
#[async_trait]
pub trait WurkExecutor: Send + Sync {
    /// Create a paid agent-to-human job (runs the PAYMENT-SIGNATURE loop).
    async fn create(&self, req: PaidRequest) -> std::result::Result<PaidResponse, String>;
    /// View submissions for a job (free, secret-authed).
    async fn view(&self, secret: &str, page: u32) -> std::result::Result<Value, String>;
    /// Pick winners by submission id (free, creator mode, secret-authed).
    async fn choose_winners(
        &self,
        secret: &str,
        submission_ids: Vec<String>,
    ) -> std::result::Result<Value, String>;
}

struct HireHumansTool {
    base_url: String,
    network: String,
    asset: String,
    cap_override: u128,
    executor: Arc<dyn WurkExecutor>,
}

#[async_trait]
impl Tool for HireHumansTool {
    fn name(&self) -> &str {
        "wurk.hire_humans"
    }
    fn description(&self) -> &str {
        "Hire humans to review an agent's work via WURK (agent-to-human microjob, paid per winner in USDC over x402). Best-effort, custodial (WURK holds funds and pays workers). Use for non-code human judgment: content, instructions, faithfulness."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "The task shown to human workers." },
                "winners": { "type": "integer", "description": "Number of paid replies sought (default 10)." },
                "per_user_usdc": { "type": "number", "description": "USDC paid per winner (default 0.05, min 0.01)." },
                "advanced": { "type": "boolean", "description": "Use agenttohumanadvanced (enables the fields below)." },
                "selection_type": { "type": "string", "enum": ["creator", "random"], "description": "Winner selection mode (advanced)." },
                "selection_time_minutes": { "type": "integer", "description": "Submission window, 60-4320 (advanced)." },
                "human_verified": { "type": "boolean", "description": "Require VeryAI palm verification (advanced)." },
                "community": { "type": "string", "description": "Restrict to a WURK community (e.g. the Covenant pool)." }
            },
            "required": ["description"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let args = as_object(&arguments)?;
        let description = req_str(args, "description")?;
        let winners = args.get("winners").and_then(Value::as_u64).unwrap_or(10).max(1);
        let per_user = args
            .get("per_user_usdc")
            .and_then(Value::as_f64)
            .unwrap_or(0.05)
            .max(0.01);
        let advanced = args.get("advanced").and_then(Value::as_bool).unwrap_or(false)
            || args.contains_key("selection_type")
            || args.contains_key("selection_time_minutes")
            || args.contains_key("human_verified");

        let slug = if advanced {
            "agenttohumanadvanced"
        } else {
            "agenttohuman"
        };
        let mut query: Vec<(String, String)> = vec![
            ("description".into(), description),
            ("winners".into(), winners.to_string()),
            ("perUser".into(), trim_num(per_user)),
        ];
        if advanced {
            if let Some(v) = args.get("selection_type").and_then(Value::as_str) {
                query.push(("selectionType".into(), v.to_string()));
            }
            if let Some(v) = args.get("selection_time_minutes").and_then(Value::as_u64) {
                query.push(("selectionTimeMinutes".into(), v.to_string()));
            }
            if args.get("human_verified").and_then(Value::as_bool) == Some(true) {
                query.push(("human_verified".into(), "yes".into()));
            }
        }
        if let Some(v) = args.get("community").and_then(Value::as_str) {
            query.push(("community".into(), v.to_string()));
        }

        let url = format!(
            "{}/solana/{slug}?{}",
            self.base_url.trim_end_matches('/'),
            query
                .iter()
                .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
                .collect::<Vec<_>>()
                .join("&")
        );

        let computed_cap = (winners as f64 * per_user * 1_000_000.0).round() as u128;
        let per_call_cap = if self.cap_override > 0 {
            self.cap_override.min(computed_cap.max(1))
        } else {
            computed_cap.max(1)
        };

        let req = PaidRequest {
            provider: PROVIDER.into(),
            slug: slug.into(),
            url,
            method: "GET".into(),
            body: None,
            network: self.network.clone(),
            asset: self.asset.clone(),
            per_call_cap,
        };

        match self.executor.create(req).await {
            Ok(resp) if (200..300).contains(&resp.status) => {
                let mut content = vec![Content::json(resp.body)];
                if let Some(rid) = resp.receipt_id {
                    content.push(Content::text(format!("x402 settled; receipt {rid}")));
                }
                Ok(ToolCallResult::ok(content))
            }
            Ok(resp) => Ok(ToolCallResult::error(format!(
                "wurk hire_humans returned status {}",
                resp.status
            ))),
            Err(e) => Ok(ToolCallResult::error(format!("wurk create failed: {e}"))),
        }
    }
}

struct JobStatusTool {
    executor: Arc<dyn WurkExecutor>,
}

#[async_trait]
impl Tool for JobStatusTool {
    fn name(&self) -> &str {
        "wurk.job_status"
    }
    fn description(&self) -> &str {
        "Read submissions for a WURK job (free, by job secret). Returns the submissions array for grading."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "secret": { "type": "string", "description": "The job secret returned at creation." },
                "page": { "type": "integer", "description": "Page number (default 1)." }
            },
            "required": ["secret"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let args = as_object(&arguments)?;
        let secret = req_str(args, "secret")?;
        let page = args.get("page").and_then(Value::as_u64).unwrap_or(1) as u32;
        match self.executor.view(&secret, page).await {
            Ok(body) => Ok(ToolCallResult::ok(vec![Content::json(body)])),
            Err(e) => Ok(ToolCallResult::error(format!("wurk view failed: {e}"))),
        }
    }
}

struct ChooseWinnersTool {
    executor: Arc<dyn WurkExecutor>,
}

#[async_trait]
impl Tool for ChooseWinnersTool {
    fn name(&self) -> &str {
        "wurk.choose_winners"
    }
    fn description(&self) -> &str {
        "Select winners for a WURK job by submission id (free, creator mode, by job secret)."
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "secret": { "type": "string", "description": "The job secret." },
                "submission_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Submission short ids to mark as winners."
                }
            },
            "required": ["secret", "submission_ids"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: Value) -> Result<ToolCallResult, ToolError> {
        let args = as_object(&arguments)?;
        let secret = req_str(args, "secret")?;
        let ids: Vec<String> = args
            .get("submission_ids")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(ToolError::InvalidArguments(
                "submission_ids must be a non-empty array of strings".into(),
            ));
        }
        match self.executor.choose_winners(&secret, ids).await {
            Ok(body) => Ok(ToolCallResult::ok(vec![Content::json(body)])),
            Err(e) => Ok(ToolCallResult::error(format!(
                "wurk choose_winners failed: {e}"
            ))),
        }
    }
}

/// Build the WURK tool set wired to `executor`.
pub fn wurk_tools(config: &WurkConfig, executor: Arc<dyn WurkExecutor>) -> Vec<Arc<dyn Tool>> {
    if !config.enabled {
        return Vec::new();
    }
    vec![
        Arc::new(HireHumansTool {
            base_url: config.base_url.clone(),
            network: config.network.clone(),
            asset: config.asset.clone(),
            cap_override: config.per_call_cap,
            executor: executor.clone(),
        }),
        Arc::new(JobStatusTool {
            executor: executor.clone(),
        }),
        Arc::new(ChooseWinnersTool { executor }),
    ]
}

/// Tool specs for discovery without binding a payer.
pub fn wurk_specs(config: &WurkConfig) -> Vec<ToolSpec> {
    let mut cfg = config.clone();
    cfg.enabled = true;
    wurk_tools(&cfg, Arc::new(SpecOnly))
        .iter()
        .map(|t| t.spec())
        .collect()
}

struct SpecOnly;

#[async_trait]
impl WurkExecutor for SpecOnly {
    async fn create(&self, _req: PaidRequest) -> std::result::Result<PaidResponse, String> {
        Err("spec-only executor cannot make calls".into())
    }
    async fn view(&self, _secret: &str, _page: u32) -> std::result::Result<Value, String> {
        Err("spec-only executor cannot make calls".into())
    }
    async fn choose_winners(
        &self,
        _secret: &str,
        _ids: Vec<String>,
    ) -> std::result::Result<Value, String> {
        Err("spec-only executor cannot make calls".into())
    }
}

fn as_object(arguments: &Value) -> Result<&Map<String, Value>, ToolError> {
    match arguments {
        Value::Object(m) => Ok(m),
        Value::Null => {
            Err(ToolError::InvalidArguments("arguments must be a JSON object".into()))
        }
        _ => Err(ToolError::InvalidArguments(
            "arguments must be a JSON object".into(),
        )),
    }
}

fn req_str(args: &Map<String, Value>, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| ToolError::InvalidArguments(format!("missing string {key:?}")))
}

/// Format a USDC amount without a trailing `.0` so `0.05` stays `0.05`.
fn trim_num(n: f64) -> String {
    let s = format!("{n}");
    s
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
        last_create: Mutex<Option<PaidRequest>>,
        last_view: Mutex<Option<(String, u32)>>,
        last_choose: Mutex<Option<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl WurkExecutor for MockExecutor {
        async fn create(&self, req: PaidRequest) -> Result<PaidResponse, String> {
            *self.last_create.lock().unwrap() = Some(req);
            Ok(PaidResponse {
                status: 200,
                body: serde_json::json!({ "ok": true, "jobId": "j1", "secret": "s1" }),
                receipt_id: Some("rcpt-1".into()),
            })
        }
        async fn view(&self, secret: &str, page: u32) -> Result<Value, String> {
            *self.last_view.lock().unwrap() = Some((secret.into(), page));
            Ok(serde_json::json!({ "ok": true, "submissions": [] }))
        }
        async fn choose_winners(&self, secret: &str, ids: Vec<String>) -> Result<Value, String> {
            *self.last_choose.lock().unwrap() = Some((secret.into(), ids));
            Ok(serde_json::json!({ "ok": true }))
        }
    }

    fn enabled_config() -> WurkConfig {
        WurkConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn find<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
        tools.iter().find(|t| t.name() == name).expect("tool present")
    }

    #[test]
    fn disabled_config_registers_no_tools() {
        assert!(wurk_tools(&WurkConfig::default(), Arc::new(MockExecutor::default())).is_empty());
    }

    #[test]
    fn enabled_config_registers_three_tools() {
        let tools = wurk_tools(&enabled_config(), Arc::new(MockExecutor::default()));
        assert_eq!(tools.len(), 3);
        for n in ["wurk.hire_humans", "wurk.job_status", "wurk.choose_winners"] {
            assert!(tools.iter().any(|t| t.name() == n), "missing {n}");
        }
    }

    #[tokio::test]
    async fn hire_builds_url_and_cap() {
        let exec = Arc::new(MockExecutor::default());
        let tools = wurk_tools(&enabled_config(), exec.clone());
        let res = find(&tools, "wurk.hire_humans")
            .call(serde_json::json!({ "description": "judge this", "winners": 10, "per_user_usdc": 0.05 }))
            .await
            .unwrap();
        assert!(!res.is_error);
        let req = exec.last_create.lock().unwrap().clone().unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.slug, "agenttohuman");
        assert!(req.url.starts_with("https://wurkapi.fun/solana/agenttohuman?"));
        assert!(req.url.contains("description=judge%20this"));
        assert!(req.url.contains("winners=10"));
        assert!(req.url.contains("perUser=0.05"));
        // 10 * 0.05 USDC = $0.50 = 500000 atomic.
        assert_eq!(req.per_call_cap, 500_000);
    }

    #[tokio::test]
    async fn advanced_flags_route_to_advanced_endpoint() {
        let exec = Arc::new(MockExecutor::default());
        let tools = wurk_tools(&enabled_config(), exec.clone());
        find(&tools, "wurk.hire_humans")
            .call(serde_json::json!({
                "description": "x", "selection_type": "creator",
                "selection_time_minutes": 1440, "human_verified": true,
                "community": "covenant"
            }))
            .await
            .unwrap();
        let req = exec.last_create.lock().unwrap().clone().unwrap();
        assert_eq!(req.slug, "agenttohumanadvanced");
        assert!(req.url.contains("/solana/agenttohumanadvanced?"));
        assert!(req.url.contains("selectionType=creator"));
        assert!(req.url.contains("selectionTimeMinutes=1440"));
        assert!(req.url.contains("human_verified=yes"));
        assert!(req.url.contains("community=covenant"));
    }

    #[tokio::test]
    async fn hire_requires_description() {
        let tools = wurk_tools(&enabled_config(), Arc::new(MockExecutor::default()));
        let err = find(&tools, "wurk.hire_humans")
            .call(serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn job_status_passes_secret_and_page() {
        let exec = Arc::new(MockExecutor::default());
        let tools = wurk_tools(&enabled_config(), exec.clone());
        find(&tools, "wurk.job_status")
            .call(serde_json::json!({ "secret": "abc", "page": 2 }))
            .await
            .unwrap();
        assert_eq!(
            exec.last_view.lock().unwrap().clone().unwrap(),
            ("abc".to_string(), 2)
        );
    }

    #[tokio::test]
    async fn choose_winners_requires_non_empty_ids() {
        let tools = wurk_tools(&enabled_config(), Arc::new(MockExecutor::default()));
        let err = find(&tools, "wurk.choose_winners")
            .call(serde_json::json!({ "secret": "s", "submission_ids": [] }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn choose_winners_forwards_ids() {
        let exec = Arc::new(MockExecutor::default());
        let tools = wurk_tools(&enabled_config(), exec.clone());
        find(&tools, "wurk.choose_winners")
            .call(serde_json::json!({ "secret": "s", "submission_ids": ["ba5db22d", "abc12345"] }))
            .await
            .unwrap();
        let (secret, ids) = exec.last_choose.lock().unwrap().clone().unwrap();
        assert_eq!(secret, "s");
        assert_eq!(ids, vec!["ba5db22d", "abc12345"]);
    }

    #[test]
    fn specs_list_three_tools_without_executor() {
        let specs = wurk_specs(&WurkConfig::default());
        assert_eq!(specs.len(), 3);
    }
}
