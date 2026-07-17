//! Invoica MCP tools.
//!
//! Four actions over Invoica's invoice API: create, get, and list invoices,
//! and read settlement state. Each holds a shared [`InvoicaClient`] carrying
//! the brokered key, so the tool runs the authenticated call and the agent
//! only sees the result. The daemon's generic tool path capability-gates and
//! audits each call. The x402-paid surface (the priced invoice/settle/tax
//! endpoints) is Phase 2.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Map, Value};

use crate::client::InvoicaClient;
use crate::config::InvoicaConfig;
use crate::types::CreateInvoiceRequest;

pub const PROVIDER: &str = "invoica";
pub const INVOICE_CREATE: &str = "invoica.invoice.create";
pub const INVOICE_GET: &str = "invoica.invoice.get";
pub const INVOICE_LIST: &str = "invoica.invoice.list";
pub const SETTLEMENT_CHECK: &str = "invoica.settlement.check";

const CURRENCIES: [&str; 3] = ["USD", "EUR", "GBP"];
const MAX_FIELD_LEN: usize = 512;
const MAX_PAGE_SIZE: u64 = 100;

/// Build the Invoica tool set. Empty when disabled. `default_chain` from config
/// stamps invoices the caller does not pin a chain on.
pub fn invoica_tools(client: Arc<InvoicaClient>, cfg: &InvoicaConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    vec![
        Arc::new(CreateTool {
            client: client.clone(),
            default_chain: cfg.chain.clone(),
        }),
        Arc::new(GetTool {
            client: client.clone(),
        }),
        Arc::new(ListTool {
            client: client.clone(),
        }),
        Arc::new(SettlementTool { client }),
    ]
}

fn ok(value: Value) -> ToolCallResult {
    ToolCallResult::ok(vec![Content::json(value)])
}

fn arg_str(args: &Value, key: &str) -> Result<String, ToolError> {
    let s = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::InvalidArguments(format!("missing non-empty \"{key}\"")))?;
    bounded(key, s)
}

fn opt_str(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key).and_then(Value::as_str).map(str::trim) {
        Some(s) if !s.is_empty() => Ok(Some(bounded(key, s)?)),
        _ => Ok(None),
    }
}

fn bounded(key: &str, s: &str) -> Result<String, ToolError> {
    if s.len() > MAX_FIELD_LEN {
        return Err(ToolError::InvalidArguments(format!(
            "\"{key}\" exceeds {MAX_FIELD_LEN} bytes"
        )));
    }
    Ok(s.to_string())
}

/// An invoice id argument. Rejects `.` and `..`, which would resolve to a
/// different path than `/v1/invoices/:id` once the URL is normalized.
fn invoice_id(args: &Value, key: &str) -> Result<String, ToolError> {
    let id = arg_str(args, key)?;
    if id == "." || id == ".." {
        return Err(ToolError::InvalidArguments(format!(
            "\"{key}\" is not a valid invoice id"
        )));
    }
    Ok(id)
}

fn positive_amount(args: &Value) -> Result<f64, ToolError> {
    args.get("amount")
        .and_then(Value::as_f64)
        .filter(|a| a.is_finite() && *a > 0.0)
        .ok_or_else(|| ToolError::InvalidArguments("\"amount\" must be a positive number".into()))
}

struct CreateTool {
    client: Arc<InvoicaClient>,
    default_chain: String,
}

#[async_trait]
impl Tool for CreateTool {
    fn name(&self) -> &str {
        INVOICE_CREATE
    }
    fn description(&self) -> &str {
        "Create an Invoica invoice for a completed service. Amount is in the invoice currency \
         (USD, EUR, or GBP); settlement is in USDC on chain. Returns the created invoice."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "amount": { "type": "number", "description": "Invoice amount in the given currency, e.g. 100 for 100.00." },
                "customerEmail": { "type": "string" },
                "customerName": { "type": "string" },
                "currency": { "type": "string", "enum": ["USD", "EUR", "GBP"] },
                "chain": { "type": "string", "description": "Settlement chain; defaults to the configured chain." },
                "buyerCountryCode": { "type": "string", "description": "ISO country code, for tax." },
                "buyerStateCode": { "type": "string", "description": "US state code, for tax." },
                "companyId": { "type": "string" }
            },
            "required": ["amount", "customerEmail", "customerName"],
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let currency = match opt_str(&args, "currency")? {
            Some(c) if !CURRENCIES.contains(&c.as_str()) => {
                return Err(ToolError::InvalidArguments(
                    "\"currency\" must be USD, EUR, or GBP".into(),
                ));
            }
            other => other,
        };
        let req = CreateInvoiceRequest {
            amount: positive_amount(&args)?,
            customer_email: arg_str(&args, "customerEmail")?,
            customer_name: arg_str(&args, "customerName")?,
            currency,
            chain: Some(opt_str(&args, "chain")?.unwrap_or_else(|| self.default_chain.clone())),
            buyer_country_code: opt_str(&args, "buyerCountryCode")?,
            buyer_state_code: opt_str(&args, "buyerStateCode")?,
            company_id: opt_str(&args, "companyId")?,
        };
        match self.client.create_invoice(&req).await {
            Ok(inv) => Ok(ok(json!({ "provider": PROVIDER, "invoice": inv }))),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

struct GetTool {
    client: Arc<InvoicaClient>,
}

#[async_trait]
impl Tool for GetTool {
    fn name(&self) -> &str {
        INVOICE_GET
    }
    fn description(&self) -> &str {
        "Fetch a single Invoica invoice by id."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"],
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let id = invoice_id(&args, "id")?;
        match self.client.get_invoice(&id).await {
            Ok(inv) => Ok(ok(json!({ "provider": PROVIDER, "invoice": inv }))),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

struct ListTool {
    client: Arc<InvoicaClient>,
}

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        INVOICE_LIST
    }
    fn description(&self) -> &str {
        "List Invoica invoices, optionally filtered by status or chain."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string" },
                "chain": { "type": "string" },
                "page": { "type": "integer", "minimum": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
            },
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let mut params: Vec<(String, String)> = Vec::new();
        for key in ["status", "chain"] {
            if let Some(v) = opt_str(&args, key)? {
                params.push((key.to_string(), v));
            }
        }
        if let Some(p) = args.get("page").and_then(Value::as_u64).filter(|p| *p >= 1) {
            params.push(("page".into(), p.to_string()));
        }
        if let Some(l) = args.get("limit").and_then(Value::as_u64) {
            params.push(("limit".into(), l.clamp(1, MAX_PAGE_SIZE).to_string()));
        }
        match self.client.list_invoices(&params).await {
            Ok(v) => Ok(ok(json!({ "provider": PROVIDER, "result": v }))),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

/// Invoica has no settlement-by-invoice endpoint; settlement state lives on the
/// invoice itself (`status`, `paymentDetails`, `settledAt`, `completedAt`). This
/// reads the invoice and projects those fields.
struct SettlementTool {
    client: Arc<InvoicaClient>,
}

#[async_trait]
impl Tool for SettlementTool {
    fn name(&self) -> &str {
        SETTLEMENT_CHECK
    }
    fn description(&self) -> &str {
        "Check settlement state for an Invoica invoice: its status, the on-chain payment details \
         and signature once paid, and when it settled."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "invoiceId": { "type": "string" } },
            "required": ["invoiceId"],
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let id = invoice_id(&args, "invoiceId")?;
        let inv = match self.client.get_invoice(&id).await {
            Ok(inv) => inv,
            Err(e) => return Ok(ToolCallResult::error(e.to_string())),
        };
        let mut out = Map::new();
        out.insert("invoiceId".into(), json!(id));
        for key in ["status", "paymentDetails", "settledAt", "completedAt"] {
            if let Some(v) = inv.get(key) {
                out.insert(key.into(), v.clone());
            }
        }
        Ok(ok(
            json!({ "provider": PROVIDER, "settlement": Value::Object(out) }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn enabled() -> InvoicaConfig {
        InvoicaConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn create_args() -> Value {
        json!({ "amount": 100.0, "customerEmail": "b@acme.test", "customerName": "Acme" })
    }

    #[test]
    fn disabled_registers_nothing() {
        let c = Arc::new(InvoicaClient::new("http://x", "k"));
        assert!(invoica_tools(c, &InvoicaConfig::default()).is_empty());
    }

    #[test]
    fn enabled_registers_the_four_tools() {
        let c = Arc::new(InvoicaClient::new("http://x", "k"));
        let tools = invoica_tools(c, &enabled());
        let names: Vec<_> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            [INVOICE_CREATE, INVOICE_GET, INVOICE_LIST, SETTLEMENT_CHECK]
        );
    }

    #[tokio::test]
    async fn create_defaults_chain_and_returns_invoice() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/invoices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_9", "invoiceNumber": 9, "status": "PENDING", "amount": 100.0, "currency": "USD"
            })))
            .mount(&server)
            .await;
        let tool = CreateTool {
            client: Arc::new(InvoicaClient::new(server.uri(), "k")),
            default_chain: "solana".into(),
        };
        let res = tool.call(create_args()).await.unwrap();
        assert!(!res.is_error);
        let body = match &res.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        };
        assert_eq!(body["invoice"]["id"], "inv_9");
        assert_eq!(body["provider"], "invoica");
    }

    #[tokio::test]
    async fn create_rejects_missing_customer_and_bad_amount() {
        let tool = CreateTool {
            client: Arc::new(InvoicaClient::new("http://x", "k")),
            default_chain: "solana".into(),
        };
        let no_email = tool
            .call(json!({ "amount": 1.0, "customerName": "Acme" }))
            .await
            .unwrap_err();
        assert!(matches!(no_email, ToolError::InvalidArguments(_)));
        let bad_amt = tool
            .call(json!({ "amount": -5, "customerEmail": "b@a.co", "customerName": "Acme" }))
            .await
            .unwrap_err();
        assert!(matches!(bad_amt, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn create_rejects_unknown_currency() {
        let tool = CreateTool {
            client: Arc::new(InvoicaClient::new("http://x", "k")),
            default_chain: "solana".into(),
        };
        let mut args = create_args();
        args["currency"] = json!("BTC");
        assert!(matches!(
            tool.call(args).await.unwrap_err(),
            ToolError::InvalidArguments(_)
        ));
    }

    #[tokio::test]
    async fn get_and_settlement_reject_dot_segment_ids() {
        let get = GetTool {
            client: Arc::new(InvoicaClient::new("http://x", "k")),
        };
        let settle = SettlementTool {
            client: Arc::new(InvoicaClient::new("http://x", "k")),
        };
        for bad in [".", ".."] {
            assert!(matches!(
                get.call(json!({ "id": bad })).await.unwrap_err(),
                ToolError::InvalidArguments(_)
            ));
            assert!(matches!(
                settle.call(json!({ "invoiceId": bad })).await.unwrap_err(),
                ToolError::InvalidArguments(_)
            ));
        }
    }

    #[tokio::test]
    async fn settlement_projects_invoice_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/invoices/inv_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "inv_1", "status": "SETTLED", "amount": 100.0,
                "paymentDetails": { "txHash": "5xy", "network": "solana-mainnet" },
                "settledAt": "2026-07-01T00:00:00Z"
            })))
            .mount(&server)
            .await;
        let tool = SettlementTool {
            client: Arc::new(InvoicaClient::new(server.uri(), "k")),
        };
        let res = tool.call(json!({ "invoiceId": "inv_1" })).await.unwrap();
        let body = match &res.content[0] {
            Content::Json { value } => value.clone(),
            other => panic!("expected json, got {other:?}"),
        };
        assert_eq!(body["settlement"]["status"], "SETTLED");
        assert_eq!(body["settlement"]["paymentDetails"]["txHash"], "5xy");
        assert_eq!(body["settlement"]["invoiceId"], "inv_1");
    }
}
