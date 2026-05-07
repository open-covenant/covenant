//! HTTP gateway in front of `Server::respond`.
//!
//! Same `Server` instance, two transports: the Unix socket for the local
//! CLI, and HTTP for browser-facing UIs (Phase 4 web UI, plus any
//! third-party tooling). Bound to `127.0.0.1` by default — there is no
//! authentication yet beyond "you can reach the loopback interface". Phase
//! 5 will gate the HTTP surface behind capability tokens.

#![allow(clippy::needless_pass_by_value)]

use crate::Server;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use covenant_ipc::{Request, Response};
use covenant_types::MemoryTier;
use serde::Deserialize;

#[derive(Clone)]
pub struct HttpState {
    pub server: Server,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health))
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
        .route("/a2a/tasks", post(send_a2a_task))
        .route("/a2a/tasks/next", get(try_recv_a2a_task))
        .route("/a2a/results", post(post_a2a_result))
        .route("/a2a/results/next", get(try_recv_a2a_result))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
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
    Json(b): Json<SubmitIntentBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::SubmitIntent { text: b.text })
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
    Query(q): Query<RecentParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::RecentMemory {
                tier: q.tier,
                limit: q.limit.unwrap_or(10),
            })
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
    Query(q): Query<SearchParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::SearchMemory {
                query: q.q,
                tier: q.tier,
                limit: q.limit.unwrap_or(10),
            })
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
    Json(b): Json<PurgeBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::PurgeMemory {
                tier: b.tier,
                before_ms: b.before_ms,
            })
            .await,
    ))
}

#[derive(Deserialize, Default)]
struct VerifyParams {
    window: Option<usize>,
}

async fn verify(
    State(s): State<HttpState>,
    Query(q): Query<VerifyParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::Verify {
                window: q.window.unwrap_or(100),
            })
            .await,
    ))
}

#[derive(Deserialize, Default)]
struct LimitParams {
    limit: Option<usize>,
}

async fn receipts_recent(
    State(s): State<HttpState>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::RecentReceipts {
                limit: q.limit.unwrap_or(10),
            })
            .await,
    ))
}

async fn capabilities_recent(
    State(s): State<HttpState>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::RecentCapabilities {
                limit: q.limit.unwrap_or(10),
            })
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
    Json(b): Json<GrantBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::GrantCapability {
                action: b.action,
                scope: b.scope,
                expires_at: b.expires_at,
            })
            .await,
    ))
}

#[derive(Deserialize)]
struct RevokeBody {
    signature_b58: String,
}

async fn revoke_capability(
    State(s): State<HttpState>,
    Json(b): Json<RevokeBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::RevokeCapability {
                signature_b58: b.signature_b58,
            })
            .await,
    ))
}

async fn list_tools(State(s): State<HttpState>) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::ListTools).await))
}

async fn audit_recent(
    State(s): State<HttpState>,
    Query(q): Query<LimitParams>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::RecentAudit {
                limit: q.limit.unwrap_or(20),
            })
            .await,
    ))
}

async fn send_a2a_task(
    State(s): State<HttpState>,
    Json(task): Json<covenant_a2a::A2ATask>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::SendA2ATask { task }).await))
}

async fn try_recv_a2a_task(State(s): State<HttpState>) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::TryRecvA2ATask).await))
}

async fn post_a2a_result(
    State(s): State<HttpState>,
    Json(result): Json<covenant_a2a::A2ATaskResult>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server.respond(Request::PostA2AResult { result }).await,
    ))
}

async fn try_recv_a2a_result(State(s): State<HttpState>) -> Result<Json<Response>, ApiError> {
    Ok(Json(s.server.respond(Request::TryRecvA2AResult).await))
}

#[derive(Deserialize)]
struct CallToolBody {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

async fn call_tool(
    State(s): State<HttpState>,
    Json(b): Json<CallToolBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(Request::CallTool {
                name: b.name,
                arguments: b.arguments,
            })
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
