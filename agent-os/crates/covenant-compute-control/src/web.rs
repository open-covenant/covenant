use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use covenant_compute::{ComputeJob, ComputeOffer, LaunchPlan};
use serde::Serialize;

use crate::auth::{AuthError, AuthRegistry, Principal};
use crate::service::{ControlPlane, ServiceError};
use crate::store::StoreError;

#[derive(Clone)]
struct AppState {
    auth: Arc<AuthRegistry>,
    control: ControlPlane,
}

pub fn router(auth: Arc<AuthRegistry>, control: ControlPlane) -> Router {
    let state = AppState { auth, control };
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/offers", get(offers))
        .route("/v1/jobs", get(jobs).post(create_job))
        .route("/v1/jobs/:id", get(job).delete(cancel_job))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(state)
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

async fn offers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ComputeOffer>>, ApiError> {
    authenticate(&state, &headers)?;
    Ok(Json(state.control.offers().await?))
}

async fn jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ComputeJob>>, ApiError> {
    let principal = authenticate(&state, &headers)?;
    Ok(Json(state.control.jobs(&principal).await?))
}

async fn create_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(plan): Json<LaunchPlan>,
) -> Result<Json<ComputeJob>, ApiError> {
    let principal = authenticate(&state, &headers)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::MissingIdempotencyKey)?;
    Ok(Json(
        state
            .control
            .submit(&principal, idempotency_key, plan)
            .await?,
    ))
}

async fn job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ComputeJob>, ApiError> {
    let principal = authenticate(&state, &headers)?;
    Ok(Json(state.control.job(&principal, &id).await?))
}

async fn cancel_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ComputeJob>, ApiError> {
    let principal = authenticate(&state, &headers)?;
    Ok(Json(state.control.cancel(&principal, &id).await?))
}

fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Principal, ApiError> {
    state.auth.authenticate(headers).map_err(ApiError::from)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

#[derive(Debug)]
enum ApiError {
    Auth,
    MissingIdempotencyKey,
    Service(ServiceError),
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}

impl From<AuthError> for ApiError {
    fn from(_: AuthError) -> Self {
        Self::Auth
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Auth => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid bearer authorization is required",
            ),
            Self::MissingIdempotencyKey => (
                StatusCode::BAD_REQUEST,
                "missing_idempotency_key",
                "Idempotency-Key is required",
            ),
            Self::Service(ServiceError::InvalidPlan) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_launch_plan",
                "launch plan does not match the released catalog",
            ),
            Self::Service(ServiceError::StaleOffer) => (
                StatusCode::CONFLICT,
                "stale_offer",
                "the selected offer is no longer available",
            ),
            Self::Service(ServiceError::InvalidIdempotencyKey) => (
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                "Idempotency-Key is invalid",
            ),
            Self::Service(ServiceError::InvalidJobId) => (
                StatusCode::BAD_REQUEST,
                "invalid_job_id",
                "job id is invalid",
            ),
            Self::Service(ServiceError::Store(StoreError::NotFound)) => {
                (StatusCode::NOT_FOUND, "job_not_found", "job was not found")
            }
            Self::Service(ServiceError::Store(StoreError::IdempotencyConflict)) => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency-Key identifies a different launch",
            ),
            Self::Service(ServiceError::Store(StoreError::SpendCapExceeded)) => (
                StatusCode::CONFLICT,
                "spend_cap_exceeded",
                "the beta spend cap is exhausted",
            ),
            Self::Service(ServiceError::Provider(_)) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_unavailable",
                "the compute provider is unavailable",
            ),
            Self::Service(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "the compute control plane could not complete the request",
            ),
        };
        let mut response = (status, Json(ErrorEnvelope::new(code, message))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                "www-authenticate",
                HeaderValue::from_static("Bearer realm=\"covenant-compute\""),
            );
        }
        response
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

impl ErrorEnvelope {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            error: ErrorBody { code, message },
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}
