//! The control plane's HTTP surface. A public health probe and a
//! bearer-gated `/v1` API sit in front of the [`ControlPlane`]. Every
//! application error answers with the same `{ "error": { code, message } }`
//! envelope so a client keys on the code and shows the message. The
//! load-shedding guards (body-size limit, concurrency ceiling, request
//! timeout) answer with a bare `413`/`503` instead, the standard shape for
//! a request shed before it reached a handler.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::rejection::{JsonDataError, JsonRejection};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::auth::{AuthError, AuthRegistry, Principal};
use crate::plan::ResolveRejection;
use crate::service::{ControlPlane, ServiceError};
use crate::store::StoreError;
use crate::wire::{ComputeApp, ComputeJob, ComputeOffer, LaunchPlan, LaunchRequest};

/// How long one request may run before the timeout layer sheds it with a
/// 503. The engine provider derives its own per-call budget from this
/// (`engine::LAUNCH_CALL_TIMEOUT`) so a launch's coordinator calls resolve,
/// releasing their reservation on a stall, before this drops the handler.
pub(crate) const REQUEST_TIMEOUT_SECS: u64 = 30;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(REQUEST_TIMEOUT_SECS);
const MAX_CONCURRENT_REQUESTS: usize = 64;

#[derive(Clone)]
struct AppState {
    auth: Arc<AuthRegistry>,
    control: ControlPlane,
}

/// Builds the control-plane HTTP surface: a public health probe and the
/// bearer-gated `/v1` API. The body limit, concurrency ceiling and
/// request timeout bound what one process takes on before it sheds load
/// with a `503` rather than falling over.
pub fn router(auth: Arc<AuthRegistry>, control: ControlPlane) -> Router {
    let state = AppState { auth, control };
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/apps", get(apps))
        .route("/v1/offers", get(offers))
        .route("/v1/plans", post(plan))
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

/// Resolves a launch request into a concrete plan the caller can review
/// and then commit at `/v1/jobs`. No money moves here; the plan names the
/// cheapest offer that clears every floor at the price a launch will
/// escrow.
async fn plan(
    State(state): State<AppState>,
    _: Principal,
    request: Result<Json<LaunchRequest>, JsonRejection>,
) -> Result<Json<LaunchPlan>, ApiError> {
    let Json(request) = request.map_err(body_rejection)?;
    Ok(Json(state.control.plan(&request).await?))
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

async fn jobs(State(state): State<AppState>, principal: Principal) -> Json<Vec<ComputeJob>> {
    Json(state.control.jobs(&principal))
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

/// A body that is not JSON and a body that is JSON but not a launch plan
/// need different fixes, so they answer with different codes.
fn body_rejection(rejection: JsonRejection) -> ApiError {
    match rejection {
        JsonRejection::JsonSyntaxError(_) => ApiError::MalformedJson,
        JsonRejection::JsonDataError(error) => ApiError::InvalidBody(offending_field(&error)),
        JsonRejection::MissingJsonContentType(_) => ApiError::MissingJsonContentType,
        _ => ApiError::InvalidBody(Cow::Borrowed("the request body could not be read")),
    }
}

/// serde reports the path it failed on and why. Only the path is
/// forwarded, so the caller learns which field to change without the
/// response quoting the body back at it.
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
        let path = if path.is_empty() {
            field.to_owned()
        } else {
            format!("{path}.{field}")
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

impl From<AuthError> for ApiError {
    fn from(_: AuthError) -> Self {
        Self::Auth
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
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
            // A bad request is a 422; an empty or over-budget market is a
            // 409 — the request was well formed, the supply just could not
            // meet it right now.
            Self::Service(ServiceError::Unresolvable(rejection)) => {
                let status = match rejection {
                    ResolveRejection::NoOfferMeetsRequirements
                    | ResolveRejection::NoOfferWithinBudget { .. } => StatusCode::CONFLICT,
                    _ => StatusCode::UNPROCESSABLE_ENTITY,
                };
                (status, rejection.code(), rejection.to_string().into())
            }
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
                "this launch would put your open sessions over the beta spend cap; finish or \
                 cancel a running session, or launch a smaller one, then retry"
                    .into(),
            ),
            Self::Service(ServiceError::Store(StoreError::SpendCapBelowCommitments)) => (
                StatusCode::CONFLICT,
                "spend_cap_below_commitments",
                "the beta spend cap is below this owner's active reservations".into(),
            ),
            Self::Service(ServiceError::Provider(_))
            | Self::Service(ServiceError::InvalidProviderOffers) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_unavailable",
                "the compute provider is unavailable".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::BetaCredential;
    use crate::catalog::AppCatalog;
    use crate::provider::testing::MemoryProvider;
    use crate::provider::ProviderBackend;
    use crate::wire::{ComputeOffer, GpuSpec, LaunchRequest, TrustClass};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const TOKEN: &str = "a-secret-token-for-tests";
    const TOKEN_B: &str = "b-secret-token-for-tests";

    fn auth() -> AuthRegistry {
        AuthRegistry::new(vec![
            BetaCredential {
                owner: "beta-a".into(),
                token: TOKEN.into(),
                spend_cap_usdc_micros: 1_000_000,
            },
            BetaCredential {
                owner: "beta-b".into(),
                token: TOKEN_B.into(),
                spend_cap_usdc_micros: 1_000_000,
            },
        ])
        .unwrap()
    }

    fn sample_offers() -> Vec<ComputeOffer> {
        vec![
            ComputeOffer {
                id: "rtx-4090".into(),
                gpu: GpuSpec {
                    model: "rtx-4090".into(),
                    vram_mib: 24_576,
                    cuda_major: 12,
                },
                rate_usdc_micros_per_hour: 1_000_000,
                trust_class: TrustClass::Open,
                online: true,
            },
            ComputeOffer {
                id: "a100-80g".into(),
                gpu: GpuSpec {
                    model: "a100-80g".into(),
                    vram_mib: 81_920,
                    cuda_major: 12,
                },
                rate_usdc_micros_per_hour: 3_000_000,
                trust_class: TrustClass::Isolated,
                online: true,
            },
        ]
    }

    fn app_with(provider: impl ProviderBackend) -> Router {
        router(
            Arc::new(auth()),
            ControlPlane::new(AppCatalog::builtin(), Arc::new(provider)),
        )
    }

    fn app() -> Router {
        app_with(MemoryProvider::new(sample_offers()))
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn authed(uri: &str) -> Request<Body> {
        signed("GET", uri, TOKEN)
    }

    fn signed(method: &str, uri: &str, token: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await,
            serde_json::json!({ "status": "ok" })
        );
    }

    #[tokio::test]
    async fn the_api_refuses_a_missing_token_and_names_the_scheme() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1/apps")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer realm=\"covenant-compute\"")
        );
        assert_eq!(body_json(response).await["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn a_valid_token_reads_the_catalog() {
        let response = app().oneshot(authed("/v1/apps")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let apps = body_json(response).await;
        assert_eq!(
            apps.as_array().unwrap().len(),
            AppCatalog::builtin().apps().len()
        );
        assert!(apps
            .as_array()
            .unwrap()
            .iter()
            .any(|app| app["id"] == "gpu-workspace"));
    }

    #[tokio::test]
    async fn an_unknown_path_is_a_coded_404() {
        let response = app().oneshot(authed("/v1/nope")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"]["code"], "unknown_route");
    }

    #[tokio::test]
    async fn a_wrong_method_is_a_coded_405() {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/apps")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "method_not_allowed"
        );
    }

    #[tokio::test]
    async fn offers_lists_the_conforming_supply() {
        let response = app().oneshot(authed("/v1/offers")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ids: Vec<String> = body_json(response)
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|offer| offer["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, ["rtx-4090", "a100-80g"]);
    }

    #[tokio::test]
    async fn offers_needs_a_token() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1/offers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_unavailable_provider_is_a_503() {
        let response = app_with(MemoryProvider::unavailable())
            .oneshot(authed("/v1/offers"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "provider_unavailable"
        );
    }

    #[tokio::test]
    async fn a_market_of_only_malformed_offers_is_a_503() {
        let malformed = vec![ComputeOffer {
            id: String::new(),
            gpu: GpuSpec {
                model: "rtx-4090".into(),
                vram_mib: 24_576,
                cuda_major: 12,
            },
            rate_usdc_micros_per_hour: 1_000_000,
            trust_class: TrustClass::Open,
            online: true,
        }];
        let response = app_with(MemoryProvider::new(malformed))
            .oneshot(authed("/v1/offers"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "provider_unavailable"
        );
    }

    fn post_plan(request: &LaunchRequest, token: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/plans")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(request).unwrap()))
            .unwrap()
    }

    fn launch_request(max_usdc_micros: u64, min_trust: Option<TrustClass>) -> LaunchRequest {
        LaunchRequest {
            app_id: "gpu-workspace".into(),
            duration_secs: 1_800,
            max_usdc_micros,
            min_trust,
        }
    }

    #[tokio::test]
    async fn a_request_resolves_to_the_cheapest_conforming_plan() {
        let response = app()
            .oneshot(post_plan(&launch_request(500_000, None), TOKEN))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let plan = body_json(response).await;
        // The 1M/hr rtx-4090 undercuts the 3M/hr a100 for the window.
        assert_eq!(plan["offer"]["id"], "rtx-4090");
        assert_eq!(plan["app"]["id"], "gpu-workspace");
        assert_eq!(plan["duration_secs"], 1_800);
        assert_eq!(plan["maximum_usdc_micros"], 500_000);
    }

    #[tokio::test]
    async fn resolving_needs_a_token() {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/plans")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&launch_request(500_000, None)).unwrap(),
            ))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_over_budget_request_is_a_409_naming_the_price() {
        let response = app()
            .oneshot(post_plan(&launch_request(400_000, None), TOKEN))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let error = body_json(response).await;
        assert_eq!(error["error"]["code"], "over_budget");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("500000"),
            "the refusal names the price the cheapest match needs"
        );
    }

    #[tokio::test]
    async fn a_trust_floor_no_offer_meets_is_a_409() {
        // The market tops out at Isolated; a request for Attested finds
        // nothing.
        let response = app()
            .oneshot(post_plan(
                &launch_request(5_000_000, Some(TrustClass::Attested)),
                TOKEN,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "no_matching_offer"
        );
    }

    #[tokio::test]
    async fn a_misspelled_trust_floor_is_refused_not_silently_downgraded() {
        // Spelled correctly, a stronger trust floor no offer meets is a 409
        // (above). A typo must not fall through to the app's own floor and
        // resolve a weaker-isolation offer as if the caller had asked for
        // nothing: the unknown field fails the request loudly instead.
        let body = serde_json::json!({
            "app_id": "gpu-workspace",
            "duration_secs": 1_800,
            "max_usdc_micros": 5_000_000,
            "min_trus": "attested",
        });
        let request = Request::builder()
            .method("POST")
            .uri("/v1/plans")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "invalid_request_body"
        );
    }

    #[tokio::test]
    async fn a_resolved_plan_launches_when_committed() {
        let router = app();
        let resolved = router
            .clone()
            .oneshot(post_plan(&launch_request(500_000, None), TOKEN))
            .await
            .unwrap();
        assert_eq!(resolved.status(), StatusCode::OK);
        let plan: LaunchPlan = serde_json::from_value(body_json(resolved).await).unwrap();

        let launched = router
            .clone()
            .oneshot(post_job(&plan, Some("from-plan")))
            .await
            .unwrap();
        assert_eq!(launched.status(), StatusCode::OK);
        let job = body_json(launched).await;
        assert_eq!(job["status"], "running");
        assert_eq!(job["offer_id"], "rtx-4090");
        assert_eq!(job["maximum_usdc_micros"], 500_000);
    }

    #[tokio::test]
    async fn a_request_that_is_not_json_is_refused() {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/plans")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from("{not json"))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "malformed_json");
    }

    fn valid_plan() -> LaunchPlan {
        let app = AppCatalog::builtin().app("gpu-workspace").unwrap().clone();
        let offer = sample_offers()[0].clone();
        let duration_secs = 1_800;
        let maximum_usdc_micros =
            crate::wire::quote_maximum(offer.rate_usdc_micros_per_hour, duration_secs).unwrap();
        LaunchPlan {
            app,
            offer,
            duration_secs,
            maximum_usdc_micros,
        }
    }

    fn post_job(plan: &LaunchPlan, key: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/jobs")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json");
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
        }
        builder
            .body(Body::from(serde_json::to_vec(plan).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn a_valid_plan_launches_a_running_job() {
        let response = app()
            .oneshot(post_job(&valid_plan(), Some("launch-1")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let job = body_json(response).await;
        assert_eq!(job["status"], "running");
        assert_eq!(job["app_id"], "gpu-workspace");
        assert_eq!(job["offer_id"], "rtx-4090");
        assert_eq!(job["maximum_usdc_micros"], 500_000);
        assert_eq!(job["access_url"], "ssh renter@stub.test -p 2222");
    }

    #[tokio::test]
    async fn a_launch_without_an_idempotency_key_is_refused() {
        let response = app().oneshot(post_job(&valid_plan(), None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "missing_idempotency_key"
        );
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_refused() {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/jobs")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .header("idempotency-key", "k")
            .body(Body::from("{not json"))
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "malformed_json");
    }

    #[tokio::test]
    async fn an_invalid_body_names_its_field_without_quoting_the_value_back() {
        // A well-formed JSON body that will not deserialize is a coded 400
        // that names the field to change — but the error carries only the
        // field path, never the value it rejected. A caller cannot use a
        // crafted body to bounce arbitrary content back out of the API.

        // A missing required field is named so the caller knows what to add.
        let missing = Request::builder()
            .method("POST")
            .uri("/v1/plans")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "duration_secs": 1_800, "max_usdc_micros": 400_000 })
                    .to_string(),
            ))
            .unwrap();
        let response = app().oneshot(missing).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = body_json(response).await;
        assert_eq!(error["error"]["code"], "invalid_request_body");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("app_id"),
            "the refusal names the field the caller left out"
        );

        // A field of the wrong type is refused without the offending value —
        // a distinctive sentinel here — ever appearing in the response.
        let sentinel = "reflected-sentinel-4f7c9a";
        let wrong_type = Request::builder()
            .method("POST")
            .uri("/v1/plans")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "app_id": "gpu-workspace",
                    "duration_secs": sentinel,
                    "max_usdc_micros": 400_000,
                })
                .to_string(),
            ))
            .unwrap();
        let response = app().oneshot(wrong_type).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = body_json(response).await;
        assert_eq!(error["error"]["code"], "invalid_request_body");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(
            !message.contains(sentinel),
            "the rejected value must never be echoed back: {message}"
        );
    }

    #[tokio::test]
    async fn a_plan_naming_an_offer_off_the_market_is_stale() {
        let mut plan = valid_plan();
        plan.offer.id = "ghost-offer".into();
        let response = app().oneshot(post_job(&plan, Some("k"))).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(response).await["error"]["code"], "stale_offer");
    }

    #[tokio::test]
    async fn the_spend_cap_holds_across_launches() {
        let app = app();
        for key in ["a", "b"] {
            let response = app
                .clone()
                .oneshot(post_job(&valid_plan(), Some(key)))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{key} should fit the cap"
            );
        }
        let response = app
            .clone()
            .oneshot(post_job(&valid_plan(), Some("c")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "spend_cap_exceeded"
        );
    }

    #[tokio::test]
    async fn a_failed_launch_releases_the_reservation() {
        use crate::provider::{ProviderError, ProviderJob};
        use uuid::Uuid;

        // A launch that fails at the provider must free the cap it reserved,
        // or a coordinator having a bad minute would ratchet an owner's spend
        // cap to zero one stranded reservation at a time. This provider clears
        // the market check with real offers, then fails every launch, so each
        // attempt reserves and must release; under the 1M cap with 500k plans a
        // leaked reservation would wedge the third attempt on spend_cap_exceeded.
        struct LaunchFails {
            offers: Vec<ComputeOffer>,
        }
        #[async_trait]
        impl ProviderBackend for LaunchFails {
            async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError> {
                Ok(self.offers.clone())
            }
            async fn launch(
                &self,
                _job_id: Uuid,
                _plan: &LaunchPlan,
            ) -> Result<ProviderJob, ProviderError> {
                Err(ProviderError::Unavailable)
            }
            async fn poll(
                &self,
                _job_id: Uuid,
                _plan: &LaunchPlan,
            ) -> Result<ProviderJob, ProviderError> {
                Err(ProviderError::Unavailable)
            }
            async fn cancel(
                &self,
                _job_id: Uuid,
                _plan: &LaunchPlan,
            ) -> Result<ProviderJob, ProviderError> {
                Err(ProviderError::Unavailable)
            }
        }

        let app = app_with(LaunchFails {
            offers: sample_offers(),
        });
        for key in ["a", "b", "c", "d", "e"] {
            let response = app
                .clone()
                .oneshot(post_job(&valid_plan(), Some(key)))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{key}: a failed launch should surface 503, never wedge the cap",
            );
        }
    }

    #[tokio::test]
    async fn a_repeated_key_returns_the_same_job_without_reserving_twice() {
        let app = app();
        let first = app
            .clone()
            .oneshot(post_job(&valid_plan(), Some("dup")))
            .await
            .unwrap();
        let id = body_json(first).await["id"].as_str().unwrap().to_owned();
        let replay = app
            .clone()
            .oneshot(post_job(&valid_plan(), Some("dup")))
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(body_json(replay).await["id"], id);
        // A single reservation of 500k leaves room for one more under the 1M cap.
        let other = app
            .clone()
            .oneshot(post_job(&valid_plan(), Some("other")))
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_launches_racing_one_key_open_a_single_lease() {
        use crate::provider::{ProviderError, ProviderJob};
        use crate::wire::JobStatus;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Barrier;
        use uuid::Uuid;

        // A provider that rendezvouses both requests inside the market
        // check — after each has read the key as unknown, before either
        // reserves — so the test drives the exact interleaving the
        // replay-then-reserve gap opens, then counts the leases opened.
        struct RaceProvider {
            offers: Vec<ComputeOffer>,
            gate: Barrier,
            launches: AtomicUsize,
        }

        #[async_trait]
        impl ProviderBackend for RaceProvider {
            async fn offers(&self) -> Result<Vec<ComputeOffer>, ProviderError> {
                self.gate.wait().await;
                Ok(self.offers.clone())
            }
            async fn launch(
                &self,
                _job_id: Uuid,
                _plan: &LaunchPlan,
            ) -> Result<ProviderJob, ProviderError> {
                self.launches.fetch_add(1, Ordering::SeqCst);
                Ok(ProviderJob {
                    status: JobStatus::Running,
                    access_url: Some("ssh renter@race.test -p 2222".into()),
                    error: None,
                    receipt: None,
                })
            }
            async fn poll(
                &self,
                _job_id: Uuid,
                _plan: &LaunchPlan,
            ) -> Result<ProviderJob, ProviderError> {
                Err(ProviderError::Unavailable)
            }
            async fn cancel(
                &self,
                _job_id: Uuid,
                _plan: &LaunchPlan,
            ) -> Result<ProviderJob, ProviderError> {
                Err(ProviderError::Unavailable)
            }
        }

        let provider = Arc::new(RaceProvider {
            offers: sample_offers(),
            gate: Barrier::new(2),
            launches: AtomicUsize::new(0),
        });
        let app = router(
            Arc::new(auth()),
            ControlPlane::new(AppCatalog::builtin(), provider.clone()),
        );

        let (first, second) = tokio::join!(
            app.clone().oneshot(post_job(&valid_plan(), Some("dup"))),
            app.clone().oneshot(post_job(&valid_plan(), Some("dup"))),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(
            body_json(first).await["id"],
            body_json(second).await["id"],
            "both requests must resolve to the one job the key names"
        );
        assert_eq!(
            provider.launches.load(Ordering::SeqCst),
            1,
            "a duplicate key must open exactly one lease however the requests interleave"
        );
    }

    #[tokio::test]
    async fn a_reused_key_for_a_different_plan_conflicts() {
        let app = app();
        app.clone()
            .oneshot(post_job(&valid_plan(), Some("k")))
            .await
            .unwrap();
        let mut other = valid_plan();
        other.duration_secs = 3_600;
        other.maximum_usdc_micros =
            crate::wire::quote_maximum(other.offer.rate_usdc_micros_per_hour, 3_600).unwrap();
        let response = app
            .clone()
            .oneshot(post_job(&other, Some("k")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "idempotency_conflict"
        );
    }

    #[tokio::test]
    async fn a_provider_refusal_becomes_a_failed_job() {
        let response = app_with(MemoryProvider::rejecting(sample_offers()))
            .oneshot(post_job(&valid_plan(), Some("k")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let job = body_json(response).await;
        assert_eq!(job["status"], "failed");
        assert_eq!(job["error"], "provider_rejected");
    }

    async fn launch_job(router: &Router, key: &str) -> String {
        let response = router
            .clone()
            .oneshot(post_job(&valid_plan(), Some(key)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await["id"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn a_listing_holds_the_owners_jobs_without_credentials() {
        let router = app();
        let id = launch_job(&router, "j1").await;
        let response = router.clone().oneshot(authed("/v1/jobs")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let jobs = body_json(response).await;
        let jobs = jobs.as_array().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["id"], id);
        assert!(
            jobs[0]["access_url"].is_null(),
            "a listing never carries an access credential"
        );
    }

    #[tokio::test]
    async fn a_listing_is_scoped_to_its_owner() {
        let router = app();
        launch_job(&router, "j1").await;
        let response = router
            .clone()
            .oneshot(signed("GET", "/v1/jobs", TOKEN_B))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_json(response).await.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reading_one_job_refreshes_it_and_carries_the_access() {
        let router = app();
        let id = launch_job(&router, "j1").await;
        let response = router
            .clone()
            .oneshot(authed(&format!("/v1/jobs/{id}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let job = body_json(response).await;
        assert_eq!(job["status"], "running");
        assert_eq!(job["access_url"], "ssh renter@stub.test -p 2222");
    }

    #[tokio::test]
    async fn another_owner_cannot_read_the_job() {
        let router = app();
        let id = launch_job(&router, "j1").await;
        let response = router
            .clone()
            .oneshot(signed("GET", &format!("/v1/jobs/{id}"), TOKEN_B))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"]["code"], "job_not_found");
    }

    #[tokio::test]
    async fn another_owner_cannot_cancel_the_job() {
        // The one surface that ends a session must be owner-scoped too: a
        // foreign owner's cancel reads as absent, exactly as a foreign read
        // does, and never reaches another tenant's lease. The owner's
        // session is left running with the credential the cancel would have
        // revoked, so one beta tenant cannot tear down another's machine.
        let router = app();
        let id = launch_job(&router, "j1").await;

        let response = router
            .clone()
            .oneshot(signed("DELETE", &format!("/v1/jobs/{id}"), TOKEN_B))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"]["code"], "job_not_found");

        let owned = router
            .clone()
            .oneshot(authed(&format!("/v1/jobs/{id}")))
            .await
            .unwrap();
        assert_eq!(owned.status(), StatusCode::OK);
        let job = body_json(owned).await;
        assert_eq!(job["status"], "running");
        assert_eq!(job["access_url"], "ssh renter@stub.test -p 2222");
    }

    #[tokio::test]
    async fn an_unknown_job_is_a_404() {
        let response = app()
            .oneshot(authed("/v1/jobs/11111111-1111-4111-8111-111111111111"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"]["code"], "job_not_found");
    }

    #[tokio::test]
    async fn a_non_uuid_job_id_is_a_400() {
        let response = app().oneshot(authed("/v1/jobs/not-a-uuid")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "invalid_job_id");
    }

    #[tokio::test]
    async fn cancelling_settles_the_job_below_its_ceiling() {
        let router = app();
        let id = launch_job(&router, "j1").await;
        let response = router
            .clone()
            .oneshot(signed("DELETE", &format!("/v1/jobs/{id}"), TOKEN))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let job = body_json(response).await;
        assert_eq!(job["status"], "completed");
        assert!(job["access_url"].is_null());
        let charged = job["receipt"]["charged_usdc_micros"].as_u64().unwrap();
        let maximum = job["maximum_usdc_micros"].as_u64().unwrap();
        assert!(charged < maximum, "a closed lease bills under its ceiling");
    }

    #[tokio::test]
    async fn cancelling_is_safe_to_repeat() {
        let router = app();
        let id = launch_job(&router, "j1").await;
        router
            .clone()
            .oneshot(signed("DELETE", &format!("/v1/jobs/{id}"), TOKEN))
            .await
            .unwrap();
        let again = router
            .clone()
            .oneshot(signed("DELETE", &format!("/v1/jobs/{id}"), TOKEN))
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(body_json(again).await["status"], "completed");
    }
}
