//! HTTP gateway in front of `Server::respond`.
//!
//! Same `Server` instance, two transports: the Unix socket for the local
//! CLI, and HTTP for browser-facing UIs (Phase 4 web UI, plus any
//! third-party tooling). Bound to `127.0.0.1` by default. Every route
//! except `/health` requires a `Authorization: Bearer <token>` header
//! whose token resolves to a registered peer through the
//! [`covenant_peer_auth::PeerRegistry`] the daemon was constructed
//! with — same registry that gates the Unix-socket `Authenticate`
//! handshake.

#![allow(clippy::needless_pass_by_value)]

use crate::Server;
use axum::{
    extract::{Extension, Query, Request as AxumRequest, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response as AxumResponse},
    routing::{get, post},
    Json, Router,
};
use covenant_ipc::{Request, Response};
use covenant_peer_auth::PeerToken;
use covenant_types::{AgentId, MemoryTier};
use serde::Deserialize;

#[derive(Clone)]
pub struct HttpState {
    pub server: Server,
}

pub fn router(state: HttpState) -> Router {
    let protected = Router::new()
        .route("/intent", post(submit_intent))
        .route("/memory/recent", get(memory_recent))
        .route("/memory/search", get(memory_search))
        .route("/memory/purge", post(memory_purge))
        .route("/verify", get(verify))
        .route("/receipts/recent", get(receipts_recent))
        .route("/capabilities/recent", get(capabilities_recent))
        .route("/capabilities/grant", post(grant_capability))
        .route("/capabilities/revoke", post(revoke_capability))
        .route("/tools", get(list_tools))
        .route("/tools/call", post(call_tool))
        .route("/audit/recent", get(audit_recent))
        .route("/audit/purge", post(audit_purge))
        .route("/capabilities/purge", post(capabilities_purge))
        .route("/a2a/tasks", post(send_a2a_task))
        .route("/a2a/tasks/next", get(try_recv_a2a_task))
        .route("/a2a/tasks/recent", get(recent_a2a_tasks))
        .route("/a2a/results", post(post_a2a_result))
        .route("/a2a/results/next", get(try_recv_a2a_result))
        .route("/a2a/results/recent", get(recent_a2a_results))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        .with_state(state.clone());

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(tower_http::cors::CorsLayer::permissive())
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
    s.server.record_auth_failure("http", message).await;
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "kind": "error", "message": message })),
    )
        .into_response()
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
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
                },
                &peer,
            )
            .await,
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
