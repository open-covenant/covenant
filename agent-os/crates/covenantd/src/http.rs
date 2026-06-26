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
//! before presenting credentials. `POST /a2a/peer-tasks` is also exempt from
//! the bearer gate, but for a different reason: a daemon on another host holds
//! no local token and authenticates by an ed25519 envelope signature instead,
//! so that route is gated by [`Server::admit_remote_a2a_task`] in its own
//! confined router group rather than by [`require_bearer`] (multi-host slice
//! 4b-2b).
//!
//! CORS: explicit origin allow-list, default `http://localhost:3000`.
//! Override via `COVENANT_HTTP_ORIGINS` (comma-separated list of
//! origins). The bearer-auth check still gates every request, so a
//! permissive CORS would not by itself authorise a malicious site —
//! but tightening defends against browser-side attacks where the
//! malicious site already holds a leaked bearer token (e.g., XSS in
//! the operator's web UI).

#![allow(clippy::needless_pass_by_value)]

use crate::{cross_host::RemoteAdmission, sse, Server};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Extension, Path, Query, Request as AxumRequest, State},
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        HeaderMap, HeaderValue, Method, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response as AxumResponse},
    routing::{get, post},
    Json, Router,
};
use covenant_ipc::{protocol_info, Request, Response, MAX_FRAME};
use covenant_peer_auth::PeerToken;
use covenant_runtime::StreamedTrace;
use covenant_types::{AgentEvent, AgentId, MemoryTier};
use futures::stream::StreamExt;
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Clone)]
pub struct HttpState {
    pub server: Server,
    /// Live runtime-trace fan-out sender. `spawn_runtime_event_drainer`
    /// publishes one `StreamedTrace` here per Hermes event when
    /// `COVENANT_LIVE_TRACE=1`. The `/intents/:id/events` handler
    /// subscribes per-request and filters by intent_id.
    pub live_traces_tx: tokio::sync::broadcast::Sender<StreamedTrace>,
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
        .route("/identity/sign", post(identity_sign))
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
        .route("/intents/:id/result", get(intent_result))
        .route("/intents/:id/events", get(intent_events))
        .route("/budget/debits", get(budget_debits))
        .route("/chain/status", get(chain_status))
        .route("/chain/flush-receipts", post(chain_flush_receipts))
        .route("/chain/receipt-batches", get(chain_receipt_batches))
        .route("/sap/stats", get(sap_stats))
        .route("/x402/pay", post(pay_x402_route))
        .route(
            "/settlement/receipts/backfill",
            post(settlement_backfill_receipts),
        )
        .route("/memory/records/backfill", post(memory_backfill_records))
        // Match the IPC frame cap exactly so a payload that flows over
        // the unix socket also flows over HTTP. axum's default body cap
        // is 2 MiB, which silently 413s intents that the IPC daemon
        // would have accepted.
        .layer(DefaultBodyLimit::max(MAX_FRAME as usize))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        .with_state(state.clone());

    // Cross-host A2A inbound admission (multi-host slice 4b-2b). A remote sender
    // holds no local bearer token — it authenticates by the envelope signature —
    // so this route is EXEMPT from require_bearer. It lives in its own router
    // group with NO auth layer and a tighter body cap; because axum layers apply
    // only to the routes of the sub-router they are attached to, the exemption is
    // structurally confined and cannot reach the protected group. The handler
    // hands the raw envelope to the admission core, which fail-closes on every
    // authenticity, authorization, freshness, and replay check — "no bearer"
    // here is not "no authentication".
    let peer_tasks = Router::new()
        .route("/a2a/peer-tasks", post(admit_peer_task))
        .layer(DefaultBodyLimit::max(PEER_TASK_BODY_CAP))
        .with_state(state.clone());

    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .merge(protected)
        .merge(peer_tasks)
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
        Ok(None) => Err(reject(&s, "unknown or revoked token").await),
        Err(e) => {
            // Storage outage on the peer-registry read — distinct from
            // a credential probe. The underlying error (file path,
            // errno, etc.) goes to the operator log; the audit row
            // gets a distinct reason so triage can separate "probe
            // storm" from "registry down"; the wire response uses 503
            // because the fault is server-side, not the client's
            // credential.
            tracing::warn!(error = %e, "peer registry resolve failed during bearer auth");
            let _ = s
                .server
                .record_auth_failure("http", "peer registry unavailable")
                .await;
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "kind": "error",
                    "message": "peer registry unavailable",
                })),
            )
                .into_response())
        }
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

/// Cumulative Synapse-bridge RPC usage counters for the operator `/sap`
/// console. The TS bridge worker writes a tiny JSON file on every
/// RPC-bearing op (publish / attest / discovery); this route reads that
/// same file — resolved from the identical env layering the worker uses
/// (`COVENANT_SAP_STATS_PATH` > `COVENANT_HOME` > `~/.covenant`) — and
/// derives success-rate + uptime. Returns zeros when nothing has been
/// recorded yet (file absent), never an error: a missing counter file is
/// the normal pre-first-call state.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawSapStats {
    calls: u64,
    successes: u64,
    failures: u64,
    first_seen_unix: u64,
    last_call_unix: u64,
}

fn sap_stats_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("COVENANT_SAP_STATS_PATH") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return std::path::PathBuf::from(trimmed);
        }
    }
    let home = std::env::var("COVENANT_HOME")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| {
            let h = std::env::var("HOME").unwrap_or_default();
            format!("{h}/.covenant")
        });
    std::path::PathBuf::from(home).join("sap-rpc-stats.json")
}

fn read_sap_stats() -> RawSapStats {
    match std::fs::read_to_string(sap_stats_path()) {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => RawSapStats::default(),
    }
}

async fn sap_stats() -> impl IntoResponse {
    let s = read_sap_stats();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let success_rate = if s.calls > 0 {
        s.successes as f64 / s.calls as f64
    } else {
        0.0
    };
    let uptime_seconds = if s.first_seen_unix > 0 {
        now.saturating_sub(s.first_seen_unix)
    } else {
        0
    };
    Json(serde_json::json!({
        "calls": s.calls,
        "successes": s.successes,
        "failures": s.failures,
        "successRate": success_rate,
        "firstSeenUnix": s.first_seen_unix,
        "lastCallUnix": s.last_call_unix,
        "uptimeSeconds": uptime_seconds,
    }))
}

#[derive(Deserialize)]
struct SubmitIntentBody {
    text: String,
}

/// Dual-mode `POST /intent` handler per ADR 0010 slice 6.j.
///
/// Symmetric with [`memory_recent`] and [`audit_recent`]: `Accept:
/// text/event-stream` selects the SSE response path that streams the
/// intent dispatch as one `AgentResult` chunk plus a `StreamEnd`
/// summary carrying `intent_id`, `status`, and `settlement`. Any other
/// Accept (or absent Accept) keeps the v1-byte-identical buffered
/// `Json<Response>` shape.
///
/// Axum extractor ordering matters here: `HeaderMap` is a non-body
/// extractor and must come BEFORE the `Json<SubmitIntentBody>` body
/// extractor — body consumers must be last in the parameter list.
///
/// The collector's `Err(Response)` arm covers capability failures,
/// ignore-rule matches, and budget exhaustion. Per the ADR 0010
/// 'daemon decides per verb' clause, the daemon renders these as a
/// buffered JSON response regardless of the client's Accept header —
/// the buffered shape is the daemon's 'streaming refused' signal,
/// distinct from a mid-flight `stream_error` SSE frame (which is
/// reserved for streams that opened then failed).
async fn submit_intent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    headers: HeaderMap,
    Json(b): Json<SubmitIntentBody>,
) -> Result<AxumResponse, ApiError> {
    let accept = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok());
    if sse::wants_event_stream(accept) {
        let connection_id = uuid::Uuid::new_v4();
        match s
            .server
            .submit_intent_envelopes(b.text, &peer, connection_id)
            .await
        {
            Ok(envelopes) => {
                let body = sse::render_envelopes_as_sse_body(&envelopes)
                    .map_err(|e| ApiError(anyhow::anyhow!("sse encode: {e}")))?;
                let mut response = AxumResponse::new(Body::from(body));
                for (name, value) in sse::sse_response_headers() {
                    response.headers_mut().insert(name, value);
                }
                Ok(response)
            }
            Err(response) => Ok(Json(response).into_response()),
        }
    } else {
        let response = s
            .server
            .respond(
                Request::SubmitIntent {
                    text: b.text,
                    prefer_stream: None,
                },
                &peer,
            )
            .await;
        Ok(Json(response).into_response())
    }
}

#[derive(Deserialize, Default)]
struct RecentParams {
    tier: Option<MemoryTier>,
    limit: Option<usize>,
}

/// Dual-mode `GET /memory` handler per ADR 0010 slice 6.h.
///
/// - `Accept: text/event-stream` (case-insensitive, q-values stripped)
///   selects the SSE response path: the daemon allocates a fresh
///   per-request connection_id, calls
///   [`crate::Server::recent_memory_envelopes`] to materialize the
///   `StreamBegin` / `StreamChunk*` / `StreamEnd` sequence, renders
///   each envelope as one SSE event block, and returns a response
///   with the pinned SSE headers (`Content-Type: text/event-stream`,
///   `Cache-Control: no-cache`, `X-Accel-Buffering: no`).
/// - Any other Accept header (or absent Accept) selects the buffered
///   JSON response path: the daemon answers via `Server::respond`
///   exactly as the v1 contract specifies — `Json<Response>` with
///   `Content-Type: application/json`. Wire-byte-identical with the
///   pre-ADR-0010 behavior.
///
/// The collector's `Err(Response)` arm is the daemon's "streaming
/// refused, render as buffered" signal: a capability failure or
/// non-`Memories` response renders as JSON regardless of the client's
/// Accept header. The client asked for SSE but the daemon decided
/// not to stream; the buffered shape is the fallback and uses the
/// same payload v1 clients already handle.
async fn memory_recent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<RecentParams>,
    headers: HeaderMap,
) -> Result<AxumResponse, ApiError> {
    let limit = q.limit.unwrap_or(10);
    let accept = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok());
    if sse::wants_event_stream(accept) {
        let connection_id = uuid::Uuid::new_v4();
        match s
            .server
            .recent_memory_envelopes(q.tier, limit, &peer, connection_id)
            .await
        {
            Ok(envelopes) => {
                let body = sse::render_envelopes_as_sse_body(&envelopes)
                    .map_err(|e| ApiError(anyhow::anyhow!("sse encode: {e}")))?;
                let mut response = AxumResponse::new(Body::from(body));
                for (name, value) in sse::sse_response_headers() {
                    response.headers_mut().insert(name, value);
                }
                Ok(response)
            }
            Err(response) => Ok(Json(response).into_response()),
        }
    } else {
        let response = s
            .server
            .respond(
                Request::RecentMemory {
                    tier: q.tier,
                    limit,
                    prefer_stream: None,
                },
                &peer,
            )
            .await;
        Ok(Json(response).into_response())
    }
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

#[derive(Deserialize)]
struct SignBody {
    message_b58: String,
    ts: u64,
}

async fn identity_sign(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<SignBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::SignAttestation {
                    message_b58: b.message_b58,
                    ts: b.ts,
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

/// Dual-mode `GET /audit/recent` handler per ADR 0010 slice 6.i.
///
/// Symmetric with [`memory_recent`]: `Accept: text/event-stream`
/// selects the SSE response path that streams the audit-events page
/// as one event per audit row; any other Accept (or absent Accept)
/// keeps the v1-byte-identical buffered `Json<Response>` shape.
///
/// Unlike memory, `recent_audit` filters by `peer.pubkey ==
/// event.issuer.pubkey` inside [`crate::Server::recent_audit`]; there
/// is no capability gate, so the `Err(Response)` arm of
/// [`crate::Server::recent_audit_envelopes`] is unreachable in
/// practice. The Result shape is preserved for symmetry, and a future
/// per-event audience gate that produces a non-`AuditEvents` Response
/// would render through the buffered-fallback path without further
/// wiring change.
async fn audit_recent(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Query(q): Query<LimitParams>,
    headers: HeaderMap,
) -> Result<AxumResponse, ApiError> {
    let limit = q.limit.unwrap_or(20);
    let since_ms = q.since_ms;
    let accept = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok());
    if sse::wants_event_stream(accept) {
        let connection_id = uuid::Uuid::new_v4();
        match s
            .server
            .recent_audit_envelopes(limit, since_ms, &peer, connection_id)
            .await
        {
            Ok(envelopes) => {
                let body = sse::render_envelopes_as_sse_body(&envelopes)
                    .map_err(|e| ApiError(anyhow::anyhow!("sse encode: {e}")))?;
                let mut response = AxumResponse::new(Body::from(body));
                for (name, value) in sse::sse_response_headers() {
                    response.headers_mut().insert(name, value);
                }
                Ok(response)
            }
            Err(response) => Ok(Json(response).into_response()),
        }
    } else {
        let response = s
            .server
            .respond(
                Request::RecentAudit {
                    limit,
                    since_ms,
                    prefer_stream: None,
                },
                &peer,
            )
            .await;
        Ok(Json(response).into_response())
    }
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

/// Maximum body the bearer-EXEMPT cross-host admission route buffers before
/// parsing. Far tighter than [`MAX_FRAME`] (8 MiB) because this is the one
/// unauthenticated surface where deserialization precedes signature
/// verification — `SignedA2ATask::open` parses the payload to learn the claimed
/// sender — so the cap must bound an attacker's pre-auth allocation. The
/// envelope's `task_json` and `signature` are `Vec<u8>` that serialize as JSON
/// integer arrays (~4-5x the raw bytes on the wire), so 64 KiB bounds a genuine
/// `A2ATask` to roughly 13 KiB of payload — comfortably above a real task plus
/// its 64-byte signature while keeping the pre-verify buffer bounded. A future
/// tighter cap must stay above that inflated wire size.
const PEER_TASK_BODY_CAP: usize = 64 * 1024;

/// `POST /a2a/peer-tasks` — admit a cross-host [`covenant_a2a::SignedA2ATask`]
/// onto the local mailbox, or refuse it (multi-host slice 4b-2b). Bearer-EXEMPT:
/// a remote daemon holds no local token and authenticates by the envelope
/// signature, so authority comes from the admission core
/// ([`Server::admit_remote_a2a_task`]), not [`require_bearer`].
///
/// The handler is a thin transport over the reviewed core: it derives no host,
/// identity, or freshness signal from the request itself. The sender host is
/// `host_component(task.sender.display)` from the *signed* payload — never the
/// transport's `Host` header, URL authority, or TLS SNI — so no case-insensitive
/// transport source can split or confuse host identity (the registry match stays
/// byte-exact, as 4b-2a documented). The receiver's real wall clock
/// ([`crate::epoch_ms`]) is supplied so the freshness window actually fires.
///
/// The response is deliberately coarse so the three `open()` variants and the
/// later gates cannot be turned into a probing oracle: every
/// [`RemoteAdmission::Rejected`] cause maps to ONE `403` body; the rejection
/// stage lives only on the operator's audit feed. `Admitted` and `Duplicate`
/// both return one `202` ack, so a replay is absorbed idempotently and the
/// caller cannot tell a fresh admission from an absorbed replay.
///
/// Rate-limiting is deferred to the ingress/reverse-proxy layer rather than an
/// in-process limiter: the daemon stays loopback-bound until a separately
/// operator-gated bind change, where the only pre-auth signal is the proxy's
/// own socket — an in-process per-source bucket would key on the proxy and be
/// ineffective. The content oracle is closed here by the coarse response; the
/// residual timing side-channel is tracked for the bind-change slice.
async fn admit_peer_task(
    State(s): State<HttpState>,
    Json(envelope): Json<covenant_a2a::SignedA2ATask>,
) -> AxumResponse {
    match s
        .server
        .admit_remote_a2a_task(envelope, crate::epoch_ms())
        .await
    {
        RemoteAdmission::Admitted { .. } | RemoteAdmission::Duplicate { .. } => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "kind": "accepted" })),
        )
            .into_response(),
        RemoteAdmission::Rejected => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "kind": "rejected" })),
        )
            .into_response(),
    }
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

async fn intent_result(
    State(s): State<HttpState>,
    Extension(_peer): Extension<AgentId>,
    Path(id): Path<uuid::Uuid>,
) -> Result<AxumResponse, ApiError> {
    match s.server.intent_outcome(&id) {
        Some(v) => Ok(Json(v).into_response()),
        None => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "kind": "error", "message": "unknown intent" })),
        )
            .into_response()),
    }
}

/// SSE keep-alive cadence. Render and Cloudflare both close idle HTTP/1.1
/// streams around the 100s mark; emitting a comment frame well under that
/// window keeps the EventSource alive between tool calls so the watch-it-
/// work page does not flip to "disconnected" during quiet stretches.
const SSE_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// SSE comment frame — lines starting with `:` are discarded silently by
/// EventSource per the spec, so the client just sees a byte flush from
/// the server. Cheaper than synthesizing a real `data:` event.
const SSE_KEEPALIVE: &[u8] = b": keepalive\n\n";

/// Live [`AgentEvent`] stream for one intent, framed as
/// `text/event-stream`.
///
/// Subscribes to the daemon's broadcast fan-out and emits one SSE frame
/// per matching trace as it lands. Each frame carries a public
/// [`AgentEvent`] JSON object (`tool_call`, `tool_result`, `file_write`,
/// or the reserved `reasoning` slot) projected from the runner-side
/// `RuntimeTrace`, so browser clients render the live trail without
/// reaching into runner-specific vocabulary. A keep-alive comment frame
/// fires every [`SSE_HEARTBEAT_INTERVAL`] of idle so platform edges
/// (Render, Cloudflare) do not silently kill the connection between
/// tool calls.
///
/// Audience model mirrors [`intent_result`]: any authenticated peer can
/// stream any intent. Tighten with an intent-owner check before exposing
/// to multi-tenant deployments.
///
/// Lagged subscribers (slow clients that fall behind by more than the
/// channel capacity) drop the missed traces and keep streaming; the live
/// view is best-effort, the audit chain is the durable record.
async fn intent_events(
    State(s): State<HttpState>,
    Extension(_peer): Extension<AgentId>,
    Path(id): Path<uuid::Uuid>,
) -> AxumResponse {
    let rx = s.live_traces_tx.subscribe();
    let stream = intent_event_stream(rx, id);
    let mut response = AxumResponse::new(Body::from_stream(stream));
    for (name, value) in sse::sse_response_headers() {
        response.headers_mut().insert(name, value);
    }
    response
}

/// Merge the live-trace projection with a heartbeat tick into one byte
/// stream of SSE frames. Factored out of [`intent_events`] so the merge
/// logic (and the heartbeat-on-idle invariant) is exercised directly in
/// unit tests without spinning up an axum router.
fn intent_event_stream(
    rx: tokio::sync::broadcast::Receiver<StreamedTrace>,
    id: uuid::Uuid,
) -> impl futures::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send + 'static {
    let events = tokio_broadcast_stream(rx).filter_map(move |trace| async move {
        let trace = trace?;
        agent_event_sse_frame(&trace, id).map(Ok::<_, std::io::Error>)
    });
    let heartbeat = futures::stream::unfold((), |()| async {
        tokio::time::sleep(SSE_HEARTBEAT_INTERVAL).await;
        Some((Ok::<_, std::io::Error>(SSE_KEEPALIVE.to_vec()), ()))
    });
    // Box-pin both halves so `futures::stream::select` (which requires
    // Unpin) can merge them; an idle stream now emits a keep-alive
    // comment instead of holding silent until the upstream proxy kills
    // the connection.
    futures::stream::select(Box::pin(events), Box::pin(heartbeat))
}

/// Build the SSE frame bytes for one trace. Returns `None` when the
/// trace belongs to a different intent (handler skips it) or the
/// projected event fails to serialize (best-effort — the audit chain
/// is the durable record). Factored out of `intent_events` so the wire
/// shape is testable without spinning up a router.
fn agent_event_sse_frame(trace: &StreamedTrace, id: uuid::Uuid) -> Option<Vec<u8>> {
    if trace.intent_id != id {
        return None;
    }
    let event = AgentEvent::from(&trace.trace);
    let payload = serde_json::to_string(&event).ok()?;
    Some(format!("data: {payload}\n\n").into_bytes())
}

/// Bridge a `tokio::sync::broadcast::Receiver<StreamedTrace>` into a
/// `Stream` of `Option<StreamedTrace>` (None on lag — handler drops it).
/// Avoids a `tokio-stream` dep by hand-rolling the loop with
/// `futures::stream::unfold`.
fn tokio_broadcast_stream(
    rx: tokio::sync::broadcast::Receiver<StreamedTrace>,
) -> impl futures::Stream<Item = Option<StreamedTrace>> + Send + 'static {
    futures::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(trace) => Some((Some(trace), rx)),
            // Slow client; skip the lagged window and keep going.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Some((None, rx)),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    })
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

#[derive(Deserialize, Default)]
struct SettlementBackfillBody {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    scope_pubkey: Option<String>,
}

async fn settlement_backfill_receipts(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<SettlementBackfillBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::BackfillSettlementReceipts {
                    dry_run: b.dry_run,
                    scope_pubkey: b.scope_pubkey,
                },
                &peer,
            )
            .await,
    ))
}

#[derive(Deserialize, Default)]
struct MemoryBackfillBody {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    scope_pubkey: Option<String>,
}

async fn memory_backfill_records(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<MemoryBackfillBody>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::BackfillMemoryRecords {
                    dry_run: b.dry_run,
                    scope_pubkey: b.scope_pubkey,
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

/// HTTP body shape for `POST /x402/pay`. Mirrors the [`Request::PayX402`]
/// fields except for the `kind` discriminator (the HTTP layer fills
/// that in). `per_call_cap` is a decimal string so atomic u128
/// amounts above JSON's 53-bit integer ceiling survive the wire.
#[derive(Deserialize)]
struct PayX402Body {
    provider: String,
    endpoint: String,
    method: String,
    #[serde(default)]
    body: Option<serde_json::Value>,
    network: String,
    asset: String,
    per_call_cap: String,
    credits: u64,
}

async fn pay_x402_route(
    State(s): State<HttpState>,
    Extension(peer): Extension<AgentId>,
    Json(b): Json<PayX402Body>,
) -> Result<Json<Response>, ApiError> {
    Ok(Json(
        s.server
            .respond(
                Request::PayX402 {
                    provider: b.provider,
                    endpoint: b.endpoint,
                    method: b.method,
                    body: b.body,
                    network: b.network,
                    asset: b.asset,
                    per_call_cap: b.per_call_cap,
                    credits: b.credits,
                },
                &peer,
            )
            .await,
    ))
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
    fn agent_event_sse_frame_pins_public_taxonomy_wire_form_and_intent_filter() {
        // `agent_event_sse_frame` is the wire-form seam for
        // `/intents/:id/events`. The composition of
        // `From<&RuntimeTrace> for AgentEvent` (pinned in covenant-runtime)
        // and the `AgentEvent` serde shape (pinned in covenant-types) is
        // not enough on its own — a refactor that "simplified" the
        // handler by reverting to `serde_json::to_string(&trace.trace)`
        // would silently re-emit the internal `RuntimeTrace` shape and
        // break every browser client destructuring on the public
        // `tool_call` / `tool_result` / `file_write` slugs. Pin the
        // exact framed bytes so that regression class fails here.
        use covenant_runtime::{RuntimeTrace, StreamedTrace};
        use covenant_types::AgentId as CovAgentId;

        let intent_id = uuid::Uuid::from_u128(0xc0_ffee);
        let other_id = uuid::Uuid::from_u128(0xdead_beef);
        let issuer = CovAgentId::new("operator@local", [7u8; 32]);

        let tool_invoked = StreamedTrace {
            intent_id,
            issuer: issuer.clone(),
            trace: RuntimeTrace::HermesToolInvoked {
                run_id: "run-1".into(),
                tool: "terminal".into(),
                preview: "ls -la".into(),
            },
        };
        let frame = agent_event_sse_frame(&tool_invoked, intent_id).expect(
            "intent-matching frame must be emitted; None here means the \
             intent filter incorrectly rejected a matching trace",
        );
        let text = String::from_utf8(frame).expect("frame must be utf8");
        assert_eq!(
            text, "data: {\"type\":\"tool_call\",\"run_id\":\"run-1\",\"tool\":\"terminal\",\"preview\":\"ls -la\"}\n\n",
            "SSE frame must carry the public AgentEvent taxonomy with \
             snake_case type discriminator and trailing double-newline \
             boundary. A refactor that reverted to RuntimeTrace JSON \
             would emit type=\"hermes_tool_invoked\" here and break the \
             browser client wire contract",
        );

        // Intent filter: a trace for a different intent must NOT emit.
        // Without this guard, every authenticated SSE subscriber would
        // see traces for every in-flight intent on the daemon — a
        // separate slice can tighten audience further but the
        // intent-id filter must remain.
        assert!(
            agent_event_sse_frame(&tool_invoked, other_id).is_none(),
            "frame must be None when the trace's intent_id does not \
             match the path id; a regression here would leak traces \
             across intent boundaries",
        );

        // file_write end-to-end — also pins that bytes serializes as a
        // JSON number, not a string, so size-based client logic does
        // not silently break.
        let file_written = StreamedTrace {
            intent_id,
            issuer: issuer.clone(),
            trace: RuntimeTrace::HermesFileWritten {
                run_id: "run-2".into(),
                path: "src/main.rs".into(),
                bytes: 4_096,
            },
        };
        let text = String::from_utf8(
            agent_event_sse_frame(&file_written, intent_id).expect("frame emitted"),
        )
        .unwrap();
        assert_eq!(
            text,
            "data: {\"type\":\"file_write\",\"run_id\":\"run-2\",\"path\":\"src/main.rs\",\"bytes\":4096}\n\n",
        );

        // Approval-as-tool: the lossy projection (choice/resolved
        // dropped, duration_ms=0, error=false) is part of the public
        // contract. Pin the wire form so a refactor that surfaced
        // choice/resolved on the wire would have to update this test.
        let responded = StreamedTrace {
            intent_id,
            issuer,
            trace: RuntimeTrace::HermesApprovalResponded {
                run_id: "run-3".into(),
                choice: "approve".into(),
                resolved: 1,
            },
        };
        let text =
            String::from_utf8(agent_event_sse_frame(&responded, intent_id).expect("frame emitted"))
                .unwrap();
        assert_eq!(
            text,
            "data: {\"type\":\"tool_result\",\"run_id\":\"run-3\",\"tool\":\"approval\",\"duration_ms\":0,\"error\":false}\n\n",
            "approval-responded must surface as a tool_result with \
             tool=\"approval\"; the audit chain keeps the durable \
             HermesApprovalResolved row with the choice and resolved \
             count, so dropping them on the wire is intentional",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn intent_event_stream_emits_keepalive_when_no_traces_arrive() {
        // The watch-it-work page shows "disconnected" whenever the
        // browser EventSource fires onerror. The most common cause on
        // managed platforms is the upstream HTTP idle timeout (Render
        // and Cloudflare both sit around 100s) closing a silent SSE
        // response. A keep-alive comment frame at SSE_HEARTBEAT_INTERVAL
        // keeps the connection live across quiet stretches between tool
        // calls. A refactor that dropped the heartbeat — or moved it
        // behind a feature flag without keeping the production default
        // on — would silently regress to the old behavior.
        use futures::StreamExt;

        let (_tx, rx) = tokio::sync::broadcast::channel::<StreamedTrace>(16);
        let mut stream = Box::pin(intent_event_stream(rx, uuid::Uuid::nil()));
        // Paused tokio time auto-advances when every task is parked on a
        // timer, so `next().await` resolves the moment the heartbeat
        // sleep's virtual deadline passes.
        let frame = stream
            .next()
            .await
            .expect("an idle SSE stream must emit a heartbeat instead of staying silent")
            .expect("the heartbeat side never yields an io::Error");
        assert_eq!(
            frame, SSE_KEEPALIVE,
            "the heartbeat frame must be the SSE comment form `: keepalive\\n\\n` — \
             EventSource silently ignores comments, so the byte flush keeps the \
             connection alive without surfacing as a synthetic AgentEvent in the UI",
        );

        let second = stream
            .next()
            .await
            .expect("the heartbeat side is infinite and must yield again")
            .expect("the heartbeat side never yields an io::Error");
        assert_eq!(
            second, SSE_KEEPALIVE,
            "the heartbeat must keep firing — a refactor that emitted exactly one \
             tick would let the connection die on the second platform-edge timeout",
        );
    }

    #[tokio::test]
    async fn intent_event_stream_forwards_a_published_trace_before_a_heartbeat() {
        // The real-traffic path: when a trace arrives before the heartbeat
        // deadline, the merged stream yields the trace frame first.
        // Without start_paused, the heartbeat's 15s sleep stays unfired
        // for the duration of this test, so whatever next() returns must
        // be the broadcast-side frame.
        use covenant_runtime::{RuntimeTrace, StreamedTrace};
        use covenant_types::AgentId;
        use futures::StreamExt;

        let id = uuid::Uuid::new_v4();
        let (tx, rx) = tokio::sync::broadcast::channel::<StreamedTrace>(16);
        let mut stream = Box::pin(intent_event_stream(rx, id));
        tx.send(StreamedTrace {
            intent_id: id,
            issuer: AgentId::new("agent@local", [0u8; 32]),
            trace: RuntimeTrace::HermesFileWritten {
                run_id: "run-merge".into(),
                path: "src/main.rs".into(),
                bytes: 1_024,
            },
        })
        .expect("send to broadcast");

        let frame = stream
            .next()
            .await
            .expect("the trace must arrive before the heartbeat")
            .expect("agent_event_sse_frame never yields an io::Error");
        let text = std::str::from_utf8(&frame).expect("SSE frame must be utf8");
        assert!(
            text.starts_with("data: "),
            "a real trace must emit a `data:` frame; got {text:?}",
        );
        assert!(
            text.contains("\"type\":\"file_write\""),
            "the trace must project through agent_event_sse_frame's AgentEvent \
             taxonomy and not be replaced by a heartbeat comment; got {text:?}",
        );
    }

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

    static SAP_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn sap_stats_path_pins_explicit_precedence_trim_and_blank_fallthrough() {
        let _guard = SAP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("COVENANT_SAP_STATS_PATH").ok();

        // An explicit override wins outright and is trimmed: the SAP worker
        // and the /sap/stats handler must resolve the identical counter file
        // or the endpoint reports a phantom zero against a live counter.
        std::env::set_var(
            "COVENANT_SAP_STATS_PATH",
            "  /var/lib/covenant/explicit-stats.json  ",
        );
        assert_eq!(
            sap_stats_path(),
            std::path::PathBuf::from("/var/lib/covenant/explicit-stats.json"),
        );

        // A blank override is treated as unset, never as the literal path,
        // and the fixed counter filename is always appended. Asserting the
        // filename rather than the home-derived prefix keeps this test
        // independent of concurrent COVENANT_HOME mutation in sibling tests.
        std::env::set_var("COVENANT_SAP_STATS_PATH", "   ");
        let resolved = sap_stats_path();
        assert_ne!(resolved, std::path::PathBuf::from("   "));
        assert_eq!(
            resolved.file_name().and_then(|s| s.to_str()),
            Some("sap-rpc-stats.json"),
        );

        match saved {
            Some(v) => std::env::set_var("COVENANT_SAP_STATS_PATH", v),
            None => std::env::remove_var("COVENANT_SAP_STATS_PATH"),
        }
    }

    #[test]
    fn read_sap_stats_defaults_on_missing_and_malformed_else_parses_camelcase() {
        use std::io::Write;

        let _guard = SAP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("COVENANT_SAP_STATS_PATH").ok();

        // Missing file is the normal pre-first-call state: zeros, not an error.
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("COVENANT_SAP_STATS_PATH", dir.path().join("absent.json"));
        let absent = read_sap_stats();
        assert_eq!(
            (
                absent.calls,
                absent.successes,
                absent.failures,
                absent.first_seen_unix,
                absent.last_call_unix,
            ),
            (0, 0, 0, 0, 0),
        );

        // Valid camelCase JSON parses field-for-field.
        let mut good = tempfile::NamedTempFile::new().expect("tempfile");
        good.write_all(
            br#"{"calls":7,"successes":5,"failures":2,"firstSeenUnix":11,"lastCallUnix":13}"#,
        )
        .expect("write good stats");
        std::env::set_var("COVENANT_SAP_STATS_PATH", good.path());
        let parsed = read_sap_stats();
        assert_eq!(
            (
                parsed.calls,
                parsed.successes,
                parsed.failures,
                parsed.first_seen_unix,
                parsed.last_call_unix,
            ),
            (7, 5, 2, 11, 13),
        );

        // A corrupt body must fall back to defaults via unwrap_or_default(),
        // never panic the /sap/stats handler with a mid-write counter file.
        let mut bad = tempfile::NamedTempFile::new().expect("tempfile");
        bad.write_all(b"this is not json").expect("write bad stats");
        std::env::set_var("COVENANT_SAP_STATS_PATH", bad.path());
        let corrupt = read_sap_stats();
        assert_eq!(
            (corrupt.calls, corrupt.successes, corrupt.failures),
            (0, 0, 0)
        );

        match saved {
            Some(v) => std::env::set_var("COVENANT_SAP_STATS_PATH", v),
            None => std::env::remove_var("COVENANT_SAP_STATS_PATH"),
        }
    }
}
