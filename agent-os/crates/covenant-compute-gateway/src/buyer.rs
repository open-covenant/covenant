//! The real gateway-to-Covenant execution adapter. Its budget is mandatory;
//! successful output includes the receipt already verified by the buyer SDK.
use crate::discovery::{DiscoveryOffer, DiscoveryPricing};
use crate::pricing::TariffBook;
use crate::{ExecutionFailure, ExecutionQuote, Executor, GatewayError, PreparedExecution, Result};
use async_trait::async_trait;
use covenant_compute_buyer::{
    dispatch_signed, durable_budget::DurableBudget, sign_envelope, BuyerConfig, JobRequest,
};
use covenant_compute_protocol::{
    canonical_model, chat_input, generation_input, CapacityView, ChatMessage, ChatRole,
    GenerationParams, JobKind,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const DELIVERY_MARGIN_MS: u64 = 45_000;
const CAPACITY_TIMEOUT: Duration = Duration::from_secs(5);
const CAPACITY_MAX_BYTES: usize = 1024 * 1024;

async fn read_capacity(url: &str, timeout: Duration) -> Result<CapacityView> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| GatewayError::Unavailable("capacity client unavailable".into()))?;
    let mut response = http
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::PRAGMA, "no-cache")
        .send()
        .await
        .map_err(|_| GatewayError::Unavailable("live capacity unavailable".into()))?;
    if response.status() != reqwest::StatusCode::OK
        || response
            .content_length()
            .is_some_and(|size| size > CAPACITY_MAX_BYTES as u64)
    {
        return Err(GatewayError::Unavailable(
            "invalid capacity response".into(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| GatewayError::Unavailable("live capacity read failed".into()))?
    {
        if chunk.len() > CAPACITY_MAX_BYTES.saturating_sub(bytes.len()) {
            return Err(GatewayError::Unavailable(
                "capacity response too large".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| GatewayError::Unavailable("invalid capacity document".into()))
}

pub struct BuyerExecutor {
    pub config: BuyerConfig,
    pub identity: Arc<LocalIdentity>,
    pub http: reqwest::Client,
    pub budget: DurableBudget,
    pub model: String,
    pub offer_micro_usdc: u64,
    pub max_tokens: u32,
    /// UTF-8 bytes of the normalized JSON message array, matching the provider.
    pub max_input_bytes: usize,
    /// Operator receipt keys approved for this gateway deployment.
    pub approved_operators: Vec<String>,
    pub tariffs: Option<Arc<TariffBook>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    model: String,
    messages: Vec<TextMessage>,
    max_tokens: u32,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    pricing: Option<PricingMode>,
}
#[derive(Deserialize)]
enum PricingMode {
    #[serde(rename = "model_tariff_v1")]
    ModelTariffV1,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TextMessage {
    role: ChatRole,
    content: String,
}

#[async_trait]
impl Executor for BuyerExecutor {
    fn validate_request(&self, request: &Value) -> Result<()> {
        self.parse_request(request).map(|_| ())
    }
    async fn discovery_offers(&self, now: u64, legacy_price: u64) -> Result<Vec<DiscoveryOffer>> {
        let started = Instant::now();
        let url = format!(
            "{}/federation/capacity",
            self.config.coordinator_url.trim_end_matches('/')
        );
        let capacity = read_capacity(&url, CAPACITY_TIMEOUT).await?;
        // Re-evaluate tariff validity and the durable clock watermark after the
        // bounded network wait. Never refresh discovery by changing the budget.
        let now = crate::after_wait(now, started);
        let remaining = self
            .budget
            .snapshot(now)
            .map_err(|_| GatewayError::Unavailable("durable budget unavailable".into()))?
            .remaining_micro_usdc;
        Ok(self.offers_from_capacity(&capacity, now, legacy_price, remaining))
    }
    async fn check_available(
        &self,
        request: &Value,
        now: u64,
        legacy_price: u64,
        remaining: u64,
    ) -> Result<()> {
        self.validate_request(request)?;
        let offers = self.discovery_offers(now, legacy_price).await?;
        if offers
            .into_iter()
            .any(|mut offer| offer.constrain_to_budget(remaining) && offer.matches_request(request))
        {
            return Ok(());
        }
        Err(GatewayError::Unavailable(
            "requested inference has no observed affordable live supply".into(),
        ))
    }
    fn prepare(&self, request: &Value) -> Result<PreparedExecution> {
        self.prepare_with_pricing(request, true, crate::now_ms())
    }
    fn prepare_legacy(&self, request: &Value) -> Result<PreparedExecution> {
        self.prepare_with_pricing(request, false, crate::now_ms())
    }
    fn quote(&self, request: &Value, legacy_price: u64, now: u64) -> Result<ExecutionQuote> {
        let prepared = self.prepare_with_pricing(request, true, now)?;
        let (amount_atomic, mut expires_at_ms) = if request.get("pricing").is_some() {
            let book = self
                .tariffs
                .as_ref()
                .ok_or_else(|| GatewayError::Unavailable("model tariffs are disabled".into()))?;
            let model = request["model"]
                .as_str()
                .ok_or_else(|| GatewayError::Invalid("model required".into()))?;
            let cap = request["max_tokens"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| GatewayError::Invalid("output cap required".into()))?;
            let (retail, _) = book.quote(model, cap, now)?;
            let expires = now.saturating_add(90_000).min(
                retail
                    .tariff
                    .valid_until_ms
                    .saturating_sub(DELIVERY_MARGIN_MS),
            );
            (retail.price_micro_usdc, expires)
        } else {
            (legacy_price, now.saturating_add(90_000))
        };
        for envelope in &prepared.job_envelopes {
            expires_at_ms = expires_at_ms.min(
                envelope
                    .payload
                    .issued_at_ms
                    .saturating_add(covenant_compute_protocol::JOB_FRESHNESS_WINDOW_MS)
                    .saturating_sub(DELIVERY_MARGIN_MS),
            );
        }
        Ok(ExecutionQuote {
            batch_pricing: None,
            prepared,
            amount_atomic,
            expires_at_ms,
        })
    }
    fn validate_admission(&self, prepared: &PreparedExecution, now: u64) -> Result<()> {
        for envelope in &prepared.job_envelopes {
            if envelope.payload.issued_at_ms.saturating_add(
                envelope
                    .payload
                    .deadline_ms
                    .min(covenant_compute_protocol::JOB_FRESHNESS_WINDOW_MS),
            ) <= now.saturating_add(DELIVERY_MARGIN_MS)
            {
                return Err(GatewayError::Unavailable(
                    "native execution freshness or deadline too close".into(),
                ));
            }
            if let Some(quote) =
                covenant_compute_protocol::parse_tariff_input(&envelope.payload.input)
                    .map_err(|e| GatewayError::Invalid(e.to_string()))?
            {
                if !quote.tariff.valid_at(now)
                    || quote.tariff.valid_until_ms <= now.saturating_add(DELIVERY_MARGIN_MS)
                {
                    return Err(GatewayError::Unavailable(
                        "quoted native tariff expired or too close to expiry".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    fn validate_batch_budget(&self, amount: u64, now: u64) -> Result<()> {
        let remaining = self
            .budget
            .snapshot(now)
            .map_err(|_| GatewayError::Unavailable("durable budget unavailable".into()))?
            .remaining_micro_usdc;
        if amount > remaining {
            return Err(GatewayError::Unavailable(
                "batch exceeds remaining daily buyer budget".into(),
            ));
        }
        Ok(())
    }
    fn models(&self, now: u64, legacy_price: u64) -> Vec<Value> {
        self.tariffs.as_ref().map(|book| book.models(now)).unwrap_or_else(|| vec![json!({
            "id":self.model,"object":"model","owned_by":"covenant",
            "pricing":{"currency":"USDC","unit":"micro_usdc","billing":"fixed_request", "price_micro_usdc":legacy_price,"max_output_tokens":self.max_tokens}
        })])
    }
    async fn execute(
        &self,
        prepared: &PreparedExecution,
    ) -> std::result::Result<Value, ExecutionFailure> {
        let uncertain = |message: String| ExecutionFailure {
            message,
            definitely_unpaid: false,
            terminal_seller_loss: false,
        };
        if prepared.buyer != self.identity.agent_id().pubkey_base58()
            || prepared.coordinator != self.config.coordinator_url
            || prepared.operation.is_some()
            || prepared.job_envelopes.len() != 1
        {
            return Err(uncertain("persisted execution identity mismatch".into()));
        }
        let envelope = prepared.job_envelopes[0].clone();
        self.budget
            .reserve(&envelope, crate::now_ms())
            .map_err(|e| uncertain(e.to_string()))?;
        let outcome = dispatch_signed(&self.http, &self.config, &self.identity, envelope)
            .await
            .map_err(|e| ExecutionFailure {
                definitely_unpaid: e.concludes_purchase_unpaid(),
                terminal_seller_loss: false,
                message: e.to_string(),
            })?;
        self.require_approved_operator(&outcome.receipt.signer_pubkey_b58)?;
        completed_response(outcome, &self.model).map_err(|e| uncertain(e.to_string()))
    }
}
impl BuyerExecutor {
    fn offers_from_capacity(
        &self,
        view: &CapacityView,
        now: u64,
        legacy_price: u64,
        remaining: u64,
    ) -> Vec<DiscoveryOffer> {
        if view.matchable_operators == 0 || self.max_tokens == 0 {
            return Vec::new();
        }
        // Capacity is anonymous aggregate metadata. It cannot promise that a
        // matching operator is in approved_operators; signed receipt admission
        // continues to enforce that independent deployment policy.
        let rows: Vec<_> = view
            .entries
            .iter()
            .filter(|row| {
                row.kind == JobKind::InferenceCall
                    && row.operators > 0
                    && (row.model == "any"
                        || canonical_model(&row.model) == canonical_model(&self.model))
            })
            .collect();
        let mut offers = Vec::new();
        if self.offer_micro_usdc > 0
            && legacy_price >= self.offer_micro_usdc
            && rows
                .iter()
                .any(|row| row.min_ask_micro_usdc <= self.offer_micro_usdc)
        {
            offers.push(DiscoveryOffer {
                model: self.model.clone(),
                max_output_tokens: self.max_tokens,
                max_input_bytes: self.max_input_bytes,
                pricing: DiscoveryPricing::Legacy {
                    retail_micro_usdc: legacy_price,
                    native_micro_usdc: self.offer_micro_usdc,
                },
            });
        }
        if let Some(row) = self
            .tariffs
            .as_ref()
            .and_then(|book| book.for_model(&self.model))
        {
            if row.retail.valid_at(now)
                && row.native.valid_at(now)
                && row.retail.valid_until_ms > now.saturating_add(DELIVERY_MARGIN_MS)
                && row.native.valid_until_ms > now.saturating_add(DELIVERY_MARGIN_MS)
                && rows.iter().any(|capacity| {
                    capacity
                        .inference_tariffs
                        .iter()
                        .any(|offer| offer.tariff == row.native)
                })
            {
                offers.push(DiscoveryOffer {
                    model: self.model.clone(),
                    max_output_tokens: self.max_tokens.min(row.native.max_output_tokens),
                    max_input_bytes: self.max_input_bytes,
                    pricing: DiscoveryPricing::ModelTariff {
                        retail: row.retail.clone(),
                        native: row.native.clone(),
                    },
                });
            }
        }
        offers.retain_mut(|offer| offer.constrain_to_budget(remaining));
        offers
    }
    /// Pure request admission shared by fresh availability and signed preparation.
    /// This never reads a budget, fetches capacity, or uses the buyer identity.
    fn parse_request(&self, request: &Value) -> Result<Request> {
        if request.get("pricing").is_some_and(Value::is_null) {
            return Err(GatewayError::Invalid(
                "pricing must be omitted or model_tariff_v1".into(),
            ));
        }
        let request:Request=serde_json::from_value(request.clone()).map_err(|_|GatewayError::Invalid("expected text chat: model, messages, max_tokens, optional temperature/pricing; streaming unsupported".into()))?;
        if request.model != self.model
            || request.max_tokens == 0
            || request.max_tokens > self.max_tokens
            || request.stream
            || request.messages.is_empty()
            || request.messages.len() > 32
            || serde_json::to_vec(&request.messages)?.len() > self.max_input_bytes
            || request
                .messages
                .iter()
                .any(|m| m.role == ChatRole::Tool || m.content.is_empty())
        {
            return Err(GatewayError::Invalid(
                "unsupported model, output cap or message type".into(),
            ));
        }
        generation_input(GenerationParams {
            max_tokens: Some(request.max_tokens),
            temperature: request.temperature,
            ..Default::default()
        })
        .map_err(|e| GatewayError::Invalid(e.to_string()))?;
        Ok(request)
    }
    fn prepare_with_pricing(
        &self,
        request: &Value,
        use_tariffs: bool,
        now: u64,
    ) -> Result<PreparedExecution> {
        let request = self.parse_request(request)?;
        let mut input = chat_input(
            request
                .messages
                .into_iter()
                .map(|m| ChatMessage {
                    role: m.role,
                    content: m.content,
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                })
                .collect(),
        );
        input.push(
            generation_input(GenerationParams {
                max_tokens: Some(request.max_tokens),
                temperature: request.temperature,
                ..Default::default()
            })
            .map_err(|e| GatewayError::Invalid(e.to_string()))?,
        );
        let offer = if use_tariffs && request.pricing.is_some() {
            let book = self
                .tariffs
                .as_ref()
                .ok_or_else(|| GatewayError::Unavailable("model tariffs are disabled".into()))?;
            {
                let (_, native) = book.quote(&request.model, request.max_tokens, now)?;
                input.push(
                    covenant_compute_protocol::tariff_input(&native)
                        .map_err(|e| GatewayError::Invalid(e.to_string()))?,
                );
                native.price_micro_usdc
            }
        } else {
            self.offer_micro_usdc
        };
        let envelope = sign_envelope(
            &self.config,
            &self.identity,
            JobRequest {
                kind: JobKind::InferenceCall,
                input,
                model: Some(request.model),
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: offer,
                // Includes authorization and dispatch; quote expiry separately
                // reserves time within the node's signed-request freshness bound.
                deadline_ms: 180_000,
            },
        )
        .map_err(|e| GatewayError::Invalid(e.to_string()))?;
        Ok(PreparedExecution {
            operation: None,
            buyer: self.identity.agent_id().pubkey_base58(),
            coordinator: self.config.coordinator_url.clone(),
            job_envelopes: vec![envelope],
            maximum_cost_micro_usdc: offer,
        })
    }

    pub(crate) fn require_approved_operator(
        &self,
        signer_pubkey_b58: &str,
    ) -> std::result::Result<(), ExecutionFailure> {
        if !self
            .approved_operators
            .iter()
            .any(|key| key == signer_pubkey_b58)
        {
            return Err(ExecutionFailure {
                message: "verified native result used an unapproved inference operator; customer payment not settled".into(),
                definitely_unpaid: false,
                terminal_seller_loss: true,
            });
        }
        Ok(())
    }
}
pub fn completed_response(
    outcome: covenant_compute_buyer::DispatchOutcome,
    model: &str,
) -> Result<Value> {
    let text = outcome
        .output
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let finish_reason = outcome
        .receipt
        .receipt
        .meter
        .finish_reason
        .map(|reason| reason.as_openai())
        .unwrap_or("stop");
    Ok(json!({
        "id":format!("chatcmpl-{}",outcome.envelope.payload.job_id), "object":"chat.completion", "created":crate::now_ms()/1000,
        "model":model, "choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":finish_reason}],
        "covenant":{"job_id":outcome.envelope.payload.job_id,"envelope":outcome.envelope,"receipt":outcome.receipt,"output_json":serde_json::to_string(&outcome.output)?,"output":outcome.output}
    }))
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use crate::pricing::ModelTariff;
    use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
    use covenant_compute_protocol::{
        CapacityEntry, InferenceTariff, InferenceTariffOffer, PriceUnit,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    fn tariff(now: u64) -> ModelTariff {
        let native = InferenceTariff {
            schema_version: 1,
            id: "test-native".into(),
            revision: 1,
            model: "model".into(),
            base_micro_usdc: 100,
            output_cap_micro_usdc_per_million: 1_000_000,
            max_output_tokens: 64,
            valid_from_ms: now.saturating_sub(1),
            valid_until_ms: now + 100_000,
        };
        let retail = InferenceTariff {
            id: "test-retail".into(),
            base_micro_usdc: 120,
            ..native.clone()
        };
        ModelTariff { native, retail }
    }
    fn executor(root: &std::path::Path, endpoint: &str, now: u64) -> BuyerExecutor {
        let identity = Arc::new(LocalIdentity::generate("discovery@local"));
        BuyerExecutor {
            budget: DurableBudget::open(root, &identity.agent_id().pubkey_base58(), endpoint, 1000)
                .unwrap(),
            identity,
            config: BuyerConfig {
                coordinator_url: endpoint.into(),
                poll_interval: Duration::from_millis(1),
                referral_code: None,
                rpc_url: None,
            },
            http: reqwest::Client::new(),
            model: "model".into(),
            offer_micro_usdc: 30,
            max_tokens: 64,
            max_input_bytes: 4096,
            approved_operators: vec!["not-an-aggregate-identity".into()],
            tariffs: Some(Arc::new(TariffBook::new(vec![tariff(now)]).unwrap())),
        }
    }
    fn capacity(now: u64, legacy_ask: u64) -> CapacityView {
        CapacityView {
            registered_operators: 1,
            matchable_operators: 1,
            liveness_window_ms: 90_000,
            min_score_bps: 0,
            min_bond_micro_usdc: 0,
            entries: vec![CapacityEntry {
                kind: JobKind::InferenceCall,
                model: "model:latest".into(),
                operators: 1,
                min_ask_micro_usdc: legacy_ask,
                max_ask_micro_usdc: legacy_ask,
                min_ask_unit: PriceUnit::PerJob,
                max_vram_gb: 24,
                gpu_classes: vec!["gpu".into()],
                tee_capable: false,
                inference_tariffs: vec![InferenceTariffOffer {
                    tariff: tariff(now).native,
                    gpu_class: "gpu".into(),
                    vram_gb: 24,
                }],
            }],
        }
    }
    fn request(cap: u32, priced: bool) -> Value {
        let mut request = json!({"model":"model","messages":[{"role":"user","content":"offline fixture"}],"max_tokens":cap});
        if priced {
            request["pricing"] = json!("model_tariff_v1");
        }
        request
    }
    #[test]
    fn exact_tariff_supply_is_independent_of_legacy_ask_and_bounded_by_budget() {
        let root = tempfile::tempdir().unwrap();
        let executor = executor(root.path(), "http://127.0.0.1:1", 100);
        let view = capacity(100, 999);
        let offers = executor.offers_from_capacity(&view, 100, 50, 120);
        assert_eq!(
            offers.len(),
            1,
            "high legacy ask must not hide an exact signed tariff"
        );
        assert!(matches!(
            offers[0].pricing,
            DiscoveryPricing::ModelTariff { .. }
        ));
        assert_eq!(offers[0].max_output_tokens, 20);
        assert!(offers[0].matches_request(&request(20, true)));
        assert!(!offers[0].matches_request(&request(21, true)));
        assert_eq!(offers[0].native_cost(20).unwrap(), 120);
        assert!(executor
            .offers_from_capacity(&view, 100, 50, 100)
            .is_empty());
        let mut changed = view.clone();
        changed.entries[0].inference_tariffs[0].tariff.revision += 1;
        assert!(
            executor
                .offers_from_capacity(&changed, 100, 50, 1000)
                .is_empty(),
            "same price/model with a different revision is not an approved tariff"
        );
        changed.entries[0].min_ask_micro_usdc = 30;
        let offers = executor.offers_from_capacity(&changed, 100, 50, 1000);
        assert_eq!(offers.len(), 1);
        assert!(matches!(offers[0].pricing, DiscoveryPricing::Legacy { .. }));
        let mut unavailable = capacity(100, 30);
        unavailable.matchable_operators = 0;
        assert!(executor
            .offers_from_capacity(&unavailable, 100, 50, 1000)
            .is_empty());
        unavailable.matchable_operators = 1;
        unavailable.entries[0].operators = 0;
        assert!(executor
            .offers_from_capacity(&unavailable, 100, 50, 1000)
            .is_empty());
        unavailable.entries[0].operators = 1;
        unavailable.entries[0].model = "other".into();
        assert!(executor
            .offers_from_capacity(&unavailable, 100, 50, 1000)
            .is_empty());
    }
    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, task)
    }
    #[tokio::test]
    async fn discovery_refetches_capacity_and_checks_remaining_budget_without_creating_a_journal() {
        let now = crate::now_ms();
        let state = Arc::new(Mutex::new(capacity(now, 30)));
        let app = Router::new()
            .route(
                "/federation/capacity",
                get(|State(state): State<Arc<Mutex<CapacityView>>>| async move {
                    Json(state.lock().unwrap().clone())
                }),
            )
            .with_state(state.clone());
        let (base, server) = serve(app).await;
        let root = tempfile::tempdir().unwrap();
        let executor = executor(root.path(), &base, now);
        let namespace = std::fs::read_dir(root.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let offers = executor.discovery_offers(now, 50).await.unwrap();
        assert_eq!(offers.len(), 2);
        assert_eq!(
            std::fs::read_dir(&namespace).unwrap().count(),
            0,
            "free reads cannot create a budget or lock"
        );
        assert!(executor
            .check_available(&request(21, true), now, 50, 120)
            .await
            .is_err());
        assert!(executor
            .check_available(&request(20, true), now, 50, 120)
            .await
            .is_ok());
        state.lock().unwrap().matchable_operators = 0;
        assert!(
            executor.discovery_offers(now, 50).await.unwrap().is_empty(),
            "no stale cached offer"
        );
        assert!(executor
            .check_available(&request(1, false), now, 50, 1000)
            .await
            .is_err());
        assert_eq!(std::fs::read_dir(&namespace).unwrap().count(), 0);
        server.abort();
    }
    #[tokio::test]
    async fn invalid_requests_fail_before_capacity_or_budget_access() {
        let now = crate::now_ms();
        let hits = Arc::new(AtomicUsize::new(0));
        let count = hits.clone();
        let app = Router::new().route(
            "/federation/capacity",
            get(move |headers: axum::http::HeaderMap| {
                let count = count.clone();
                async move {
                    assert_eq!(headers["cache-control"], "no-cache");
                    assert_eq!(headers["pragma"], "no-cache");
                    count.fetch_add(1, Ordering::SeqCst);
                    Json(capacity(now, 30))
                }
            }),
        );
        let (base, server) = serve(app).await;
        let root = tempfile::tempdir().unwrap();
        let executor = executor(root.path(), &base, now);
        let namespace = std::fs::read_dir(root.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        // A malformed durable journal proves syntax refusal does not even read
        // budget state. A valid request reaches the capacity and budget gates.
        let journal = namespace.join("budget.json");
        std::fs::write(&journal, b"{torn").unwrap();
        let mut invalid = vec![json!({}), request(0, false), request(65, true)];
        for (field, value) in [
            ("model", json!("wrong")),
            ("pricing", Value::Null),
            ("pricing", json!("wrong")),
            ("temperature", json!(-0.1)),
            ("temperature", json!(2.1)),
            ("stream", json!(true)),
            ("messages", json!([])),
            ("messages", json!([{"role":"tool","content":"text"}])),
            ("messages", json!([{"role":"user","content":""}])),
            (
                "messages",
                json!([{"role":"user","content":"x".repeat(4096)}]),
            ),
            ("unknown", json!(true)),
        ] {
            let mut value_request = request(1, false);
            value_request[field] = value;
            invalid.push(value_request);
        }
        let mut too_many = request(1, false);
        too_many["messages"] = json!(vec![json!({"role":"user","content":"text"}); 33]);
        invalid.push(too_many);
        for value in invalid {
            assert!(matches!(
                executor.validate_request(&value),
                Err(GatewayError::Invalid(_))
            ));
            assert!(matches!(
                executor.prepare(&value),
                Err(GatewayError::Invalid(_))
            ));
            assert!(matches!(
                executor.check_available(&value, now, 50, 1000).await,
                Err(GatewayError::Invalid(_))
            ));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        let valid = request(1, true);
        assert!(executor.validate_request(&valid).is_ok());
        assert!(matches!(
            executor.check_available(&valid, now, 50, 1000).await,
            Err(GatewayError::Unavailable(_))
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(&journal).unwrap(), b"{torn");
        assert_eq!(std::fs::read_dir(&namespace).unwrap().count(), 1);
        server.abort();
    }
    #[test]
    fn x402scan_sample_passes_the_real_buyer_request_validator() {
        // Captured from x402scan 5732bb83's sampler and discovery 1.7.5,
        // using the OpenAPI fixture emitted by discovery_tests.rs.
        let request: Value =
            serde_json::from_str(include_str!("../testdata/x402scan-request.json")).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut executor = executor(root.path(), "http://127.0.0.1:1", crate::now_ms());
        executor.model = "example".into();
        executor.max_tokens = 2048;
        assert!(executor.validate_request(&request).is_ok());
        assert!(executor.validate_request(&json!({})).is_err());
        let mut legacy = request;
        legacy.as_object_mut().unwrap().remove("pricing");
        assert!(executor.validate_request(&legacy).is_ok());
    }
    #[tokio::test]
    async fn capacity_wait_counts_against_tariff_delivery_margin() {
        let now = crate::now_ms();
        let mut row = tariff(now);
        row.native.valid_until_ms = now + DELIVERY_MARGIN_MS + 15;
        row.retail.valid_until_ms = row.native.valid_until_ms;
        let mut view = capacity(now, 999);
        view.entries[0].inference_tariffs[0].tariff = row.native.clone();
        let app = Router::new().route(
            "/federation/capacity",
            get(move || {
                let view = view.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Json(view)
                }
            }),
        );
        let (base, server) = serve(app).await;
        let root = tempfile::tempdir().unwrap();
        let mut executor = executor(root.path(), &base, now);
        executor.tariffs = Some(Arc::new(TariffBook::new(vec![row]).unwrap()));
        assert!(executor.discovery_offers(now, 50).await.unwrap().is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn capacity_refuses_redirects_oversized_malformed_and_slow_responses() {
        let hits = Arc::new(AtomicUsize::new(0));
        let count = hits.clone();
        let app = Router::new()
            .route(
                "/redirect",
                get(|| async { (StatusCode::TEMPORARY_REDIRECT, [("location", "/target")]) }),
            )
            .route(
                "/target",
                get(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        "{}"
                    }
                }),
            )
            .route(
                "/large",
                get(|| async { "x".repeat(CAPACITY_MAX_BYTES + 1) }),
            )
            .route("/invalid", get(|| async { "{torn" }))
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    "{}"
                }),
            );
        let (base, server) = serve(app).await;
        for route in ["redirect", "large", "invalid"] {
            assert!(
                read_capacity(&format!("{base}/{route}"), CAPACITY_TIMEOUT)
                    .await
                    .is_err(),
                "{route}"
            );
        }
        assert!(
            read_capacity(&format!("{base}/slow"), Duration::from_millis(10))
                .await
                .is_err()
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "redirect must never be followed"
        );
        server.abort();
    }
}
