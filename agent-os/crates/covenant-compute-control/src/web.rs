use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::rejection::{JsonDataError, JsonRejection};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use covenant_compute::{ComputeApp, ComputeJob, ComputeOffer, LaunchPlan};
use serde::Serialize;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::auth::{AuthError, AuthRegistry, Principal};
use crate::service::{ControlPlane, ServiceError};
use crate::store::StoreError;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_REQUESTS: usize = 64;

#[derive(Clone)]
struct AppState {
    auth: Arc<AuthRegistry>,
    control: ControlPlane,
}

pub fn router(auth: Arc<AuthRegistry>, control: ControlPlane) -> Router {
    let state = AppState { auth, control };
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/apps", get(apps))
        .route("/v1/offers", get(offers))
        .route("/v1/jobs", get(jobs).post(create_job))
        .route("/v1/jobs/:id", get(job).delete(cancel_job))
        .fallback(unknown_route)
        .method_not_allowed_fallback(unsupported_method)
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(GlobalConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        .with_state(state)
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

async fn unknown_route() -> ApiError {
    ApiError::UnknownRoute
}

async fn unsupported_method() -> ApiError {
    ApiError::UnsupportedMethod
}

async fn apps(State(state): State<AppState>, _: Principal) -> Json<Vec<ComputeApp>> {
    Json(state.control.apps().to_vec())
}

async fn offers(
    State(state): State<AppState>,
    _: Principal,
) -> Result<Json<Vec<ComputeOffer>>, ApiError> {
    Ok(Json(state.control.offers().await?))
}

async fn jobs(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<ComputeJob>>, ApiError> {
    Ok(Json(state.control.jobs(&principal).await?))
}

async fn create_job(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    plan: Result<Json<LaunchPlan>, JsonRejection>,
) -> Result<Json<ComputeJob>, ApiError> {
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::MissingIdempotencyKey)?;
    let Json(plan) = plan.map_err(body_rejection)?;
    Ok(Json(
        state
            .control
            .submit(&principal, idempotency_key, plan)
            .await?,
    ))
}

async fn job(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<ComputeJob>, ApiError> {
    Ok(Json(state.control.job(&principal, &id).await?))
}

async fn cancel_job(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<ComputeJob>, ApiError> {
    Ok(Json(state.control.cancel(&principal, &id).await?))
}

#[async_trait]
impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        state
            .auth
            .authenticate(&parts.headers)
            .map_err(ApiError::from)
    }
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

/// A body that is not JSON and a body that is JSON but not a launch plan need
/// different fixes, so they answer with different codes.
fn body_rejection(rejection: JsonRejection) -> ApiError {
    match rejection {
        JsonRejection::JsonSyntaxError(_) => ApiError::MalformedJson,
        JsonRejection::JsonDataError(error) => ApiError::InvalidBody(offending_field(&error)),
        JsonRejection::MissingJsonContentType(_) => ApiError::MissingJsonContentType,
        _ => ApiError::InvalidBody(Cow::Borrowed("the request body could not be read")),
    }
}

/// serde reports the path it failed on and why. Only the path is forwarded, so
/// the caller learns which field to change without the response quoting the
/// body back at it.
fn offending_field(error: &JsonDataError) -> Cow<'static, str> {
    const GENERIC: Cow<'static, str> = Cow::Borrowed("the request body is not a valid launch plan");
    let Some(detail) = std::error::Error::source(error).map(ToString::to_string) else {
        return GENERIC;
    };
    let detail = detail.split(" at line ").next().unwrap_or_default();
    let (path, reason) = detail.split_once(": ").unwrap_or(("", detail));
    if let Some(field) = reason
        .strip_prefix("missing field `")
        .and_then(|rest| rest.strip_suffix('`'))
    {
        let path = match path.is_empty() {
            true => field.to_owned(),
            false => format!("{path}.{field}"),
        };
        if nameable(&path) {
            return Cow::Owned(format!("the request body is missing the field `{path}`"));
        }
        return GENERIC;
    }
    if nameable(path) {
        return Cow::Owned(format!(
            "the request body field `{path}` is not a valid launch plan value"
        ));
    }
    GENERIC
}

fn nameable(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 100
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'))
}

#[derive(Debug)]
pub enum ApiError {
    Auth,
    MissingIdempotencyKey,
    MalformedJson,
    MissingJsonContentType,
    InvalidBody(Cow<'static, str>),
    UnknownRoute,
    UnsupportedMethod,
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
        let (status, code, message): (_, _, Cow<'static, str>) = match self {
            Self::Auth => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid bearer authorization is required".into(),
            ),
            Self::MissingIdempotencyKey => (
                StatusCode::BAD_REQUEST,
                "missing_idempotency_key",
                "Idempotency-Key is required".into(),
            ),
            Self::MalformedJson => (
                StatusCode::BAD_REQUEST,
                "malformed_json",
                "the request body is not valid JSON".into(),
            ),
            Self::MissingJsonContentType => (
                StatusCode::BAD_REQUEST,
                "invalid_content_type",
                "the request body must be sent as application/json".into(),
            ),
            Self::InvalidBody(message) => {
                (StatusCode::BAD_REQUEST, "invalid_request_body", message)
            }
            Self::UnknownRoute => (
                StatusCode::NOT_FOUND,
                "unknown_route",
                "no such endpoint".into(),
            ),
            Self::UnsupportedMethod => (
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "this endpoint does not support that method".into(),
            ),
            Self::Service(ServiceError::InvalidPlan(rejection)) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                rejection.code(),
                rejection.to_string().into(),
            ),
            Self::Service(ServiceError::StaleOffer) => (
                StatusCode::CONFLICT,
                "stale_offer",
                "the selected offer is no longer available".into(),
            ),
            Self::Service(ServiceError::InvalidIdempotencyKey) => (
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                "Idempotency-Key is invalid".into(),
            ),
            Self::Service(ServiceError::InvalidJobId) => (
                StatusCode::BAD_REQUEST,
                "invalid_job_id",
                "job id is invalid".into(),
            ),
            Self::Service(ServiceError::Store(StoreError::NotFound)) => (
                StatusCode::NOT_FOUND,
                "job_not_found",
                "job was not found".into(),
            ),
            Self::Service(ServiceError::Store(StoreError::IdempotencyConflict)) => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency-Key identifies a different launch".into(),
            ),
            Self::Service(ServiceError::Store(StoreError::SpendCapExceeded)) => (
                StatusCode::CONFLICT,
                "spend_cap_exceeded",
                "the beta spend cap is exhausted".into(),
            ),
            Self::Service(ServiceError::Store(StoreError::SpendCapBelowCommitments)) => (
                StatusCode::CONFLICT,
                "spend_cap_below_commitments",
                "the configured spend cap is below this owner's reserved and spent amounts".into(),
            ),
            Self::Service(ServiceError::InvalidProviderOffers)
            | Self::Service(ServiceError::Provider(_)) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_unavailable",
                "the compute provider is unavailable".into(),
            ),
            Self::Service(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "the compute control plane could not complete the request".into(),
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
    fn new(code: &'static str, message: Cow<'static, str>) -> Self {
        Self {
            error: ErrorBody { code, message },
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: Cow<'static, str>,
}
