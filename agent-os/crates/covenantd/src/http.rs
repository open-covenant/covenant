//! HTTP gateway in front of `Server::respond`.
//!
//! Same `Server` instance, two transports: the Unix socket for the local
//! CLI, and HTTP for browser-facing UIs (Phase 4 web UI, plus any
//! third-party tooling). Bound to `127.0.0.1` by default. Every route
//! except `/health` requires a `Authorization: Bearer <token>` header
//! whose token resolves to a registered peer through the
//! [`covenant_peer_auth::PeerRegistry`] the daemon was constructed
//! with — same registry that gates the Unix-socket `Authenticate`
//! handshake. `/health` and `/version` are intentionally unauthenticated
//! so supervisors and clients can check liveness and wire compatibility
//! before presenting credentials.
//!
//! CORS: explicit origin allow-list, default `http://localhost:3000`.
//! Override via `COVENANT_HTTP_ORIGINS` (comma-separated list of
//! origins). The bearer-auth check still gates every request, so a
//! permissive CORS would not by itself authorise a malicious site —
//! but tightening defends against browser-side attacks where the
//! malicious site already holds a leaked bearer token (e.g., XSS in
//! the operator's web UI).

#![allow(clippy::needless_pass_by_value)]

use crate::Server;
use axum::{
    extract::{Extension, Query, Request as AxumRequest, State},
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        HeaderValue, Method, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response as AxumResponse},
    routing::{get, post},
    Json, Router,
};
use covenant_ipc::{protocol_info, Request, Response};
use covenant_peer_auth::PeerToken;
use covenant_types::{AgentId, MemoryTier};
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Clone)]
pub struct HttpState {
    pub server: Server,
}

/// Default CORS origin when `COVENANT_HTTP_ORIGINS` is unset. Matches
/// the Next.js `pnpm dev` default port; operators with a different web
/// UI deployment override via env.
const DEFAULT_CORS_ORIGIN: &str = "http://localhost:3000";

/// Parse a comma-separated origin list into `Vec<HeaderValue>`. Pure
/// over the input string — no env reads — so tests can drive every
/// branch without touching process-global state.
///
/// Behaviour:
/// - `None` or whitespace-only `Some(_)` → fall back to
///   [`DEFAULT_CORS_ORIGIN`].
/// - Mixed valid/invalid entries → keep the valid ones; an invalid
///   origin that a real browser couldn't send anyway shouldn't kill
///   the daemon. A `tracing::warn!` names the dropped count so the
///   operator notices a typo.
/// - Every entry invalid → fall back to [`DEFAULT_CORS_ORIGIN`] with a
///   distinct warn. An empty allow-list combined with
///   `allow_credentials(true)` would reject every cross-origin request.
fn cors_origins_from_value(value: Option<&str>) -> Vec<HeaderValue> {
    let Some(raw) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return vec![HeaderValue::from_static(DEFAULT_CORS_ORIGIN)];
    };
    let entries: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let parsed: Vec<HeaderValue> = entries
        .iter()
        .filter_map(|s| HeaderValue::from_str(s).ok())
        .collect();
    if parsed.is_empty() {
        tracing::warn!(
            env = "COVENANT_HTTP_ORIGINS",
            value = raw,
            "every entry in COVENANT_HTTP_ORIGINS failed to parse as a valid Origin header; falling back to {}",
            DEFAULT_CORS_ORIGIN
        );
        return vec![HeaderValue::from_static(DEFAULT_CORS_ORIGIN)];
    }
    let dropped = entries.len() - parsed.len();
    if dropped > 0 {
        tracing::warn!(
            env = "COVENANT_HTTP_ORIGINS",
            kept = parsed.len(),
            dropped,
            "dropped {dropped} invalid entries from COVENANT_HTTP_ORIGINS"
        );
    }
    parsed
}

fn cors_origins_from_env() -> Vec<HeaderValue> {
    cors_origins_from_value(std::env::var("COVENANT_HTTP_ORIGINS").ok().as_deref())
}

fn cors_layer(origins: Vec<HeaderValue>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE])
        .allow_credentials(true)
}

pub fn router(state: HttpState) -> Router {
    router_with_origins(state, cors_origins_from_env())
}

/// Like [`router`] but takes the CORS origin allow-list explicitly.
/// Tests use this to inject deterministic origins without env-var
/// gymnastics.
pub fn router_with_origins(state: HttpState, origins: Vec<HeaderValue>) -> Router {
    let protected = Router::new()
        .route("/intent", post(submit_intent))
        .route("/memory/recent", get(memory_recent))
        .route("/memory/search", get(memory_search))
        .route("/memory/purge", post(memory_purge))
        .route("/memory/repair", post(memory_repair))
        .route("/memory/compact", post(memory_compact))
        .route("/verify", get(verify))
        .route("/receipts/recent", get(receipts_recent))
        .route("/capabilities/recent", get(capabilities_recent))
        .route("/capabilities/grant", post(grant_capability))
        .route("/capabilities/revoke", post(revoke_capability))
        .route("/tools", get(list_tools))
        .route("/tools/call", post(call_tool))
        .route("/audit/recent", get(audit_recent))
        .route("/audit/verify", get(audit_verify))
        .route("/audit/purge", post(audit_purge))
        .route("/capabilities/purge", post(capabilities_purge))
        .route("/a2a/tasks", post(send_a2a_task))
        .route("/a2a/tasks/next", get(try_recv_a2a_task))
        .route("/a2a/tasks/recent", get(recent_a2a_tasks))
        .route("/a2a/results", post(post_a2a_result))
        .route("/a2a/results/next", get(try_recv_a2a_result))
        .route("/a2a/results/recent", get(recent_a2a_results))
        .route("/a2a/queue", get(a2a_queue))
        .route("/a2a/repair", post(repair_a2a_task))
        .route("/a2a/compact", post(compact_a2a))
        .route("/peers/purge", post(peers_purge))
        .route("/peers/rotate", post(peers_rotate))
        .route("/peers/list", get(peers_list))
        .route("/peers/revoke", post(peers_revoke))
        .route("/intents/resume", post(intents_resume))
        .route("/budget/debits", get(budget_debits))
        .route("/chain/status", get(chain_status))
        .route("/chain/flush-receipts", post(chain_flush_receipts))
        .route("/chain/receipt-batches", get(chain_receipt_batches))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        .with_state(state.clone());

    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .merge(protected)
        .layer(cors_layer(origins))
}

async fn require_bearer(
    State(s): State<HttpState>,
    mut req: AxumRequest,
    next: Next,
) -> Result<AxumResponse, AxumResponse> {
    let header = match req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
    {
        Some(h) => h,
        None => return Err(reject(&s, "missing Authorization header").await),
    };
    // RFC 7235 §2.1: scheme is case-insensitive.
    let (scheme, token_b58) = match header.split_once(' ') {
        Some(parts) => parts,
        None => return Err(reject(&s, "expected `Authorization: Bearer <token>`").await),
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(reject(&s, "expected `Authorization: Bearer <token>`").await);
    }
    let token = match PeerToken::from_b58(token_b58.trim()) {
        Ok(t) => t,
        Err(_) => return Err(reject(&s, "malformed bearer token").await),
    };
    match s.server.peers.resolve(&token).await {
        Ok(Some(agent_id)) => {
            req.extensions_mut().insert(agent_id);
            Ok(next.run(req).await)
        }
        _ => Err(reject(&s, "unknown or revoked token").await),
    }
}

async fn reject(s: &HttpState, message: &'static str) -> AxumResponse {
    // Audit-write success is a precondition for the auth-failed response.
    // If the row can't land, return a generic 503 so an attacker who can
    // fill the audit disk does not get a clean rejection while the
    // operator's audit feed silently falls behind reality.
    if s.server.record_auth_failure("http", message).await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "kind": "error",
                "message": "audit write failed; refusing to proceed",
            })),
        )
            .into_response();
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "kind": "error", "message": message })),
    )
        .into_response()
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn version() -> impl IntoResponse {
    Json(Response::ProtocolInfo {
        info: protocol_info(),
    })
}

#[derive(Deserialize)]
struct SubmitIntentBody {
    text: String,
}

async fn submit_intent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<SubmitIntentBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::SubmitIntent { text: b.text }, &peer)
            .await,
    ))
}

#[derive(Deserialize, Default)]
struct RecentParams {
    tier: Option<MemoryTier>,
    limit: Option<usize>,
}

async fn memory_recent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<RecentParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RecentMemory {
                    tier: q.tier,
                    limit: q.limit.unwrap_or(10),
                    prefer_stream: None,
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    tier: Option<MemoryTier>,
    limit: Option<usize>,
    min_relevance: Option<f32>,
}

async fn memory_search(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<SearchParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::SearchMemory {
                    query: q.q,
                    tier: q.tier,
                    limit: q.limit.unwrap_or(10),
                    min_relevance: q.min_relevance,
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Deserialize)]
struct PurgeBody {
    tier: Option<MemoryTier>,
    before_ms: u64,
}

async fn memory_purge(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<PurgeBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::PurgeMemory {
                    tier: b.tier,
                    before_ms: b.before_ms,
                },
                &peer,
            )
            .await,
    ))
}

async fn memory_repair(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(request): Json<covenant_types::MemoryRepairRequest>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::RepairMemory { request }, &peer)
            .await,
    ))
}

async fn memory_compact(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(request): Json<covenant_types::MemoryCompactionRequest>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::CompactMemory { request }, &peer)
            .await,
    ))
}

#[derive(Deserialize, Default)]
struct VerifyParams {
    window: Option<usize>,
}

async fn verify(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<VerifyParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::Verify {
                    window: q.window.unwrap_or(100),
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Deserialize, Default)]
struct LimitParams {
    limit: Option<usize>,
    min_lease_age_ms: Option<u64>,
    deadline_within_ms: Option<u64>,
    state_filter: Option<covenant_a2a::A2ATaskQueueState>,
    since_ms: Option<u64>,
}

async fn receipts_recent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RecentReceipts {
                    limit: q.limit.unwrap_or(10),
                    since_ms: q.since_ms,
                },
                &peer,
            )
            .await,
    ))
}

async fn capabilities_recent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RecentCapabilities {
                    limit: q.limit.unwrap_or(10),
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Deserialize)]
struct GrantBody {
    action: String,
    #[serde(default)]
    scope: Option<serde_json::Value>,
    #[serde(default)]
    expires_at: Option<u64>,
}

async fn grant_capability(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<GrantBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::GrantCapability {
                    action: b.action,
                    scope: b.scope,
                    expires_at: b.expires_at,
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Deserialize)]
struct RevokeBody {
    signature_b58: String,
}

async fn revoke_capability(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<RevokeBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RevokeCapability {
                    signature_b58: b.signature_b58,
                },
                &peer,
            )
            .await,
    ))
}

async fn list_tools(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::ListTools, &peer).await))
}

async fn audit_recent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RecentAudit {
                    limit: q.limit.unwrap_or(20),
                    since_ms: q.since_ms,
                },
                &peer,
            )
            .await,
    ))
}

async fn audit_verify(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server.respond(Request::VerifyAuditIntegrity, &peer).await,
    ))
}

#[derive(Deserialize)]
struct PurgeAuditBody {
    before_ms: u64,
}

async fn audit_purge(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<PurgeAuditBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::PurgeAudit {
                    before_ms: b.before_ms,
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Debug, Deserialize)]
struct PurgeCapabilitiesBody {
    before_ms: u64,
}

async fn capabilities_purge(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<PurgeCapabilitiesBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::PurgeCapabilities {
                    before_ms: b.before_ms,
                },
                &peer,
            )
            .await,
    ))
}

async fn send_a2a_task(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(task): Json<covenant_a2a::A2ATask>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server.respond(Request::SendA2ATask { task }, &peer).await,
    ))
}

async fn try_recv_a2a_task(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::TryRecvA2ATask, &peer).await))
}

async fn post_a2a_result(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(result): Json<covenant_a2a::A2ATaskResult>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::PostA2AResult { result }, &peer)
            .await,
    ))
}

async fn try_recv_a2a_result(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server.respond(Request::TryRecvA2AResult, &peer).await,
    ))
}

async fn recent_a2a_tasks(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RecentA2ATasks {
                    limit: q.limit.unwrap_or(10),
                },
                &peer,
            )
            .await,
    ))
}

async fn recent_a2a_results(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RecentA2AResults {
                    limit: q.limit.unwrap_or(10),
                },
                &peer,
            )
            .await,
    ))
}

async fn a2a_queue(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::A2AQueue {
                    limit: q.limit.unwrap_or(10),
                    min_lease_age_ms: q.min_lease_age_ms,
                    deadline_within_ms: q.deadline_within_ms,
                    state_filter: q.state_filter,
                },
                &peer,
            )
            .await,
    ))
}

async fn repair_a2a_task(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(request): Json<covenant_a2a::A2ARepairRequest>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::RepairA2ATask { request }, &peer)
            .await,
    ))
}

async fn compact_a2a(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::CompactA2A, &peer).await))
}

#[derive(Debug, Deserialize)]
struct PurgePeersBody {
    before_ms: u64,
}

async fn peers_purge(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<PurgePeersBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::PurgePeers {
                    before_ms: b.before_ms,
                },
                &peer,
            )
            .await,
    ))
}

async fn peers_rotate(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server.respond(Request::RotateOperatorToken, &peer).await,
    ))
}

#[derive(Deserialize, Default)]
struct PeersListParams {
    limit: Option<usize>,
    prefix: Option<String>,
    /// `live` / `revoked`. Anything else (or absent) → no status filter.
    /// `serde` rejects unknown variant tags by default, so a typo at the
    /// query layer would 400 the request before it reaches the daemon;
    /// untyped `String` here is permissive — typos degrade to no-filter
    /// rather than an error, matching the rest of the query layer's
    /// "missing field is no filter" posture.
    status: Option<String>,
}
fn parse_status(s: Option<&str>) -> Option<covenant_peer_auth::PeerStatusFilter> {
    match s {
        Some("live") => Some(covenant_peer_auth::PeerStatusFilter::Live),
        Some("revoked") => Some(covenant_peer_auth::PeerStatusFilter::Revoked),
        _ => None,
    }
}

async fn peers_list(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<PeersListParams>,
) -> Result<Json<Response>, ApiError> {
    let status_filter = parse_status(q.status.as_deref());
    Ok(Json(
        s.server
            .respond(
                Request::ListPeers {
                    limit: q.limit.unwrap_or(20),
                    pubkey_prefix: q.prefix,
                    status_filter,
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Debug, Deserialize)]
struct RevokePeerBody {
    token_prefix: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    match_limit: Option<usize>,
}

async fn peers_revoke(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<RevokePeerBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RevokePeer {
                    token_prefix: b.token_prefix,
                    force: b.force,
                    match_limit: b.match_limit,
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Debug, Deserialize)]
struct ResumeIntentBody {
    intent_id: uuid::Uuid,
}

async fn intents_resume(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<ResumeIntentBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::ResumeIntent {
                    intent_id: b.intent_id,
                },
                &peer,
            )
            .await,
    ))
}

async fn budget_debits(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::RecentDebits {
                    limit: q.limit.unwrap_or(20),
                },
                &peer,
            )
            .await,
    ))
}

async fn chain_status(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::ChainStatus, &peer).await))
}

async fn chain_receipt_batches(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::ReceiptBatches {
                    limit: q.limit.unwrap_or(10),
                },
                &peer,
            )
            .await,
    ))
}

async fn chain_flush_receipts(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(q): Json<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::FlushReceipts {
                    limit: q.limit.unwrap_or(10),
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Deserialize)]
struct CallToolBody {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

async fn call_tool(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<CallToolBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::CallTool {
                    name: b.name,
                    arguments: b.arguments,
                },
                &peer,
            )
            .await,
    ))
}

/// All daemon errors over HTTP are 500 with a JSON body. Validation-level
/// problems (missing capabilities, no agent matched) come through as
/// `Response::Error` inside a 200 — same as the Unix socket — so callers
/// get a consistent shape.
pub struct ApiError(pub anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{}", self.0) })),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_parse_status_pins_accepted_spellings_and_permissive_fallback() {
        use covenant_peer_auth::PeerStatusFilter;

        assert_eq!(
            parse_status(Some("live")),
            Some(PeerStatusFilter::Live),
            "the documented ?status=live spelling must resolve to the Live filter so existing HTTP clients keep working",
        );
        assert_eq!(
            parse_status(Some("revoked")),
            Some(PeerStatusFilter::Revoked),
            "the documented ?status=revoked spelling must resolve to the Revoked filter so existing HTTP clients keep working",
        );

        assert_eq!(
            parse_status(None),
            None,
            "an absent ?status= param must mean no filter, not an implicit Live filter, so the default response shape matches what clients see today",
        );
        assert_eq!(
            parse_status(Some("")),
            None,
            "an empty ?status= must degrade to no-filter under the documented permissive query-layer posture",
        );
        assert_eq!(
            parse_status(Some("Live")),
            None,
            "parse_status is case-sensitive by design; tightening to case-insensitive would silently change response shape for clients that send ?status=Live expecting no filter",
        );
        assert_eq!(
            parse_status(Some("unknown")),
            None,
            "unknown status values must degrade to no-filter rather than error, matching the documented permissive query-layer posture for typos",
        );
        assert_eq!(
            parse_status(Some("all")),
            None,
            "a future synonym like ?status=all must NOT be silently accepted; if added, it should be added with an explicit arm and a test, not via a fallthrough match arm",
        );
    }

    #[test]
    fn cors_origins_default_when_value_is_none() {
        let v = cors_origins_from_value(None);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].to_str().unwrap(), DEFAULT_CORS_ORIGIN);
    }

    #[test]
    fn cors_origins_from_env_pins_env_default_and_forward() {
        use std::sync::Mutex;

        static CORS_ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = CORS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let saved = std::env::var("COVENANT_HTTP_ORIGINS").ok();
        std::env::remove_var("COVENANT_HTTP_ORIGINS");

        let defaults = cors_origins_from_env();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].to_str().unwrap(), DEFAULT_CORS_ORIGIN);

        std::env::set_var(
            "COVENANT_HTTP_ORIGINS",
            "http://localhost:3000,https://app.example.com",
        );
        let forwarded = cors_origins_from_env();
        assert_eq!(forwarded.len(), 2);
        assert_eq!(forwarded[0].to_str().unwrap(), "http://localhost:3000");
        assert_eq!(forwarded[1].to_str().unwrap(), "https://app.example.com");

        match saved {
            Some(v) => std::env::set_var("COVENANT_HTTP_ORIGINS", v),
            None => std::env::remove_var("COVENANT_HTTP_ORIGINS"),
        }
    }

    #[test]
    fn cors_origins_default_when_value_is_empty_or_whitespace() {
        let empty = cors_origins_from_value(Some(""));
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].to_str().unwrap(), DEFAULT_CORS_ORIGIN);
        let blank = cors_origins_from_value(Some("   "));
        assert_eq!(blank.len(), 1);
        assert_eq!(blank[0].to_str().unwrap(), DEFAULT_CORS_ORIGIN);
    }

    #[test]
    fn cors_origins_parses_comma_separated() {
        let v = cors_origins_from_value(Some("http://localhost:3000,https://app.example.com"));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].to_str().unwrap(), "http://localhost:3000");
        assert_eq!(v[1].to_str().unwrap(), "https://app.example.com");
    }

    #[test]
    fn cors_origins_falls_back_to_default_when_all_entries_invalid() {
        // An env value where every entry fails `HeaderValue::from_str`
        // would otherwise produce an empty allow-list; combined with
        // `allow_credentials(true)` that rejects every cross-origin
        // request. \n and \r are both rejected by `HeaderValue::from_str`.
        let v = cors_origins_from_value(Some("bad\norigin,also\rbad"));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].to_str().unwrap(), DEFAULT_CORS_ORIGIN);
    }

    #[test]
    fn cors_origins_skips_invalid_but_keeps_valid_entries() {
        let v = cors_origins_from_value(Some("http://localhost:3000,bad\norigin"));
        assert_eq!(v.len(), 1, "the invalid entry is dropped");
        assert_eq!(v[0].to_str().unwrap(), "http://localhost:3000");
    }
}
