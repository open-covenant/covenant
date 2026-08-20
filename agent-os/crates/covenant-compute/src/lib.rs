#![deny(unsafe_code)]

use std::cmp::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client as HttpClient, Method, RequestBuilder, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_RESPONSE_BYTES: u64 = 1_048_576;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct ProviderApiError {
    status: u16,
    code: &'static str,
    message: &'static str,
}

impl ProviderApiError {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }

    fn from_wire(status: u16, code: &str, message: &str) -> Option<Self> {
        let (code, message) = match (status, code, message) {
            (401, "unauthorized", "valid bearer authorization is required") => {
                ("unauthorized", "valid bearer authorization is required")
            }
            (400, "missing_idempotency_key", "Idempotency-Key is required") => {
                ("missing_idempotency_key", "Idempotency-Key is required")
            }
            (422, "invalid_launch_plan", "launch plan does not match the released catalog") => (
                "invalid_launch_plan",
                "launch plan does not match the released catalog",
            ),
            (409, "stale_offer", "the selected offer is no longer available") => {
                ("stale_offer", "the selected offer is no longer available")
            }
            (400, "invalid_idempotency_key", "Idempotency-Key is invalid") => {
                ("invalid_idempotency_key", "Idempotency-Key is invalid")
            }
            (400, "invalid_job_id", "job id is invalid") => ("invalid_job_id", "job id is invalid"),
            (404, "job_not_found", "job was not found") => ("job_not_found", "job was not found"),
            (409, "idempotency_conflict", "Idempotency-Key identifies a different launch") => (
                "idempotency_conflict",
                "Idempotency-Key identifies a different launch",
            ),
            (409, "spend_cap_exceeded", "the beta spend cap is exhausted") => {
                ("spend_cap_exceeded", "the beta spend cap is exhausted")
            }
            (503, "provider_unavailable", "the compute provider is unavailable") => (
                "provider_unavailable",
                "the compute provider is unavailable",
            ),
            (500, "internal_error", "the compute control plane could not complete the request") => {
                (
                    "internal_error",
                    "the compute control plane could not complete the request",
                )
            }
            _ => return None,
        };
        Some(Self {
            status,
            code,
            message,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderErrorEnvelope {
    error: ProviderErrorBody,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Workspace,
    Image,
    Chat,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppAvailability {
    Available,
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    Open,
    Isolated,
    Attested,
    Confidential,
}

impl TrustClass {
    fn rank(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Isolated => 1,
            Self::Attested => 2,
            Self::Confidential => 3,
        }
    }
}

impl PartialOrd for TrustClass {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TrustClass {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeApp {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub kind: AppKind,
    pub availability: AppAvailability,
    pub image: Option<String>,
    pub min_vram_mib: u64,
    pub min_trust: TrustClass,
    pub default_duration_secs: u64,
    pub max_duration_secs: u64,
    pub default_max_usdc_micros: u64,
}

impl ComputeApp {
    pub fn validate(&self) -> Result<(), ComputeError> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(ComputeError::InvalidAppId(self.id.clone()));
        }
        if self.default_duration_secs == 0 || self.default_duration_secs > self.max_duration_secs {
            return Err(ComputeError::InvalidAppDuration(self.id.clone()));
        }
        if self.default_max_usdc_micros == 0 {
            return Err(ComputeError::InvalidAppBudget(self.id.clone()));
        }
        if let Some(image) = &self.image {
            validate_digest_pinned_image(image)?;
        }
        if self.availability == AppAvailability::Available && self.image.is_none() {
            return Err(ComputeError::MissingAppImage(self.id.clone()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AppCatalog {
    apps: Arc<[ComputeApp]>,
}

impl AppCatalog {
    pub fn new(apps: Vec<ComputeApp>) -> Result<Self, ComputeError> {
        let mut seen = std::collections::HashSet::new();
        for app in &apps {
            app.validate()?;
            if !seen.insert(app.id.clone()) {
                return Err(ComputeError::DuplicateApp(app.id.clone()));
            }
        }
        Ok(Self { apps: apps.into() })
    }

    pub fn builtin() -> Self {
        Self::new(vec![
            ComputeApp {
                id: "comfyui".into(),
                name: "ComfyUI".into(),
                summary: "Create images with a visual generative workflow.".into(),
                kind: AppKind::Image,
                availability: AppAvailability::Preview,
                image: None,
                min_vram_mib: 16_384,
                min_trust: TrustClass::Open,
                default_duration_secs: 1_800,
                max_duration_secs: 14_400,
                default_max_usdc_micros: 500_000,
            },
            ComputeApp {
                id: "open-webui".into(),
                name: "Open WebUI".into(),
                summary: "Run an open-model chat session on a dedicated GPU.".into(),
                kind: AppKind::Chat,
                availability: AppAvailability::Preview,
                image: None,
                min_vram_mib: 16_384,
                min_trust: TrustClass::Open,
                default_duration_secs: 3_600,
                max_duration_secs: 21_600,
                default_max_usdc_micros: 1_000_000,
            },
            ComputeApp {
                id: "gpu-workspace".into(),
                name: "GPU Workspace".into(),
                summary: "Open a bounded CUDA and Jupyter workspace on a dedicated GPU.".into(),
                kind: AppKind::Workspace,
                availability: AppAvailability::Available,
                image: Some(
                    "docker.io/nvidia/cuda@sha256:cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10"
                        .into(),
                ),
                min_vram_mib: 16_384,
                min_trust: TrustClass::Open,
                default_duration_secs: 1_800,
                max_duration_secs: 21_600,
                default_max_usdc_micros: 500_000,
            },
        ])
        .expect("built-in compute catalog must be valid")
    }

    pub fn apps(&self) -> &[ComputeApp] {
        &self.apps
    }

    pub fn app(&self, id: &str) -> Result<&ComputeApp, ComputeError> {
        self.apps
            .iter()
            .find(|app| app.id == id)
            .ok_or_else(|| ComputeError::UnknownApp(id.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuSpec {
    pub model: String,
    pub vram_mib: u64,
    pub cuda_major: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeOffer {
    pub id: String,
    pub gpu: GpuSpec,
    pub rate_usdc_micros_per_hour: u64,
    pub trust_class: TrustClass,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub app_id: String,
    pub duration_secs: u64,
    pub max_usdc_micros: u64,
    #[serde(default)]
    pub min_trust: Option<TrustClass>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub app: ComputeApp,
    pub offer: ComputeOffer,
    pub duration_secs: u64,
    pub maximum_usdc_micros: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Funding,
    Provisioning,
    Running,
    Stopping,
    Completed,
    Cancelled,
    Failed,
}

impl JobStatus {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeReceipt {
    pub id: String,
    pub job_id: String,
    pub app_id: String,
    pub provider: String,
    pub runtime_secs: u64,
    pub charged_usdc_micros: u64,
    pub refunded_usdc_micros: u64,
    pub commitment: String,
    pub transaction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeJob {
    pub id: String,
    pub app_id: String,
    pub offer_id: String,
    pub status: JobStatus,
    pub maximum_usdc_micros: u64,
    pub access_url: Option<String>,
    pub error: Option<String>,
    pub receipt: Option<ComputeReceipt>,
}

#[async_trait]
pub trait ComputeProvider: Send + Sync {
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ComputeError>;
    async fn launch(
        &self,
        plan: &LaunchPlan,
        idempotency_key: &str,
    ) -> Result<ComputeJob, ComputeError>;
    async fn jobs(&self) -> Result<Vec<ComputeJob>, ComputeError>;
    async fn job(&self, id: &str) -> Result<ComputeJob, ComputeError>;
    async fn cancel(&self, id: &str) -> Result<ComputeJob, ComputeError>;
}

#[derive(Clone)]
pub struct ComputeClient {
    catalog: AppCatalog,
    provider: Arc<dyn ComputeProvider>,
}

impl ComputeClient {
    pub fn new(catalog: AppCatalog, provider: Arc<dyn ComputeProvider>) -> Self {
        Self { catalog, provider }
    }

    pub fn catalog(&self) -> &[ComputeApp] {
        self.catalog.apps()
    }

    pub async fn offers(&self) -> Result<Vec<ComputeOffer>, ComputeError> {
        self.provider.offers().await
    }

    pub async fn plan(&self, request: LaunchRequest) -> Result<LaunchPlan, ComputeError> {
        let app = self.catalog.app(&request.app_id)?.clone();
        if app.availability != AppAvailability::Available {
            return Err(ComputeError::AppUnavailable(app.id));
        }
        if request.duration_secs == 0 || request.duration_secs > app.max_duration_secs {
            return Err(ComputeError::DurationExceeded {
                requested: request.duration_secs,
                maximum: app.max_duration_secs,
            });
        }
        if request.max_usdc_micros == 0 {
            return Err(ComputeError::ZeroBudget);
        }

        let min_trust = request
            .min_trust
            .unwrap_or(app.min_trust)
            .max(app.min_trust);
        let mut compatible = self
            .provider
            .offers()
            .await?
            .into_iter()
            .filter(|offer| {
                offer.online
                    && offer.gpu.vram_mib >= app.min_vram_mib
                    && offer.trust_class >= min_trust
            })
            .filter_map(|offer| {
                let maximum =
                    quote_maximum(offer.rate_usdc_micros_per_hour, request.duration_secs)?;
                (maximum <= request.max_usdc_micros).then_some((offer, maximum))
            })
            .collect::<Vec<_>>();

        compatible.sort_by_key(|(offer, maximum)| (*maximum, offer.id.clone()));
        let (offer, maximum_usdc_micros) = compatible
            .into_iter()
            .next()
            .ok_or(ComputeError::NoCompatibleOffer)?;

        Ok(LaunchPlan {
            app,
            offer,
            duration_secs: request.duration_secs,
            maximum_usdc_micros,
        })
    }

    pub async fn launch(
        &self,
        request: LaunchRequest,
        idempotency_key: &str,
    ) -> Result<ComputeJob, ComputeError> {
        let plan = self.plan(request).await?;
        self.launch_plan(&plan, idempotency_key).await
    }

    pub async fn launch_plan(
        &self,
        plan: &LaunchPlan,
        idempotency_key: &str,
    ) -> Result<ComputeJob, ComputeError> {
        validate_idempotency_key(idempotency_key)?;
        let app = self.catalog.app(&plan.app.id)?;
        let expected_maximum =
            quote_maximum(plan.offer.rate_usdc_micros_per_hour, plan.duration_secs)
                .ok_or(ComputeError::InvalidLaunchPlan)?;
        if app != &plan.app
            || plan.app.availability != AppAvailability::Available
            || plan.duration_secs == 0
            || plan.duration_secs > plan.app.max_duration_secs
            || !plan.offer.online
            || plan.offer.gpu.vram_mib < plan.app.min_vram_mib
            || plan.offer.trust_class < plan.app.min_trust
            || plan.maximum_usdc_micros != expected_maximum
        {
            return Err(ComputeError::InvalidLaunchPlan);
        }
        self.provider.launch(plan, idempotency_key).await
    }

    pub async fn jobs(&self) -> Result<Vec<ComputeJob>, ComputeError> {
        self.provider.jobs().await
    }

    pub async fn job(&self, id: &str) -> Result<ComputeJob, ComputeError> {
        validate_job_id(id)?;
        self.provider.job(id).await
    }

    pub async fn cancel(&self, id: &str) -> Result<ComputeJob, ComputeError> {
        validate_job_id(id)?;
        self.provider.cancel(id).await
    }
}

#[derive(Clone)]
pub struct HttpComputeProvider {
    base_url: Url,
    client: HttpClient,
    authorization: Option<HeaderValue>,
}

impl HttpComputeProvider {
    pub fn new(base_url: &str) -> Result<Self, ComputeError> {
        let base_url = Url::parse(base_url).map_err(|_| ComputeError::InvalidProviderUrl)?;
        if !matches!(base_url.scheme(), "http" | "https")
            || !base_url.username().is_empty()
            || base_url.password().is_some()
        {
            return Err(ComputeError::InvalidProviderUrl);
        }
        if base_url.scheme() != "https" && base_url.host_str() != Some("127.0.0.1") {
            return Err(ComputeError::InsecureProviderUrl);
        }
        Ok(Self {
            base_url,
            client: HttpClient::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| ComputeError::ProviderClient)?,
            authorization: None,
        })
    }

    pub fn with_bearer_token(mut self, token: &str) -> Result<Self, ComputeError> {
        if token.is_empty() || token.len() > 4_096 || token.trim() != token {
            return Err(ComputeError::InvalidProviderToken);
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ComputeError::InvalidProviderToken)?;
        value.set_sensitive(true);
        self.authorization = Some(value);
        Ok(self)
    }

    fn endpoint(&self, path: &str) -> Result<Url, ComputeError> {
        self.base_url
            .join(path)
            .map_err(|_| ComputeError::InvalidProviderUrl)
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, ComputeError> {
        let request = self.client.request(method, self.endpoint(path)?);
        Ok(match &self.authorization {
            Some(value) => request.header(AUTHORIZATION, value),
            None => request,
        })
    }

    async fn decode<T: for<'de> Deserialize<'de>>(
        response: reqwest::Response,
    ) -> Result<T, ComputeError> {
        let status = response.status();
        let json_response = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
        let bytes = Self::read_bounded(response).await?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|_| ComputeError::ProviderResponse);
        }

        if json_response {
            if let Ok(envelope) = serde_json::from_slice::<ProviderErrorEnvelope>(&bytes) {
                if let Some(error) = ProviderApiError::from_wire(
                    status.as_u16(),
                    &envelope.error.code,
                    &envelope.error.message,
                ) {
                    return Err(ComputeError::ProviderApi(error));
                }
            }
        }
        Err(ComputeError::ProviderStatus(status.as_u16()))
    }

    async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, ComputeError> {
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES)
        {
            return Err(ComputeError::ProviderResponseTooLarge);
        }

        let capacity = response
            .content_length()
            .unwrap_or_default()
            .min(MAX_RESPONSE_BYTES) as usize;
        let mut body = Vec::with_capacity(capacity);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ComputeError::ProviderTransport)?
        {
            if chunk.len() > MAX_RESPONSE_BYTES as usize - body.len() {
                return Err(ComputeError::ProviderResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[async_trait]
impl ComputeProvider for HttpComputeProvider {
    async fn offers(&self) -> Result<Vec<ComputeOffer>, ComputeError> {
        let response = self
            .request(Method::GET, "v1/offers")?
            .send()
            .await
            .map_err(|_| ComputeError::ProviderTransport)?;
        Self::decode(response).await
    }

    async fn launch(
        &self,
        plan: &LaunchPlan,
        idempotency_key: &str,
    ) -> Result<ComputeJob, ComputeError> {
        let response = self
            .request(Method::POST, "v1/jobs")?
            .header("Idempotency-Key", idempotency_key)
            .json(plan)
            .send()
            .await
            .map_err(|_| ComputeError::ProviderTransport)?;
        Self::decode(response).await
    }

    async fn job(&self, id: &str) -> Result<ComputeJob, ComputeError> {
        let response = self
            .request(Method::GET, &format!("v1/jobs/{id}"))?
            .send()
            .await
            .map_err(|_| ComputeError::ProviderTransport)?;
        Self::decode(response).await
    }

    async fn jobs(&self) -> Result<Vec<ComputeJob>, ComputeError> {
        let response = self
            .request(Method::GET, "v1/jobs")?
            .send()
            .await
            .map_err(|_| ComputeError::ProviderTransport)?;
        Self::decode(response).await
    }

    async fn cancel(&self, id: &str) -> Result<ComputeJob, ComputeError> {
        let response = self
            .request(Method::DELETE, &format!("v1/jobs/{id}"))?
            .header("Idempotency-Key", uuid::Uuid::new_v4().to_string())
            .send()
            .await
            .map_err(|_| ComputeError::ProviderTransport)?;
        Self::decode(response).await
    }
}

fn quote_maximum(rate_per_hour: u64, duration_secs: u64) -> Option<u64> {
    rate_per_hour
        .checked_mul(duration_secs)?
        .checked_add(3_599)?
        .checked_div(3_600)
}

fn validate_digest_pinned_image(image: &str) -> Result<(), ComputeError> {
    let Some((repository, digest)) = image.rsplit_once("@sha256:") else {
        return Err(ComputeError::ImageNotPinned);
    };
    if repository.is_empty() || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ComputeError::ImageNotPinned);
    }
    Ok(())
}

fn validate_job_id(id: &str) -> Result<(), ComputeError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(ComputeError::InvalidJobId);
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<(), ComputeError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ComputeError::InvalidIdempotencyKey);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ComputeError {
    #[error("invalid compute app id: {0}")]
    InvalidAppId(String),
    #[error("compute app has an invalid duration contract: {0}")]
    InvalidAppDuration(String),
    #[error("compute app has an invalid budget contract: {0}")]
    InvalidAppBudget(String),
    #[error("compute app is missing a release image: {0}")]
    MissingAppImage(String),
    #[error("duplicate compute app: {0}")]
    DuplicateApp(String),
    #[error("unknown compute app: {0}")]
    UnknownApp(String),
    #[error("compute app is not released yet: {0}")]
    AppUnavailable(String),
    #[error("container image must be pinned to a sha256 digest")]
    ImageNotPinned,
    #[error("requested duration {requested}s exceeds the app maximum {maximum}s")]
    DurationExceeded { requested: u64, maximum: u64 },
    #[error("maximum spend must be greater than zero")]
    ZeroBudget,
    #[error("no online GPU satisfies the app policy and spend limit")]
    NoCompatibleOffer,
    #[error("invalid compute provider URL")]
    InvalidProviderUrl,
    #[error("compute provider URL must use HTTPS outside localhost")]
    InsecureProviderUrl,
    #[error("compute provider client could not be initialized")]
    ProviderClient,
    #[error("invalid compute provider session token")]
    InvalidProviderToken,
    #[error("compute provider is unreachable")]
    ProviderTransport,
    #[error(transparent)]
    ProviderApi(ProviderApiError),
    #[error("compute provider returned HTTP {0}")]
    ProviderStatus(u16),
    #[error("compute provider response is invalid")]
    ProviderResponse,
    #[error("compute provider response exceeds the size limit")]
    ProviderResponseTooLarge,
    #[error("invalid compute job id")]
    InvalidJobId,
    #[error("invalid compute launch idempotency key")]
    InvalidIdempotencyKey,
    #[error("compute launch plan does not match the released app policy")]
    InvalidLaunchPlan,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    struct FakeProvider {
        offers: Vec<ComputeOffer>,
        jobs: Mutex<HashMap<String, ComputeJob>>,
    }

    impl FakeProvider {
        fn new(offers: Vec<ComputeOffer>) -> Self {
            Self {
                offers,
                jobs: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl ComputeProvider for FakeProvider {
        async fn offers(&self) -> Result<Vec<ComputeOffer>, ComputeError> {
            Ok(self.offers.clone())
        }

        async fn launch(
            &self,
            plan: &LaunchPlan,
            _idempotency_key: &str,
        ) -> Result<ComputeJob, ComputeError> {
            let job = ComputeJob {
                id: "job-1".into(),
                app_id: plan.app.id.clone(),
                offer_id: plan.offer.id.clone(),
                status: JobStatus::Provisioning,
                maximum_usdc_micros: plan.maximum_usdc_micros,
                access_url: None,
                error: None,
                receipt: None,
            };
            self.jobs
                .lock()
                .unwrap()
                .insert(job.id.clone(), job.clone());
            Ok(job)
        }

        async fn job(&self, id: &str) -> Result<ComputeJob, ComputeError> {
            self.jobs
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or(ComputeError::ProviderStatus(404))
        }

        async fn jobs(&self) -> Result<Vec<ComputeJob>, ComputeError> {
            Ok(self.jobs.lock().unwrap().values().cloned().collect())
        }

        async fn cancel(&self, id: &str) -> Result<ComputeJob, ComputeError> {
            let mut jobs = self.jobs.lock().unwrap();
            let job = jobs.get_mut(id).ok_or(ComputeError::ProviderStatus(404))?;
            job.status = JobStatus::Cancelled;
            Ok(job.clone())
        }
    }

    fn offer(id: &str, rate: u64, vram_mib: u64, trust_class: TrustClass) -> ComputeOffer {
        ComputeOffer {
            id: id.into(),
            gpu: GpuSpec {
                model: "L40S".into(),
                vram_mib,
                cuda_major: 12,
            },
            rate_usdc_micros_per_hour: rate,
            trust_class,
            online: true,
        }
    }

    fn available_catalog() -> AppCatalog {
        AppCatalog::new(vec![ComputeApp {
            id: "app".into(),
            name: "App".into(),
            summary: "A released app".into(),
            kind: AppKind::Agent,
            availability: AppAvailability::Available,
            image: Some(format!("registry.example/app@sha256:{}", "a".repeat(64))),
            min_vram_mib: 16_384,
            min_trust: TrustClass::Open,
            default_duration_secs: 3_600,
            max_duration_secs: 7_200,
            default_max_usdc_micros: 1_000_000,
        }])
        .unwrap()
    }

    async fn spawn_chunked_response(
        status: &str,
        chunks: Vec<Vec<u8>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_owned();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).await;
            let headers = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 transfer-encoding: chunked\r\nconnection: close\r\n\r\n"
            );
            if stream.write_all(headers.as_bytes()).await.is_err() {
                return;
            }
            for chunk in chunks {
                let prefix = format!("{:x}\r\n", chunk.len());
                if stream.write_all(prefix.as_bytes()).await.is_err()
                    || stream.write_all(&chunk).await.is_err()
                    || stream.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
        });
        (format!("http://{address}"), server)
    }

    async fn spawn_declared_oversized_error() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).await;
            let headers = format!(
                "HTTP/1.1 409 Conflict\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n",
                MAX_RESPONSE_BYTES + 1
            );
            let _ = stream.write_all(headers.as_bytes()).await;
        });
        (format!("http://{address}"), server)
    }

    #[test]
    fn builtins_are_valid_and_keep_unreleased_images_in_preview() {
        let catalog = AppCatalog::builtin();
        assert_eq!(catalog.apps().len(), 3);
        assert_eq!(
            catalog.app("comfyui").unwrap().availability,
            AppAvailability::Preview
        );
        assert_eq!(
            catalog.app("gpu-workspace").unwrap().availability,
            AppAvailability::Available
        );
    }

    #[test]
    fn available_apps_require_digest_pinned_images() {
        let err = AppCatalog::new(vec![ComputeApp {
            id: "bad".into(),
            name: "Bad".into(),
            summary: "Bad".into(),
            kind: AppKind::Agent,
            availability: AppAvailability::Available,
            image: Some("registry.example/app:latest".into()),
            min_vram_mib: 1,
            min_trust: TrustClass::Open,
            default_duration_secs: 1,
            max_duration_secs: 1,
            default_max_usdc_micros: 1,
        }])
        .unwrap_err();
        assert_eq!(err, ComputeError::ImageNotPinned);
    }

    #[tokio::test]
    async fn plan_picks_the_cheapest_compatible_offer() {
        let provider = Arc::new(FakeProvider::new(vec![
            offer("expensive", 900_000, 48_000, TrustClass::Isolated),
            offer("cheap", 700_000, 48_000, TrustClass::Open),
            offer("small", 100_000, 8_000, TrustClass::Confidential),
        ]));
        let client = ComputeClient::new(available_catalog(), provider);
        let plan = client
            .plan(LaunchRequest {
                app_id: "app".into(),
                duration_secs: 1_800,
                max_usdc_micros: 500_000,
                min_trust: None,
            })
            .await
            .unwrap();
        assert_eq!(plan.offer.id, "cheap");
        assert_eq!(plan.maximum_usdc_micros, 350_000);
    }

    #[tokio::test]
    async fn plan_fails_closed_when_the_spend_limit_is_too_low() {
        let provider = Arc::new(FakeProvider::new(vec![offer(
            "gpu",
            800_000,
            48_000,
            TrustClass::Open,
        )]));
        let client = ComputeClient::new(available_catalog(), provider);
        let err = client
            .plan(LaunchRequest {
                app_id: "app".into(),
                duration_secs: 3_600,
                max_usdc_micros: 799_999,
                min_trust: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err, ComputeError::NoCompatibleOffer);
    }

    #[tokio::test]
    async fn higher_trust_requirements_do_not_silently_downgrade() {
        let provider = Arc::new(FakeProvider::new(vec![offer(
            "gpu",
            100_000,
            48_000,
            TrustClass::Open,
        )]));
        let client = ComputeClient::new(available_catalog(), provider);
        let err = client
            .plan(LaunchRequest {
                app_id: "app".into(),
                duration_secs: 3_600,
                max_usdc_micros: 500_000,
                min_trust: Some(TrustClass::Isolated),
            })
            .await
            .unwrap_err();
        assert_eq!(err, ComputeError::NoCompatibleOffer);
    }

    #[tokio::test]
    async fn launch_and_cancel_preserve_the_policy_maximum() {
        let provider = Arc::new(FakeProvider::new(vec![offer(
            "gpu",
            800_000,
            48_000,
            TrustClass::Open,
        )]));
        let client = ComputeClient::new(available_catalog(), provider);
        let job = client
            .launch(
                LaunchRequest {
                    app_id: "app".into(),
                    duration_secs: 1_800,
                    max_usdc_micros: 500_000,
                    min_trust: None,
                },
                "launch-1",
            )
            .await
            .unwrap();
        assert_eq!(job.maximum_usdc_micros, 400_000);
        let cancelled = client.cancel(&job.id).await.unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn launch_rejects_an_invalid_idempotency_key_before_provider_work() {
        let provider = Arc::new(FakeProvider::new(vec![offer(
            "gpu",
            800_000,
            48_000,
            TrustClass::Open,
        )]));
        let client = ComputeClient::new(available_catalog(), provider);
        let err = client
            .launch(
                LaunchRequest {
                    app_id: "app".into(),
                    duration_secs: 1_800,
                    max_usdc_micros: 500_000,
                    min_trust: None,
                },
                "contains whitespace",
            )
            .await
            .unwrap_err();
        assert_eq!(err, ComputeError::InvalidIdempotencyKey);
    }

    #[tokio::test]
    async fn launch_rejects_a_reviewed_plan_that_no_longer_matches_the_catalog() {
        let provider = Arc::new(FakeProvider::new(vec![offer(
            "gpu",
            800_000,
            48_000,
            TrustClass::Open,
        )]));
        let client = ComputeClient::new(available_catalog(), provider);
        let mut plan = client
            .plan(LaunchRequest {
                app_id: "app".into(),
                duration_secs: 1_800,
                max_usdc_micros: 500_000,
                min_trust: None,
            })
            .await
            .unwrap();
        plan.app.image = Some(format!(
            "registry.example/tampered@sha256:{}",
            "b".repeat(64)
        ));

        let error = client
            .launch_plan(&plan, "launch-reviewed")
            .await
            .unwrap_err();
        assert_eq!(error, ComputeError::InvalidLaunchPlan);
        assert!(client.jobs().await.unwrap().is_empty());
    }

    #[test]
    fn provider_configuration_rejects_insecure_urls_and_malformed_tokens() {
        assert!(matches!(
            HttpComputeProvider::new("http://compute.example"),
            Err(ComputeError::InsecureProviderUrl)
        ));
        let provider = HttpComputeProvider::new("https://compute.example").unwrap();
        assert!(matches!(
            provider.with_bearer_token(" token"),
            Err(ComputeError::InvalidProviderToken)
        ));
    }

    #[tokio::test]
    async fn http_launch_forwards_the_stable_key_and_sensitive_session() {
        async fn launch(headers: HeaderMap, Json(plan): Json<LaunchPlan>) -> Json<ComputeJob> {
            assert_eq!(
                headers
                    .get("idempotency-key")
                    .and_then(|value| value.to_str().ok()),
                Some("launch-stable")
            );
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer session-token")
            );
            Json(ComputeJob {
                id: "job-http".into(),
                app_id: plan.app.id,
                offer_id: plan.offer.id,
                status: JobStatus::Provisioning,
                maximum_usdc_micros: plan.maximum_usdc_micros,
                access_url: None,
                error: None,
                receipt: None,
            })
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/jobs", post(launch)))
                .await
                .unwrap();
        });
        let provider = HttpComputeProvider::new(&format!("http://{address}"))
            .unwrap()
            .with_bearer_token("session-token")
            .unwrap();
        let app = available_catalog().app("app").unwrap().clone();
        let job = provider
            .launch(
                &LaunchPlan {
                    app,
                    offer: offer("gpu", 800_000, 48_000, TrustClass::Open),
                    duration_secs: 1_800,
                    maximum_usdc_micros: 400_000,
                },
                "launch-stable",
            )
            .await
            .unwrap();
        assert_eq!(job.id, "job-http");
        server.abort();
    }

    #[tokio::test]
    async fn chunked_responses_stop_when_the_stream_crosses_one_mib() {
        let chunks = vec![vec![b'['; 600_000], vec![b']'; 600_000]];
        let (base_url, server) = spawn_chunked_response("200 OK", chunks).await;
        let provider = HttpComputeProvider::new(&base_url).unwrap();

        let error = provider.offers().await.unwrap_err();

        assert_eq!(error, ComputeError::ProviderResponseTooLarge);
        server.abort();
    }

    #[tokio::test]
    async fn oversized_structured_errors_are_rejected_before_body_buffering() {
        let (base_url, server) = spawn_declared_oversized_error().await;
        let provider = HttpComputeProvider::new(&base_url).unwrap();

        let error = provider.offers().await.unwrap_err();

        assert_eq!(error, ComputeError::ProviderResponseTooLarge);
        server.abort();
    }

    #[tokio::test]
    async fn malformed_or_untrusted_error_bodies_are_never_exposed() {
        async fn malformed() -> (StatusCode, [(&'static str, &'static str); 1], &'static str) {
            (
                StatusCode::CONFLICT,
                [("content-type", "application/json")],
                "{not-json",
            )
        }

        async fn untrusted() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": {
                        "code": "stale_offer",
                        "message": "credential=must-not-reach-the-desktop"
                    }
                })),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/offers", get(malformed))
                    .route("/v1/jobs", get(untrusted)),
            )
            .await
            .unwrap();
        });
        let provider = HttpComputeProvider::new(&format!("http://{address}")).unwrap();

        let malformed_error = provider.offers().await.unwrap_err();
        let untrusted_error = provider.jobs().await.unwrap_err();

        assert_eq!(malformed_error, ComputeError::ProviderStatus(409));
        assert_eq!(untrusted_error, ComputeError::ProviderStatus(409));
        assert!(!untrusted_error.to_string().contains("credential"));
        assert!(!untrusted_error.to_string().contains("must-not-reach"));
        server.abort();
    }

    #[tokio::test]
    async fn known_structured_api_errors_preserve_safe_codes_and_messages() {
        async fn stale_offer() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": {
                        "code": "stale_offer",
                        "message": "the selected offer is no longer available"
                    }
                })),
            )
        }

        async fn allowance_exhausted() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": {
                        "code": "spend_cap_exceeded",
                        "message": "the beta spend cap is exhausted"
                    }
                })),
            )
        }

        async fn idempotency_conflict() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": {
                        "code": "idempotency_conflict",
                        "message": "Idempotency-Key identifies a different launch"
                    }
                })),
            )
        }

        async fn unauthorized() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": {
                        "code": "unauthorized",
                        "message": "valid bearer authorization is required"
                    }
                })),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/offers", get(stale_offer))
                    .route("/v1/jobs", get(allowance_exhausted))
                    .route(
                        "/v1/jobs/:id",
                        get(idempotency_conflict).delete(unauthorized),
                    ),
            )
            .await
            .unwrap();
        });
        let provider = HttpComputeProvider::new(&format!("http://{address}")).unwrap();

        let errors = [
            provider.offers().await.unwrap_err(),
            provider.jobs().await.unwrap_err(),
            provider.job("job-1").await.unwrap_err(),
            provider.cancel("job-2").await.unwrap_err(),
        ];
        let expected = [
            (
                409,
                "stale_offer",
                "the selected offer is no longer available",
            ),
            (409, "spend_cap_exceeded", "the beta spend cap is exhausted"),
            (
                409,
                "idempotency_conflict",
                "Idempotency-Key identifies a different launch",
            ),
            (
                401,
                "unauthorized",
                "valid bearer authorization is required",
            ),
        ];

        for (error, (status, code, message)) in errors.into_iter().zip(expected) {
            let ComputeError::ProviderApi(error) = error else {
                panic!("expected a structured provider error");
            };
            assert_eq!(error.status(), status);
            assert_eq!(error.code(), code);
            assert_eq!(error.message(), message);
            assert_eq!(error.to_string(), message);
        }
        server.abort();
    }

    #[tokio::test]
    async fn provider_does_not_follow_control_plane_redirects() {
        async fn redirect() -> (StatusCode, [(&'static str, &'static str); 1]) {
            (StatusCode::TEMPORARY_REDIRECT, [("location", "/target")])
        }

        async fn target() -> Json<Vec<ComputeOffer>> {
            Json(vec![offer(
                "should-not-be-reached",
                1,
                48_000,
                TrustClass::Open,
            )])
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/offers", get(redirect))
                    .route("/target", get(target)),
            )
            .await
            .unwrap();
        });
        let provider = HttpComputeProvider::new(&format!("http://{address}")).unwrap();
        let error = provider.offers().await.unwrap_err();
        assert_eq!(
            error,
            ComputeError::ProviderStatus(StatusCode::TEMPORARY_REDIRECT.as_u16())
        );
        server.abort();
    }
}
