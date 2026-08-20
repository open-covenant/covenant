//! Managed Vast.ai capacity for Covenant Compute.
//!
//! [`VastClient::launch`] is the only public creation path. It selects an
//! offer below both the operator ceiling and the workload cap before creating
//! a digest-pinned instance and attaching its SSH key.
//!
//! This file modifies Apache-2.0-licensed upstream software. See `NOTICE`.

#![deny(unsafe_code)]

use std::{collections::HashSet, env, fs, net::IpAddr, path::Path, sync::Arc, time::Duration};

use futures::StreamExt;
use reqwest::{redirect::Policy as RedirectPolicy, Client, RequestBuilder, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::{Host, Url};

const DEFAULT_API_URL: &str = "https://console.vast.ai/api/v0/";
const DEFAULT_MAX_HOURLY_MICROS: u64 = 640_000;
const DEFAULT_DISK_GB: u32 = 16;
const DEFAULT_GPU_MODELS: &str = "L40S";
const DEFAULT_MIN_GPU_MEMORY_MIB: u64 = 45_000;
// Max acceptable per-GB bandwidth cost (USD micros). 0 keeps the original
// free-bandwidth-only behavior; set the env to accept cheap-bandwidth hosts.
const DEFAULT_MAX_INET_COST_MICROS: u64 = 0;
const MAX_INET_COST_MICROS: u64 = 1_000_000;
const MAX_HOURLY_MICROS: u64 = 10_000_000;
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MIN_RELIABILITY: f64 = 0.99;
const WORKSPACE_IMAGE: &str =
    "docker.io/nvidia/cuda@sha256:cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10";
const WORKSPACE_MIN_CUDA: CudaVersion = CudaVersion {
    major: 12,
    minor: 4,
};

const API_URL_ENV: &str = "COVENANT_VAST_API_URL";
const API_KEY_ENV: &str = "COVENANT_VAST_API_KEY";
const API_KEY_FILE_ENV: &str = "COVENANT_VAST_API_KEY_FILE";
const MAX_HOURLY_ENV: &str = "COVENANT_VAST_MAX_HOURLY_MICROS";
const GPU_MODELS_ENV: &str = "COVENANT_VAST_GPU_MODELS";
const MIN_GPU_MEMORY_ENV: &str = "COVENANT_VAST_MIN_GPU_MEMORY_MIB";
const DISK_GB_ENV: &str = "COVENANT_VAST_DISK_GB";
const MAX_INET_COST_ENV: &str = "COVENANT_VAST_MAX_INET_COST_MICROS";

pub type Result<T> = std::result::Result<T, VastError>;

#[derive(Debug, thiserror::Error)]
pub enum VastError {
    #[error("invalid configuration: {0}")]
    Configuration(&'static str),
    #[error("invalid request: {0}")]
    InvalidRequest(&'static str),
    #[error("Vast credentials are not configured")]
    CredentialsMissing,
    #[error("could not read the Vast credential file")]
    CredentialRead(#[source] std::io::Error),
    #[error("the Vast API credential is invalid")]
    InvalidCredential,
    #[error("could not build the Vast HTTP client")]
    ClientBuild(#[source] reqwest::Error),
    #[error("{operation} request failed")]
    Transport {
        operation: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("{operation} returned HTTP {status}")]
    UnexpectedStatus {
        operation: &'static str,
        status: StatusCode,
    },
    #[error("{operation} response exceeded {MAX_RESPONSE_BYTES} bytes")]
    ResponseTooLarge { operation: &'static str },
    #[error("{operation} returned invalid JSON")]
    Decode {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("{operation} returned invalid data: {reason}")]
    InvalidResponse {
        operation: &'static str,
        reason: &'static str,
    },
    #[error("no eligible GPU capacity is available within the workload cap")]
    NoCapacity,
    #[error("the required offer is no longer available at the quoted terms")]
    OfferChanged,
    #[error("SSH key attachment failed")]
    SshKeyAttachment(#[source] Box<VastError>),
    #[error("SSH key attachment failed and instance cleanup also failed")]
    AttachAndCleanupFailed {
        #[source]
        attachment: Box<VastError>,
        cleanup: Box<VastError>,
    },
}

#[derive(Clone)]
pub struct ApiToken(Arc<str>);

impl ApiToken {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty()
            || value.len() > 4_096
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
        {
            return Err(VastError::InvalidCredential);
        }
        Ok(Self(Arc::from(value)))
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let value = fs::read_to_string(path).map_err(VastError::CredentialRead)?;
        Self::new(value)
    }

    pub fn from_environment() -> Result<Self> {
        match env::var_os(API_KEY_FILE_ENV) {
            Some(path) => Self::from_file(Path::new(&path)),
            None => env::var(API_KEY_ENV)
                .map_err(|_| VastError::CredentialsMissing)
                .and_then(Self::new),
        }
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiToken([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VastConfig {
    pub api_url: Url,
    pub max_hourly_micros: u64,
    pub gpu_models: Vec<String>,
    pub min_gpu_memory_mib: u64,
    pub disk_gb: u32,
    pub max_inet_cost_micros: u64,
}

impl Default for VastConfig {
    fn default() -> Self {
        Self {
            api_url: Url::parse(DEFAULT_API_URL).expect("default Vast URL is valid"),
            max_hourly_micros: DEFAULT_MAX_HOURLY_MICROS,
            gpu_models: DEFAULT_GPU_MODELS.split(',').map(str::to_owned).collect(),
            min_gpu_memory_mib: DEFAULT_MIN_GPU_MEMORY_MIB,
            disk_gb: DEFAULT_DISK_GB,
            max_inet_cost_micros: DEFAULT_MAX_INET_COST_MICROS,
        }
    }
}

impl VastConfig {
    pub fn from_environment() -> Result<Self> {
        let default = Self::default();
        let api_url = match env::var(API_URL_ENV) {
            Ok(value) => Url::parse(&value)
                .map_err(|_| VastError::Configuration("COVENANT_VAST_API_URL is not a URL"))?,
            Err(_) => default.api_url,
        };
        let max_hourly_micros = env_u64(MAX_HOURLY_ENV, default.max_hourly_micros)?;
        let gpu_models = env::var(GPU_MODELS_ENV)
            .unwrap_or_else(|_| DEFAULT_GPU_MODELS.to_owned())
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned)
            .collect();
        let min_gpu_memory_mib = env_u64(MIN_GPU_MEMORY_ENV, default.min_gpu_memory_mib)?;
        let disk_gb = u32::try_from(env_u64(DISK_GB_ENV, u64::from(default.disk_gb))?)
            .map_err(|_| VastError::Configuration("COVENANT_VAST_DISK_GB is out of range"))?;
        let max_inet_cost_micros = env_u64(MAX_INET_COST_ENV, default.max_inet_cost_micros)?;
        let config = Self {
            api_url,
            max_hourly_micros,
            gpu_models,
            min_gpu_memory_mib,
            disk_gb,
            max_inet_cost_micros,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        validate_api_url(&self.api_url)?;
        if self.api_url.query().is_some() || self.api_url.fragment().is_some() {
            return Err(VastError::Configuration(
                "COVENANT_VAST_API_URL cannot contain a query or fragment",
            ));
        }
        if !self.api_url.username().is_empty() || self.api_url.password().is_some() {
            return Err(VastError::Configuration(
                "COVENANT_VAST_API_URL cannot contain credentials",
            ));
        }
        if !self.api_url.path().ends_with('/') {
            return Err(VastError::Configuration(
                "COVENANT_VAST_API_URL must end with a slash",
            ));
        }
        if !(1..=MAX_HOURLY_MICROS).contains(&self.max_hourly_micros) {
            return Err(VastError::Configuration(
                "COVENANT_VAST_MAX_HOURLY_MICROS is outside the supported range",
            ));
        }
        if self.gpu_models.is_empty()
            || self.gpu_models.len() > 32
            || self.gpu_models.iter().any(|model| !valid_text(model, 128))
        {
            return Err(VastError::Configuration(
                "COVENANT_VAST_GPU_MODELS must contain valid GPU model names",
            ));
        }
        if !(1_024..=1_048_576).contains(&self.min_gpu_memory_mib) {
            return Err(VastError::Configuration(
                "COVENANT_VAST_MIN_GPU_MEMORY_MIB is outside the supported range",
            ));
        }
        if !(16..=2_048).contains(&self.disk_gb) {
            return Err(VastError::Configuration(
                "COVENANT_VAST_DISK_GB must be between 16 and 2048",
            ));
        }
        if self.max_inet_cost_micros > MAX_INET_COST_MICROS {
            return Err(VastError::Configuration(
                "COVENANT_VAST_MAX_INET_COST_MICROS is outside the supported range",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferQuote {
    pub id: u64,
    pub machine_id: u64,
    pub gpu_model: String,
    pub gpu_memory_mib: u64,
    pub hourly_micros: u64,
}

impl OfferQuote {
    fn validate(&self) -> Result<()> {
        if self.id == 0
            || self.machine_id == 0
            || !valid_text(&self.gpu_model, 128)
            || self.gpu_memory_mib == 0
            || self.gpu_memory_mib > 1_048_576
            || self.hourly_micros == 0
        {
            return Err(VastError::InvalidRequest("required offer is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CudaVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub id: u64,
    pub machine_id: u64,
    pub gpu_model: String,
    pub gpu_memory_mib: u64,
    pub hourly_micros: u64,
    pub verification: String,
    pub reliability: f64,
    pub rentable: bool,
    pub rented: bool,
    pub direct_port_count: u32,
    pub cuda_max_good: CudaVersion,
    pub gpu_count: u16,
    pub gpu_arch: String,
    pub cpu_arch: String,
}

impl Offer {
    pub fn quote(&self) -> OfferQuote {
        OfferQuote {
            id: self.id,
            machine_id: self.machine_id,
            gpu_model: self.gpu_model.clone(),
            gpu_memory_mib: self.gpu_memory_mib,
            hourly_micros: self.hourly_micros,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub workload_id: String,
    pub image: String,
    pub max_hourly_micros: u64,
    pub ssh_public_key: String,
    pub rejected_machine_ids: Vec<u64>,
    pub required_offer: OfferQuote,
}

impl LaunchRequest {
    fn validate(&self) -> Result<()> {
        workspace_label(&self.workload_id)?;
        validate_digest_pinned_image(&self.image)?;
        validate_ssh_public_key(&self.ssh_public_key)?;
        validate_cap_and_rejections(self.max_hourly_micros, &self.rejected_machine_ids)?;
        self.required_offer.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    pub instance_id: u64,
    pub label: String,
    pub offer: Offer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshAccess {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceFacts {
    pub instance_id: u64,
    pub status: String,
    pub ready: bool,
    pub gpu_model: String,
    pub gpu_memory_mib: u64,
    pub verification: String,
    pub hourly_micros: u64,
    pub machine_id: u64,
    pub ssh: Option<SshAccess>,
    pub direct_ports_available: Option<bool>,
    pub direct_port_start: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLaunchRequest {
    pub workload_id: String,
    pub image: String,
    pub max_hourly_micros: u64,
    pub rejected_machine_ids: Vec<u64>,
    pub required_offer: OfferQuote,
}

impl WorkspaceLaunchRequest {
    fn validate(&self) -> Result<()> {
        workspace_label(&self.workload_id)?;
        validate_workspace_image(&self.image)?;
        validate_cap_and_rejections(self.max_hourly_micros, &self.rejected_machine_ids)?;
        self.required_offer.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLaunch {
    pub instance_id: u64,
    pub label: String,
    pub offer: OfferQuote,
    pub image: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceAccessUrl(Url);

impl WorkspaceAccessUrl {
    /// Explicitly reveals the credential-bearing URL for browser navigation.
    ///
    /// Callers must not log, serialize, or persist the returned value.
    pub fn expose_secret(&self) -> &Url {
        &self.0
    }
}

impl std::fmt::Debug for WorkspaceAccessUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorkspaceAccessUrl([REDACTED])")
    }
}

impl std::fmt::Display for WorkspaceAccessUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceFacts {
    pub instance_id: u64,
    pub status: String,
    pub ready: bool,
    pub gpu_model: String,
    pub gpu_memory_mib: u64,
    pub verification: String,
    pub hourly_micros: u64,
    pub machine_id: u64,
    pub image: String,
    pub runtime: String,
    pub access: Option<WorkspaceAccessUrl>,
}

#[derive(Clone)]
pub struct VastClient {
    client: Client,
    config: VastConfig,
    token: ApiToken,
}

impl std::fmt::Debug for VastClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VastClient")
            .field("api_url", &self.config.api_url)
            .field("max_hourly_micros", &self.config.max_hourly_micros)
            .field("gpu_models", &self.config.gpu_models)
            .field("min_gpu_memory_mib", &self.config.min_gpu_memory_mib)
            .field("disk_gb", &self.config.disk_gb)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl VastClient {
    pub fn new(config: VastConfig, token: ApiToken) -> Result<Self> {
        config.validate()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(RedirectPolicy::none())
            .build()
            .map_err(VastError::ClientBuild)?;
        Ok(Self {
            client,
            config,
            token,
        })
    }

    pub fn from_environment() -> Result<Option<Self>> {
        if env::var_os(API_KEY_FILE_ENV).is_none() && env::var_os(API_KEY_ENV).is_none() {
            return Ok(None);
        }
        Self::new(
            VastConfig::from_environment()?,
            ApiToken::from_environment()?,
        )
        .map(Some)
    }

    pub fn config(&self) -> &VastConfig {
        &self.config
    }

    pub fn admits(&self, gpu_model: &str, gpu_memory_mib: u64) -> bool {
        gpu_memory_mib >= self.config.min_gpu_memory_mib
            && self
                .config
                .gpu_models
                .iter()
                .any(|model| model.eq_ignore_ascii_case(gpu_model))
    }

    pub async fn offers(&self) -> Result<Vec<Offer>> {
        let request = self
            .client
            .post(self.endpoint("bundles/")?)
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({
                "gpu_name": {"in": &self.config.gpu_models},
                "num_gpus": {"eq": 1},
                "gpu_ram": {"gte": self.config.min_gpu_memory_mib},
                "reliability": {"gte": 0.99},
                "verified": {"eq": true},
                "rentable": {"eq": true},
                "rented": {"eq": false},
                "direct_port_count": {"gte": 1},
                "disk_space": {"gte": self.config.disk_gb},
                "allocated_storage": self.config.disk_gb,
                "inet_down_cost": {"lte": self.config.max_inet_cost_micros as f64 / 1_000_000.0},
                "inet_up_cost": {"lte": self.config.max_inet_cost_micros as f64 / 1_000_000.0},
                "cuda_max_good": {"gte": 12.4},
                "gpu_arch": {"eq": "nvidia"},
                "cpu_arch": {"eq": "amd64"},
                "type": "ondemand",
                "limit": 64
            }));
        let response: OfferResponse = self.request_json(request, "offer search").await?;
        let ceiling = self.config.max_inet_cost_micros;
        let offers: Vec<Offer> = response
            .offers
            .into_iter()
            .map(|raw| {
                check_inet_cost(&raw, ceiling)?;
                Offer::try_from(raw)
            })
            .collect::<Result<_>>()?;
        let mut ids = HashSet::with_capacity(offers.len());
        if offers.iter().any(|offer| !ids.insert(offer.id)) {
            return Err(invalid_response("offer search", "offer IDs are duplicated"));
        }
        Ok(offers)
    }

    pub async fn ranked_offers(
        &self,
        limit: usize,
        rejected_machine_ids: &[u64],
        workload_cap_micros: u64,
    ) -> Result<Vec<Offer>> {
        if !(1..=64).contains(&limit) {
            return Err(VastError::InvalidRequest(
                "offer limit must be between 1 and 64",
            ));
        }
        if workload_cap_micros == 0 || workload_cap_micros > MAX_HOURLY_MICROS {
            return Err(VastError::InvalidRequest(
                "workload cap is outside the supported range",
            ));
        }
        if rejected_machine_ids.len() > 1_024 || rejected_machine_ids.contains(&0) {
            return Err(VastError::InvalidRequest(
                "rejected machine IDs are invalid",
            ));
        }
        let ceiling = self.config.max_hourly_micros.min(workload_cap_micros);
        let mut offers: Vec<_> = self
            .offers()
            .await?
            .into_iter()
            .filter(|offer| {
                self.admits(&offer.gpu_model, offer.gpu_memory_mib)
                    && offer.hourly_micros <= ceiling
                    && !rejected_machine_ids.contains(&offer.machine_id)
            })
            .collect();
        offers.sort_by_key(|offer| (offer.hourly_micros, offer.id));
        offers.truncate(limit);
        Ok(offers)
    }

    pub async fn launch(&self, request: LaunchRequest) -> Result<Launch> {
        request.validate()?;
        let offer = self
            .confirm_offer(
                &request.required_offer,
                request.max_hourly_micros,
                &request.rejected_machine_ids,
            )
            .await?;
        let label = workspace_label(&request.workload_id)?;
        let instance_id = self.create(offer.id, &request.image, &label).await?;
        if let Err(attachment) = self
            .attach_ssh_key(instance_id, &request.ssh_public_key)
            .await
        {
            if let Err(cleanup) = self.destroy(instance_id).await {
                return Err(VastError::AttachAndCleanupFailed {
                    attachment: Box::new(attachment),
                    cleanup: Box::new(cleanup),
                });
            }
            return Err(VastError::SshKeyAttachment(Box::new(attachment)));
        }
        Ok(Launch {
            instance_id,
            label,
            offer,
        })
    }

    pub async fn launch_workspace(
        &self,
        request: WorkspaceLaunchRequest,
    ) -> Result<WorkspaceLaunch> {
        request.validate()?;
        let offer = self
            .confirm_offer(
                &request.required_offer,
                request.max_hourly_micros,
                &request.rejected_machine_ids,
            )
            .await?;
        let label = workspace_label(&request.workload_id)?;
        let instance_id = self
            .create_workspace(offer.id, &request.image, &label)
            .await?;
        Ok(WorkspaceLaunch {
            instance_id,
            label,
            offer: offer.quote(),
            image: request.image,
        })
    }

    pub async fn instance(&self, instance_id: u64) -> Result<InstanceFacts> {
        validate_instance_id(instance_id)?;
        let request = self
            .client
            .get(self.endpoint(&format!("instances/{instance_id}/"))?)
            .bearer_auth(self.token.expose());
        let response: InstanceResponse = self.request_json(request, "instance lookup").await?;
        response.instances.into_facts(instance_id)
    }

    pub async fn workspace(&self, launch: &WorkspaceLaunch) -> Result<WorkspaceFacts> {
        validate_instance_id(launch.instance_id)?;
        launch.offer.validate()?;
        validate_workspace_image(&launch.image)?;
        let request = self
            .client
            .get(self.endpoint(&format!("instances/{}/", launch.instance_id))?)
            .bearer_auth(self.token.expose());
        let response: InstanceResponse = self.request_json(request, "workspace lookup").await?;
        let instance = if response.instances.needs_port_mapping_fallback()? {
            self.workspace_instance_v1(launch.instance_id).await?
        } else {
            response.instances
        };
        instance.into_workspace_facts(launch)
    }

    pub async fn recover(&self, workload_id: &str) -> Result<Vec<u64>> {
        let label = workspace_label(workload_id)?;
        let mut url = self
            .config
            .api_url
            .join("../v1/instances/")
            .map_err(|_| VastError::Configuration("Vast instance-list URL is invalid"))?;
        url.query_pairs_mut()
            .append_pair(
                "select_filters",
                &serde_json::json!({"label": {"eq": &label}}).to_string(),
            )
            .append_pair("select_cols", r#"["id","label"]"#)
            .append_pair("limit", "100");
        let request = self.client.get(url).bearer_auth(self.token.expose());
        let response: InstancesResponse = self.request_json(request, "instance recovery").await?;
        if !response.success {
            return Err(invalid_response(
                "instance recovery",
                "provider reported an unsuccessful lookup",
            ));
        }
        let mut matches = Vec::new();
        for instance in response.instances {
            if instance.id == 0 {
                return Err(VastError::InvalidResponse {
                    operation: "instance recovery",
                    reason: "instance ID is zero",
                });
            }
            if instance
                .label
                .as_deref()
                .is_some_and(|value| !valid_text(value, 128))
            {
                return Err(VastError::InvalidResponse {
                    operation: "instance recovery",
                    reason: "instance label is invalid",
                });
            }
            if instance.label.as_deref() == Some(&label) {
                matches.push(instance.id);
            }
        }
        matches.sort_unstable_by(|left, right| right.cmp(left));
        matches.dedup();
        Ok(matches)
    }

    pub async fn destroy(&self, instance_id: u64) -> Result<()> {
        validate_instance_id(instance_id)?;
        let response = self
            .client
            .delete(self.endpoint(&format!("instances/{instance_id}/"))?)
            .bearer_auth(self.token.expose())
            .send()
            .await
            .map_err(|source| VastError::Transport {
                operation: "instance destruction",
                source,
            })?;
        if matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::GONE)
            || response.status().is_success()
        {
            return Ok(());
        }
        Err(VastError::UnexpectedStatus {
            operation: "instance destruction",
            status: response.status(),
        })
    }

    async fn create(&self, offer_id: u64, image: &str, label: &str) -> Result<u64> {
        if offer_id == 0 {
            return Err(VastError::InvalidRequest("offer ID is zero"));
        }
        validate_digest_pinned_image(image)?;
        let request = self
            .client
            .put(self.endpoint(&format!("asks/{offer_id}/"))?)
            .bearer_auth(self.token.expose())
            .json(&CreateRequest {
                image,
                label,
                disk: self.config.disk_gb,
                runtype: "ssh_direct",
                cancel_unavail: true,
            });
        let response: CreateResponse = self.request_json(request, "instance creation").await?;
        if response.new_contract == 0 {
            return Err(VastError::InvalidResponse {
                operation: "instance creation",
                reason: "instance ID is zero",
            });
        }
        Ok(response.new_contract)
    }

    async fn create_workspace(&self, offer_id: u64, image: &str, label: &str) -> Result<u64> {
        if offer_id == 0 {
            return Err(VastError::InvalidRequest("offer ID is zero"));
        }
        validate_workspace_image(image)?;
        let request = self
            .client
            .put(self.endpoint(&format!("asks/{offer_id}/"))?)
            .bearer_auth(self.token.expose())
            .json(&WorkspaceCreateRequest {
                image,
                label,
                disk: self.config.disk_gb,
                runtype: "jupyter_direct",
                use_jupyter_lab: true,
                jupyter_dir: "/workspace",
                cancel_unavail: true,
            });
        let response: CreateResponse = self.request_json(request, "workspace creation").await?;
        if response.new_contract == 0 {
            return Err(invalid_response(
                "workspace creation",
                "instance ID is zero",
            ));
        }
        Ok(response.new_contract)
    }

    async fn workspace_instance_v1(&self, instance_id: u64) -> Result<RawInstance> {
        let mut url = self
            .config
            .api_url
            .join("../v1/instances/")
            .map_err(|_| VastError::Configuration("Vast instance-list URL is invalid"))?;
        url.query_pairs_mut()
            .append_pair(
                "select_filters",
                &serde_json::json!({"id": {"eq": instance_id}}).to_string(),
            )
            .append_pair("select_cols", r#"["*"]"#)
            .append_pair("limit", "1");
        let request = self.client.get(url).bearer_auth(self.token.expose());
        let response: WorkspaceInstancesResponse = self
            .request_json(request, "workspace mapping lookup")
            .await?;
        if !response.success {
            return Err(invalid_response(
                "workspace mapping lookup",
                "provider reported an unsuccessful lookup",
            ));
        }
        if response.instances.len() != 1 {
            return Err(invalid_response(
                "workspace mapping lookup",
                "exact instance was not returned",
            ));
        }
        let instance = response
            .instances
            .into_iter()
            .next()
            .expect("length was checked");
        if instance.id != Some(instance_id) {
            return Err(invalid_response(
                "workspace mapping lookup",
                "instance ID does not match",
            ));
        }
        Ok(instance)
    }

    async fn confirm_offer(
        &self,
        required: &OfferQuote,
        workload_cap_micros: u64,
        rejected_machine_ids: &[u64],
    ) -> Result<Offer> {
        let offer = self
            .offers()
            .await?
            .into_iter()
            .find(|offer| offer.id == required.id)
            .filter(|offer| offer.quote() == *required)
            .ok_or(VastError::OfferChanged)?;
        let ceiling = self.config.max_hourly_micros.min(workload_cap_micros);
        if !self.admits(&offer.gpu_model, offer.gpu_memory_mib)
            || offer.hourly_micros > ceiling
            || rejected_machine_ids.contains(&offer.machine_id)
        {
            return Err(VastError::NoCapacity);
        }
        Ok(offer)
    }

    async fn attach_ssh_key(&self, instance_id: u64, ssh_key: &str) -> Result<()> {
        validate_instance_id(instance_id)?;
        validate_ssh_public_key(ssh_key)?;
        let request = self
            .client
            .post(self.endpoint(&format!("instances/{instance_id}/ssh/"))?)
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({"ssh_key": ssh_key}));
        self.request_empty(request, "SSH key attachment").await
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.config
            .api_url
            .join(path)
            .map_err(|_| VastError::Configuration("Vast API endpoint is invalid"))
    }

    async fn request_empty(&self, request: RequestBuilder, operation: &'static str) -> Result<()> {
        let response = self.send(request, operation).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(VastError::UnexpectedStatus {
                operation,
                status: response.status(),
            })
        }
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &'static str,
    ) -> Result<T> {
        let response = self.send(request, operation).await?;
        if !response.status().is_success() {
            return Err(VastError::UnexpectedStatus {
                operation,
                status: response.status(),
            });
        }
        decode_json(response, operation).await
    }

    async fn send(&self, request: RequestBuilder, operation: &'static str) -> Result<Response> {
        request
            .send()
            .await
            .map_err(|source| VastError::Transport { operation, source })
    }
}

#[derive(Deserialize)]
struct OfferResponse {
    #[serde(default)]
    offers: Vec<RawOffer>,
}

#[derive(Deserialize)]
struct RawOffer {
    id: u64,
    machine_id: u64,
    gpu_name: String,
    gpu_ram: u64,
    dph_total: f64,
    #[serde(default)]
    inet_down_cost: Option<f64>,
    #[serde(default)]
    inet_up_cost: Option<f64>,
    #[serde(default)]
    verification: Option<String>,
    #[serde(default)]
    reliability: Option<f64>,
    #[serde(default)]
    rentable: Option<bool>,
    #[serde(default)]
    rented: Option<bool>,
    #[serde(default)]
    direct_port_count: Option<u64>,
    #[serde(default)]
    cuda_max_good: Option<serde_json::Number>,
    #[serde(default)]
    num_gpus: Option<u64>,
    #[serde(default)]
    gpu_arch: Option<String>,
    #[serde(default)]
    cpu_arch: Option<String>,
}

/// Reject an offer whose per-GB bandwidth cost is missing, invalid, or above the
/// configured ceiling. `ceiling_micros` == 0 keeps the free-bandwidth-only rule.
fn check_inet_cost(raw: &RawOffer, ceiling_micros: u64) -> Result<()> {
    let cap = ceiling_micros as f64 / 1_000_000.0;
    let within =
        |cost: Option<f64>| cost.is_some_and(|c| c.is_finite() && (0.0..=cap).contains(&c));
    if within(raw.inet_down_cost) && within(raw.inet_up_cost) {
        Ok(())
    } else {
        Err(invalid_response(
            "offer search",
            "offer bandwidth cost is missing or above the configured ceiling",
        ))
    }
}

impl TryFrom<RawOffer> for Offer {
    type Error = VastError;

    fn try_from(raw: RawOffer) -> Result<Self> {
        if raw.id == 0 {
            return Err(invalid_response("offer search", "offer ID is zero"));
        }
        if raw.machine_id == 0 {
            return Err(invalid_response("offer search", "machine ID is zero"));
        }
        if !valid_text(&raw.gpu_name, 128) {
            return Err(invalid_response("offer search", "GPU model is invalid"));
        }
        if raw.gpu_ram == 0 || raw.gpu_ram > 1_048_576 {
            return Err(invalid_response("offer search", "GPU memory is invalid"));
        }
        let hourly_micros = hourly_micros(raw.dph_total)
            .map_err(|reason| invalid_response("offer search", reason))?;
        let verification = raw
            .verification
            .filter(|value| value == "verified")
            .ok_or_else(|| invalid_response("offer search", "host is not verified"))?;
        let reliability = raw
            .reliability
            .filter(|value| value.is_finite() && (MIN_RELIABILITY..=1.0).contains(value))
            .ok_or_else(|| {
                invalid_response("offer search", "host reliability is below the minimum")
            })?;
        if raw.rentable != Some(true) {
            return Err(invalid_response("offer search", "offer is not rentable"));
        }
        if raw.rented != Some(false) {
            return Err(invalid_response("offer search", "offer is already rented"));
        }
        let direct_port_count = raw
            .direct_port_count
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value != 0)
            .ok_or_else(|| invalid_response("offer search", "offer has no direct port capacity"))?;
        let cuda_max_good = raw
            .cuda_max_good
            .as_ref()
            .map(parse_cuda_version)
            .transpose()?
            .filter(|version| *version >= WORKSPACE_MIN_CUDA)
            .ok_or_else(|| {
                invalid_response(
                    "offer search",
                    "host CUDA compatibility is below the workspace requirement",
                )
            })?;
        let gpu_count = raw
            .num_gpus
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| *value == 1)
            .ok_or_else(|| invalid_response("offer search", "offer is not for exactly one GPU"))?;
        let gpu_arch = raw
            .gpu_arch
            .filter(|value| value == "nvidia")
            .ok_or_else(|| {
                invalid_response("offer search", "host GPU architecture is incompatible")
            })?;
        let cpu_arch = raw
            .cpu_arch
            .filter(|value| value == "amd64")
            .ok_or_else(|| {
                invalid_response("offer search", "host CPU architecture is incompatible")
            })?;
        Ok(Self {
            id: raw.id,
            machine_id: raw.machine_id,
            gpu_model: raw.gpu_name,
            gpu_memory_mib: raw.gpu_ram,
            hourly_micros,
            verification,
            reliability,
            rentable: true,
            rented: false,
            direct_port_count,
            cuda_max_good,
            gpu_count,
            gpu_arch,
            cpu_arch,
        })
    }
}

#[derive(Serialize)]
struct CreateRequest<'a> {
    image: &'a str,
    label: &'a str,
    disk: u32,
    runtype: &'static str,
    cancel_unavail: bool,
}

#[derive(Serialize)]
struct WorkspaceCreateRequest<'a> {
    image: &'a str,
    label: &'a str,
    disk: u32,
    runtype: &'static str,
    use_jupyter_lab: bool,
    jupyter_dir: &'static str,
    cancel_unavail: bool,
}

#[derive(Deserialize)]
struct CreateResponse {
    new_contract: u64,
}

#[derive(Deserialize)]
struct InstanceResponse {
    instances: RawInstance,
}

#[derive(Deserialize)]
struct RawInstance {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    actual_status: Option<String>,
    #[serde(default)]
    image_uuid: Option<String>,
    #[serde(default)]
    image_runtype: Option<String>,
    #[serde(default)]
    gpu_name: Option<String>,
    #[serde(default)]
    gpu_ram: Option<u64>,
    #[serde(default)]
    verification: Option<String>,
    #[serde(default)]
    dph_total: Option<f64>,
    #[serde(default)]
    ssh_host: Option<String>,
    #[serde(default)]
    ssh_port: Option<u16>,
    #[serde(default)]
    public_ipaddr: Option<String>,
    #[serde(default)]
    ports: Option<serde_json::Value>,
    #[serde(default)]
    jupyter_token: Option<String>,
    #[serde(default)]
    direct_port_start: Option<i64>,
    #[serde(default)]
    machine_id: Option<u64>,
    #[serde(default)]
    bundle_id: Option<u64>,
}

impl RawInstance {
    fn needs_port_mapping_fallback(&self) -> Result<bool> {
        match self.ports.as_ref() {
            None | Some(serde_json::Value::Null | serde_json::Value::Array(_)) => Ok(true),
            Some(serde_json::Value::Object(_)) => {
                Ok(parse_jupyter_port(self.ports.as_ref())?.is_none())
            }
            Some(_) => Err(invalid_response(
                "workspace lookup",
                "port mappings are invalid",
            )),
        }
    }

    fn into_facts(self, instance_id: u64) -> Result<InstanceFacts> {
        if self
            .actual_status
            .as_deref()
            .is_some_and(|value| !valid_text(value, 64))
        {
            return Err(invalid_response(
                "instance lookup",
                "instance status is invalid",
            ));
        }
        if self
            .gpu_name
            .as_deref()
            .is_some_and(|value| !valid_text(value, 128))
        {
            return Err(invalid_response("instance lookup", "GPU model is invalid"));
        }
        if self.gpu_ram.is_some_and(|value| value > 1_048_576) {
            return Err(invalid_response("instance lookup", "GPU memory is invalid"));
        }
        if self
            .verification
            .as_deref()
            .is_some_and(|value| !valid_text(value, 64))
        {
            return Err(invalid_response(
                "instance lookup",
                "verification state is invalid",
            ));
        }
        if self
            .ssh_host
            .as_deref()
            .is_some_and(|host| !valid_ssh_host(host))
        {
            return Err(invalid_response("instance lookup", "SSH host is invalid"));
        }
        if self.ssh_port == Some(0) {
            return Err(invalid_response("instance lookup", "SSH port is invalid"));
        }
        let hourly_micros = self
            .dph_total
            .map(hourly_micros)
            .transpose()
            .map_err(|reason| invalid_response("instance lookup", reason))?
            .unwrap_or_default();
        let (direct_ports_available, direct_port_start) =
            parse_direct_port(self.direct_port_start)?;
        let complete = self.gpu_name.is_some()
            && self.gpu_ram.is_some()
            && self.verification.is_some()
            && self.dph_total.is_some()
            && self.direct_port_start.is_some()
            && self.machine_id.is_some_and(|id| id != 0);
        let ready = self.actual_status.as_deref() == Some("running") && complete;
        let status = match self.actual_status {
            Some(status) if status == "running" && !complete => "loading".to_owned(),
            Some(status) => status,
            None => "loading".to_owned(),
        };
        let ssh = self
            .ssh_host
            .zip(self.ssh_port)
            .map(|(host, port)| SshAccess { host, port });
        Ok(InstanceFacts {
            instance_id,
            status,
            ready,
            gpu_model: self.gpu_name.unwrap_or_default(),
            gpu_memory_mib: self.gpu_ram.unwrap_or_default(),
            verification: self.verification.unwrap_or_default(),
            hourly_micros,
            machine_id: self.machine_id.unwrap_or_default(),
            ssh,
            direct_ports_available,
            direct_port_start,
        })
    }

    fn into_workspace_facts(self, launch: &WorkspaceLaunch) -> Result<WorkspaceFacts> {
        if self.id.is_some_and(|id| id != launch.instance_id) {
            return Err(invalid_response(
                "workspace lookup",
                "instance ID does not match",
            ));
        }
        validate_optional_text(
            self.actual_status.as_deref(),
            64,
            "workspace lookup",
            "instance status is invalid",
        )?;
        validate_optional_text(
            self.gpu_name.as_deref(),
            128,
            "workspace lookup",
            "GPU model is invalid",
        )?;
        validate_optional_text(
            self.verification.as_deref(),
            64,
            "workspace lookup",
            "verification state is invalid",
        )?;
        validate_optional_text(
            self.image_uuid.as_deref(),
            512,
            "workspace lookup",
            "image reference is invalid",
        )?;
        validate_optional_text(
            self.image_runtype.as_deref(),
            64,
            "workspace lookup",
            "runtime is invalid",
        )?;
        validate_optional_text(
            self.label.as_deref(),
            128,
            "workspace lookup",
            "instance label is invalid",
        )?;
        if self.gpu_ram.is_some_and(|value| value > 1_048_576) {
            return Err(invalid_response(
                "workspace lookup",
                "GPU memory is invalid",
            ));
        }

        reject_mismatch(
            self.image_uuid.as_deref(),
            &launch.image,
            "workspace image does not match",
        )?;
        reject_mismatch(
            self.label.as_deref(),
            &launch.label,
            "workspace label does not match",
        )?;
        reject_mismatch(
            self.image_runtype.as_deref(),
            "jupyter_direct",
            "workspace runtime does not match",
        )?;
        reject_mismatch(
            self.gpu_name.as_deref(),
            &launch.offer.gpu_model,
            "workspace GPU model does not match",
        )?;
        if self
            .gpu_ram
            .is_some_and(|value| value != launch.offer.gpu_memory_mib)
        {
            return Err(invalid_response(
                "workspace lookup",
                "workspace GPU memory does not match",
            ));
        }
        if self
            .machine_id
            .is_some_and(|value| value != launch.offer.machine_id)
        {
            return Err(invalid_response(
                "workspace lookup",
                "workspace machine does not match",
            ));
        }
        if self.bundle_id.is_some_and(|value| value != launch.offer.id) {
            return Err(invalid_response(
                "workspace lookup",
                "workspace offer does not match",
            ));
        }
        if self
            .verification
            .as_deref()
            .is_some_and(|value| value != "verified")
        {
            return Err(invalid_response(
                "workspace lookup",
                "workspace verification is not verified",
            ));
        }

        let hourly_micros = self
            .dph_total
            .map(hourly_micros)
            .transpose()
            .map_err(|reason| invalid_response("workspace lookup", reason))?;
        if hourly_micros.is_some_and(|value| value != launch.offer.hourly_micros) {
            return Err(invalid_response(
                "workspace lookup",
                "workspace price does not match",
            ));
        }

        let ip = parse_public_ip(self.public_ipaddr.as_deref())?;
        let port = parse_jupyter_port(self.ports.as_ref())?;
        let token = parse_jupyter_token(self.jupyter_token.as_deref())?;
        let complete = self.id == Some(launch.instance_id)
            && self.label.as_deref() == Some(&launch.label)
            && self.bundle_id == Some(launch.offer.id)
            && self.image_uuid.is_some()
            && self.image_runtype.is_some()
            && self.gpu_name.is_some()
            && self.gpu_ram.is_some()
            && self.verification.is_some()
            && hourly_micros.is_some()
            && self.machine_id.is_some_and(|id| id != 0)
            && ip.is_some()
            && port.is_some()
            && token.is_some();
        let ready = self.actual_status.as_deref() == Some("running") && complete;
        let status = match self.actual_status {
            Some(status) if status == "running" && !complete => "loading".to_owned(),
            Some(status) => status,
            None => "loading".to_owned(),
        };
        let access = if ready {
            Some(workspace_access_url(
                ip.expect("complete workspace has public IP"),
                port.expect("complete workspace has Jupyter port"),
                token.expect("complete workspace has Jupyter token"),
            )?)
        } else {
            None
        };

        Ok(WorkspaceFacts {
            instance_id: launch.instance_id,
            status,
            ready,
            gpu_model: self.gpu_name.unwrap_or_default(),
            gpu_memory_mib: self.gpu_ram.unwrap_or_default(),
            verification: self.verification.unwrap_or_default(),
            hourly_micros: hourly_micros.unwrap_or_default(),
            machine_id: self.machine_id.unwrap_or_default(),
            image: self.image_uuid.unwrap_or_default(),
            runtime: self.image_runtype.unwrap_or_default(),
            access,
        })
    }
}

#[derive(Deserialize)]
struct InstancesResponse {
    success: bool,
    #[serde(default)]
    instances: Vec<ListedInstance>,
}

#[derive(Deserialize)]
struct WorkspaceInstancesResponse {
    success: bool,
    #[serde(default)]
    instances: Vec<RawInstance>,
}

#[derive(Deserialize)]
struct ListedInstance {
    id: u64,
    label: Option<String>,
}

async fn decode_json<T: DeserializeOwned>(
    response: Response,
    operation: &'static str,
) -> Result<T> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(VastError::ResponseTooLarge { operation });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|source| VastError::Transport { operation, source })?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(VastError::ResponseTooLarge { operation });
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|source| VastError::Decode { operation, source })
}

fn invalid_response(operation: &'static str, reason: &'static str) -> VastError {
    VastError::InvalidResponse { operation, reason }
}

fn validate_cap_and_rejections(cap_micros: u64, rejected_machine_ids: &[u64]) -> Result<()> {
    if !(1..=MAX_HOURLY_MICROS).contains(&cap_micros) {
        return Err(VastError::InvalidRequest(
            "max_hourly_micros is outside the supported range",
        ));
    }
    if rejected_machine_ids.len() > 1_024 || rejected_machine_ids.contains(&0) {
        return Err(VastError::InvalidRequest("rejected_machine_ids is invalid"));
    }
    Ok(())
}

fn validate_optional_text(
    value: Option<&str>,
    max_bytes: usize,
    operation: &'static str,
    reason: &'static str,
) -> Result<()> {
    if value.is_some_and(|value| !valid_text(value, max_bytes)) {
        Err(invalid_response(operation, reason))
    } else {
        Ok(())
    }
}

fn reject_mismatch(actual: Option<&str>, expected: &str, reason: &'static str) -> Result<()> {
    if actual.is_some_and(|value| value != expected) {
        Err(invalid_response("workspace lookup", reason))
    } else {
        Ok(())
    }
}

fn parse_public_ip(value: Option<&str>) -> Result<Option<IpAddr>> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid_response("workspace lookup", "public IP is invalid"))
        })
        .transpose()
}

fn parse_jupyter_port(ports: Option<&serde_json::Value>) -> Result<Option<u16>> {
    let Some(ports) = ports else {
        return Ok(None);
    };
    let Some(bindings) = ports.as_object() else {
        return Err(invalid_response(
            "workspace lookup",
            "port mappings are invalid",
        ));
    };
    let Some(bindings) = bindings.get("8080/tcp") else {
        return Ok(None);
    };
    let Some(bindings) = bindings.as_array() else {
        return Err(invalid_response(
            "workspace lookup",
            "Jupyter port mapping is invalid",
        ));
    };
    let Some(binding) = bindings.first() else {
        return Ok(None);
    };
    let Some(port) = binding.get("HostPort").and_then(serde_json::Value::as_str) else {
        return Err(invalid_response(
            "workspace lookup",
            "Jupyter host port is invalid",
        ));
    };
    port.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .map(Some)
        .ok_or_else(|| invalid_response("workspace lookup", "Jupyter host port is invalid"))
}

fn parse_jupyter_token(value: Option<&str>) -> Result<Option<&str>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !(16..=512).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid_response(
            "workspace lookup",
            "Jupyter token is invalid",
        ));
    }
    Ok(Some(value))
}

fn workspace_access_url(ip: IpAddr, port: u16, token: &str) -> Result<WorkspaceAccessUrl> {
    let host = match ip {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    let mut url = Url::parse(&format!("https://{host}:{port}/lab"))
        .map_err(|_| invalid_response("workspace lookup", "workspace access URL is invalid"))?;
    url.query_pairs_mut().append_pair("token", token);
    Ok(WorkspaceAccessUrl(url))
}

fn validate_api_url(url: &Url) -> Result<()> {
    let local_http = url.scheme() == "http"
        && match url.host() {
            Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        };
    if url.scheme() != "https" && !local_http {
        return Err(VastError::Configuration(
            "COVENANT_VAST_API_URL must use HTTPS",
        ));
    }
    if url.host_str().is_none() {
        return Err(VastError::Configuration(
            "COVENANT_VAST_API_URL must include a host",
        ));
    }
    Ok(())
}

fn validate_digest_pinned_image(image: &str) -> Result<()> {
    if image.len() > 512 || image.chars().any(char::is_whitespace) {
        return Err(VastError::InvalidRequest(
            "image must be a digest-pinned container reference",
        ));
    }
    let Some((repository, digest)) = image.rsplit_once("@sha256:") else {
        return Err(VastError::InvalidRequest(
            "image must be pinned by SHA-256 digest",
        ));
    };
    if repository.is_empty()
        || repository.contains('@')
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(VastError::InvalidRequest(
            "image must be pinned by a valid SHA-256 digest",
        ));
    }
    Ok(())
}

fn validate_workspace_image(image: &str) -> Result<()> {
    validate_digest_pinned_image(image)?;
    if image != WORKSPACE_IMAGE {
        return Err(VastError::InvalidRequest(
            "workspace image CUDA compatibility is not registered",
        ));
    }
    Ok(())
}

fn parse_cuda_version(value: &serde_json::Number) -> Result<CudaVersion> {
    let rendered = value.to_string();
    let (major, minor) = rendered
        .split_once('.')
        .map_or((rendered.as_str(), "0"), |parts| parts);
    let minor = minor.trim_end_matches('0');
    let minor = if minor.is_empty() { "0" } else { minor };
    if major.is_empty()
        || minor.len() > 2
        || !major.bytes().all(|byte| byte.is_ascii_digit())
        || !minor.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_response(
            "offer search",
            "host CUDA compatibility is invalid",
        ));
    }
    let major = major
        .parse::<u16>()
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| invalid_response("offer search", "host CUDA compatibility is invalid"))?;
    let minor = minor
        .parse::<u16>()
        .map_err(|_| invalid_response("offer search", "host CUDA compatibility is invalid"))?;
    Ok(CudaVersion { major, minor })
}

fn validate_ssh_public_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 16_384
        || key
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        return Err(VastError::InvalidRequest("SSH public key is invalid"));
    }
    let mut fields = key.split_ascii_whitespace();
    let Some(kind) = fields.next() else {
        return Err(VastError::InvalidRequest("SSH public key is invalid"));
    };
    let Some(material) = fields.next() else {
        return Err(VastError::InvalidRequest("SSH public key is invalid"));
    };
    let supported = kind == "ssh-ed25519"
        || kind == "ssh-rsa"
        || kind.starts_with("ecdsa-sha2-")
        || kind == "sk-ssh-ed25519@openssh.com";
    if !supported
        || material.len() < 16
        || !material
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return Err(VastError::InvalidRequest("SSH public key is invalid"));
    }
    Ok(())
}

pub fn workspace_label(workload_id: &str) -> Result<String> {
    if workload_id.is_empty()
        || workload_id.len() > 64
        || !workload_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(VastError::InvalidRequest("workload ID is invalid"));
    }
    Ok(format!("covenant-workload-{workload_id}"))
}

fn validate_instance_id(instance_id: u64) -> Result<()> {
    if instance_id == 0 {
        Err(VastError::InvalidRequest("instance ID is zero"))
    } else {
        Ok(())
    }
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_ssh_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.trim() != host {
        return false;
    }
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn parse_direct_port(value: Option<i64>) -> Result<(Option<bool>, Option<u16>)> {
    match value {
        None => Ok((None, None)),
        Some(-1) => Ok((Some(false), None)),
        Some(port) if (1..=i64::from(u16::MAX)).contains(&port) => {
            Ok((Some(true), Some(port as u16)))
        }
        Some(_) => Err(invalid_response(
            "instance lookup",
            "direct port start is invalid",
        )),
    }
}

fn hourly_micros(value: f64) -> std::result::Result<u64, &'static str> {
    if !value.is_finite() || value <= 0.0 {
        return Err("hourly price is invalid");
    }
    let micros = (value * 1_000_000.0).ceil();
    if micros > u64::MAX as f64 {
        return Err("hourly price is out of range");
    }
    Ok(micros as u64)
}

fn env_u64(key: &'static str, default: u64) -> Result<u64> {
    match env::var(key) {
        Ok(value) => value
            .parse()
            .map_err(|_| VastError::Configuration(env_parse_message(key))),
        Err(_) => Ok(default),
    }
}

fn env_parse_message(key: &str) -> &'static str {
    match key {
        MAX_HOURLY_ENV => "COVENANT_VAST_MAX_HOURLY_MICROS must be an unsigned integer",
        MIN_GPU_MEMORY_ENV => "COVENANT_VAST_MIN_GPU_MEMORY_MIB must be an unsigned integer",
        DISK_GB_ENV => "COVENANT_VAST_DISK_GB must be an unsigned integer",
        _ => "environment value must be an unsigned integer",
    }
}

#[cfg(test)]
mod tests;
