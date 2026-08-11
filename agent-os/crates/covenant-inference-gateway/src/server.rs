use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use covenant_inference_protocol::{
    receipt_hash, InferenceReceiptPayload, NodeEnrollment, NodeTelemetry, TrustClass,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;

use crate::pool::{PooledStream, TunnelPool};
use crate::proxy;
use crate::receipts::{self, ReceiptRecord, ReceiptStore};
use crate::registry::{NodeRegistry, RegistryError};
use crate::x402::{
    decode_payment, PaymentContext, PaymentReceipt, PaymentVerifier, SolanaUsdcVerifier,
};

/// Matches the node surface: a single chat turn or embedding batch buffers before it
/// is forwarded, so this bounds the buffer without capping any realistic request.
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const OWNED_BY: &str = "covenant-inference-gateway";

/// How the gateway reaches a chosen node. Production parks reverse mTLS tunnels;
/// the direct variant proxies straight to a node's OpenAI surface for local runs.
#[derive(Clone)]
pub enum Transport {
    Tunnel(TunnelPool),
    Direct(Arc<str>),
}

impl Transport {
    async fn acquire(&self, node_id: &str) -> Option<(PooledStream, bool)> {
        match self {
            Self::Tunnel(pool) => pool.take(node_id).map(|stream| (stream, true)),
            Self::Direct(addr) => {
                let stream = TcpStream::connect(addr.as_ref()).await.ok()?;
                stream.set_nodelay(true).ok()?;
                Some((Box::new(stream), false))
            }
        }
    }

    fn has_connection(&self, node_id: &str) -> bool {
        match self {
            Self::Tunnel(pool) => pool.has(node_id),
            Self::Direct(_) => true,
        }
    }
}

/// Per-request metering policy. With `require_payment` off the gateway still emits
/// receipts (accounting mode); with it on, the paid routes gate on a verified
/// `X-PAYMENT` and the priced rail is what the `402` advertises.
#[derive(Clone, Default)]
pub struct Metering {
    pub require_payment: bool,
    pub price_micro: u64,
    pub pay_to: String,
    pub rail_network: String,
    pub rail_asset: String,
}

#[derive(Clone)]
pub struct Gateway {
    registry: Arc<NodeRegistry>,
    transport: Transport,
    round_robin: Arc<AtomicUsize>,
    metering: Arc<Metering>,
    verifier: Arc<dyn PaymentVerifier>,
    receipts: Arc<ReceiptStore>,
}

impl Gateway {
    /// Accounting mode by default: no payment gate, receipts in memory. The default
    /// verifier is the gated real-money seam, so flipping payment on without wiring a
    /// verifier refuses every payment rather than accepting it.
    pub fn new(registry: Arc<NodeRegistry>, transport: Transport) -> Self {
        Self {
            registry,
            transport,
            round_robin: Arc::new(AtomicUsize::new(0)),
            metering: Arc::new(Metering::default()),
            verifier: Arc::new(SolanaUsdcVerifier::gated()),
            receipts: Arc::new(ReceiptStore::in_memory()),
        }
    }

    pub fn with_metering(mut self, metering: Metering) -> Self {
        self.metering = Arc::new(metering);
        self
    }

    pub fn with_verifier(mut self, verifier: Arc<dyn PaymentVerifier>) -> Self {
        self.verifier = verifier;
        self
    }

    pub fn with_receipts(mut self, receipts: Arc<ReceiptStore>) -> Self {
        self.receipts = receipts;
        self
    }

    fn payment_context(&self, path: &str) -> PaymentContext {
        PaymentContext {
            resource: path.to_owned(),
            price_micro: self.metering.price_micro,
            pay_to: self.metering.pay_to.clone(),
            network: self.metering.rail_network.clone(),
            asset: self.metering.rail_asset.clone(),
        }
    }

    /// Resolves a request's payment. `Allowed(None)` is accounting mode — serve, but
    /// charge nothing; `Denied` carries the `402` (or malformed-payment) response to
    /// return as-is.
    fn authorize(&self, path: &str, headers: &HeaderMap) -> Authorization {
        if !self.metering.require_payment {
            return Authorization::Allowed(None);
        }
        let ctx = self.payment_context(path);
        let Some(header) = headers.get("x-payment").and_then(|v| v.to_str().ok()) else {
            return Authorization::Denied(challenge(&ctx, "payment required"));
        };
        let Ok(payload) = decode_payment(header) else {
            return Authorization::Denied(challenge(&ctx, "malformed payment"));
        };
        match self.verifier.verify(&payload, &ctx) {
            Ok(receipt) => Authorization::Allowed(Some(receipt)),
            Err(error) => Authorization::Denied(challenge(&ctx, &error.to_string())),
        }
    }

    fn refund(&self, payment: &Option<PaymentReceipt>) {
        if let Some(receipt) = payment {
            self.verifier.refund(receipt);
        }
    }

    /// Commits to a served turn: hashes the response, builds the receipt over the
    /// model, prompt and output, records it, and returns its hash for the response
    /// header. Only hashes and accounting metadata are retained.
    fn emit_receipt(
        &self,
        digest: &str,
        node_id: &str,
        model: &str,
        prompt_hash: String,
        response_body: &Bytes,
        payment: Option<&PaymentReceipt>,
    ) -> String {
        let output_hash = receipts::hash_bytes(response_body);
        let timestamp = receipts::now_unix();
        let payload = InferenceReceiptPayload {
            model_identity_digest: digest.to_owned(),
            prompt_hash,
            output_hash,
            node_id: node_id.to_owned(),
            timestamp,
        };
        let hash = receipt_hash(&payload).expect("receipt payload serializes");
        self.receipts.append(ReceiptRecord {
            receipt_hash: hash.clone(),
            model_identity_digest: payload.model_identity_digest,
            prompt_hash: payload.prompt_hash,
            output_hash: payload.output_hash,
            node_id: payload.node_id,
            model: model.to_owned(),
            timestamp,
            amount_micro: payment.map(|p| p.amount_micro).unwrap_or(0),
            payment_reference: payment.map(|p| p.reference.clone()),
        });
        hash
    }
}

enum Authorization {
    Allowed(Option<PaymentReceipt>),
    Denied(Response),
}

fn challenge(ctx: &PaymentContext, error: &str) -> Response {
    (StatusCode::PAYMENT_REQUIRED, Json(ctx.challenge(error))).into_response()
}

pub fn router(gateway: Gateway) -> Router {
    Router::new()
        .route("/v1/nodes/enroll", post(enroll))
        .route("/v1/nodes/{id}/heartbeat", post(heartbeat))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/models", get(models))
        .route("/v1/receipts", get(receipts_recent))
        .route("/v1/receipts/{hash}", get(receipt_by_hash))
        .route("/health", get(health))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(gateway)
}

async fn enroll(
    State(gateway): State<Gateway>,
    Json(enrollment): Json<NodeEnrollment>,
) -> Result<StatusCode, ApiError> {
    gateway.registry.enroll(enrollment)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn heartbeat(
    State(gateway): State<Gateway>,
    Path(id): Path<String>,
    Json(telemetry): Json<NodeTelemetry>,
) -> Result<StatusCode, ApiError> {
    gateway.registry.heartbeat(&id, telemetry)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn chat_completions(
    State(gateway): State<Gateway>,
    Query(trust): Query<TrustQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    route(gateway, "/v1/chat/completions", trust, headers, body).await
}

async fn embeddings(
    State(gateway): State<Gateway>,
    Query(trust): Query<TrustQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    route(gateway, "/v1/embeddings", trust, headers, body).await
}

async fn route(
    gateway: Gateway,
    path: &str,
    trust: TrustQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(model) = model_of(&body) else {
        return (StatusCode::BAD_REQUEST, "request body must name a model").into_response();
    };
    let min_trust = trust.resolve(&headers);

    let payment = match gateway.authorize(path, &headers) {
        Authorization::Allowed(payment) => payment,
        Authorization::Denied(response) => return response,
    };

    // Committed before the body is handed off to the proxy; only the hash survives.
    let prompt_hash = receipts::hash_bytes(&prompt_commitment(&body));

    let Some(node) = choose(&gateway, &model, min_trust).await else {
        gateway.refund(&payment);
        return no_match(&model, min_trust);
    };
    let Some((stream, relay)) = gateway.transport.acquire(&node).await else {
        // The connection raced out between selection and acquisition; treat it as no
        // capacity rather than a hard error.
        gateway.refund(&payment);
        return no_match(&model, min_trust);
    };
    let digest = gateway
        .registry
        .model_digest(&node, &model)
        .unwrap_or_default();

    match proxy::relay_collect(stream, relay, Method::POST, path, &headers, body).await {
        Ok(collected) if collected.status.is_success() => {
            let hash = gateway.emit_receipt(
                &digest,
                &node,
                &model,
                prompt_hash,
                &collected.body,
                payment.as_ref(),
            );
            let mut response = collected.into_response();
            if let Ok(value) = HeaderValue::from_str(&hash) {
                response.headers_mut().insert("x-covenant-receipt", value);
            }
            response
        }
        Ok(collected) => {
            gateway.refund(&payment);
            collected.into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "relay request to node");
            gateway.refund(&payment);
            (StatusCode::BAD_GATEWAY, "node request failed").into_response()
        }
    }
}

/// The bytes a receipt's `prompt_hash` commits to: the request's `messages` (chat)
/// or `input` (embeddings), re-serialized canonically. Falls back to the whole body
/// when neither is present. `serde_json::Value` maps are ordered, so the same turn
/// always hashes the same way.
fn prompt_commitment(body: &Bytes) -> Vec<u8> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    match value.get("messages").or_else(|| value.get("input")) {
        Some(field) => serde_json::to_vec(field).unwrap_or_else(|_| body.to_vec()),
        None => body.to_vec(),
    }
}

async fn receipts_recent(
    State(gateway): State<Gateway>,
    Query(query): Query<RecentQuery>,
) -> Json<ReceiptsPage> {
    let limit = query
        .limit
        .unwrap_or(100)
        .min(receipts::DEFAULT_RING_CAPACITY);
    Json(ReceiptsPage {
        object: "list",
        data: gateway.receipts.recent(limit),
    })
}

async fn receipt_by_hash(State(gateway): State<Gateway>, Path(hash): Path<String>) -> Response {
    match gateway.receipts.get(&hash) {
        Some(record) => Json(record).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: "unknown receipt".to_owned(),
            }),
        )
            .into_response(),
    }
}

/// Picks a node for `model` at the trust floor. A first pass uses cached catalogs;
/// on a miss it re-probes online nodes and tries once more, so a just-enrolled node
/// becomes routable without a warm-up request.
async fn choose(gateway: &Gateway, model: &str, min_trust: TrustClass) -> Option<String> {
    if let Some(node) = pick(gateway, model, min_trust) {
        return Some(node);
    }
    refresh_catalogs(gateway).await;
    pick(gateway, model, min_trust)
}

fn pick(gateway: &Gateway, model: &str, min_trust: TrustClass) -> Option<String> {
    let mut candidates: Vec<_> = gateway
        .registry
        .candidates(model, min_trust, Instant::now())
        .into_iter()
        .filter(|candidate| gateway.transport.has_connection(&candidate.node_id))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Cheapest rate wins; ties rotate round-robin over a stable order so a burst
    // spreads across equally-priced nodes instead of hammering one.
    let cheapest = candidates.iter().map(|c| c.rate_per_second).min()?;
    candidates.retain(|c| c.rate_per_second == cheapest);
    candidates.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    let index = gateway.round_robin.fetch_add(1, Ordering::Relaxed) % candidates.len();
    Some(candidates[index].node_id.clone())
}

async fn refresh_catalogs(gateway: &Gateway) {
    for node in gateway.registry.needs_catalog(Instant::now()) {
        let Some((stream, relay)) = gateway.transport.acquire(&node).await else {
            continue;
        };
        match proxy::fetch_catalog(stream, relay).await {
            Ok(catalog) => gateway.registry.set_catalog(&node, catalog),
            Err(error) => tracing::warn!(node_id = %node, %error, "catalog probe failed"),
        }
    }
}

async fn models(State(gateway): State<Gateway>) -> Json<ModelList> {
    refresh_catalogs(&gateway).await;
    let mut seen = HashSet::new();
    let data = gateway
        .registry
        .catalog_union(Instant::now())
        .into_iter()
        .filter(|entry| seen.insert(entry.digest.clone()))
        .map(|entry| ModelEntry {
            id: entry.id,
            object: "model",
            owned_by: OWNED_BY,
            model_identity_digest: entry.digest,
        })
        .collect();
    Json(ModelList {
        object: "list",
        data,
    })
}

async fn health(State(gateway): State<Gateway>) -> Json<Health> {
    let now = Instant::now();
    let online_nodes = gateway
        .registry
        .online_nodes(now)
        .into_iter()
        .filter(|node| gateway.transport.has_connection(node))
        .count();
    Json(Health {
        status: "ok",
        online_nodes,
    })
}

fn model_of(body: &Bytes) -> Option<String> {
    #[derive(Deserialize)]
    struct ModelField {
        model: String,
    }
    serde_json::from_slice::<ModelField>(body)
        .ok()
        .map(|field| field.model)
}

fn no_match(model: &str, min_trust: TrustClass) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!(
            "no online node serves model {model:?} at trust floor {}",
            min_trust.label()
        ),
    )
        .into_response()
}

#[derive(Deserialize)]
struct RecentQuery {
    limit: Option<usize>,
}

#[derive(Serialize)]
struct ReceiptsPage {
    object: &'static str,
    data: Vec<ReceiptRecord>,
}

#[derive(Deserialize, Default)]
struct TrustQuery {
    min_trust: Option<String>,
}

impl TrustQuery {
    /// `?min_trust=` wins over `X-Min-Trust`; an absent or unrecognised value falls
    /// back to the weakest class so a caller that omits it gets the open network.
    fn resolve(&self, headers: &HeaderMap) -> TrustClass {
        self.min_trust
            .as_deref()
            .or_else(|| {
                headers
                    .get("x-min-trust")
                    .and_then(|value| value.to_str().ok())
            })
            .and_then(parse_trust)
            .unwrap_or_default()
    }
}

fn parse_trust(value: &str) -> Option<TrustClass> {
    match value.trim().to_ascii_lowercase().as_str() {
        "open" => Some(TrustClass::Open),
        "isolated" => Some(TrustClass::Isolated),
        "attested" => Some(TrustClass::Attested),
        "confidential" => Some(TrustClass::Confidential),
        _ => None,
    }
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

impl From<RegistryError> for ApiError {
    fn from(error: RegistryError) -> Self {
        let status = match error {
            RegistryError::UnknownNode => StatusCode::NOT_FOUND,
            RegistryError::Replay => StatusCode::CONFLICT,
            RegistryError::Stale => StatusCode::BAD_REQUEST,
            RegistryError::IdentityMismatch
            | RegistryError::BadKey
            | RegistryError::Protocol(_) => StatusCode::UNAUTHORIZED,
        };
        Self {
            status,
            message: error.to_string(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelEntry>,
}

#[derive(Serialize)]
struct ModelEntry {
    id: String,
    object: &'static str,
    owned_by: &'static str,
    model_identity_digest: String,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    online_nodes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_floor_prefers_query_then_header_then_open() {
        let mut headers = HeaderMap::new();
        headers.insert("x-min-trust", "attested".parse().unwrap());

        let query = TrustQuery {
            min_trust: Some("isolated".into()),
        };
        assert_eq!(query.resolve(&headers), TrustClass::Isolated);

        let header_only = TrustQuery { min_trust: None };
        assert_eq!(header_only.resolve(&headers), TrustClass::Attested);

        assert_eq!(
            TrustQuery { min_trust: None }.resolve(&HeaderMap::new()),
            TrustClass::Open
        );
        assert_eq!(
            TrustQuery {
                min_trust: Some("nonsense".into())
            }
            .resolve(&HeaderMap::new()),
            TrustClass::Open
        );
    }

    #[test]
    fn model_is_read_from_the_request_body() {
        assert_eq!(
            model_of(&Bytes::from_static(br#"{"model":"qwen3:8b"}"#)).as_deref(),
            Some("qwen3:8b")
        );
        assert!(model_of(&Bytes::from_static(b"{}")).is_none());
    }
}
