#![deny(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use covenant_compute::{
    AppCatalog, ComputeApp, ComputeClient, ComputeError, ComputeJob, ComputeOffer, ComputeReceipt,
    HttpComputeProvider, JobStatus, LaunchPlan, LaunchRequest, ProviderApiError,
};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use url::{Host, Url};

const DEFAULT_CONTROL_PLANE: &str = "https://compute.opencovenant.org";
const JUPYTER_SETUP_GUIDE_URL: &str = "https://docs.vast.ai/guides/instances/connect/jupyter";

struct DesktopState {
    catalog: AppCatalog,
    base_provider: Option<HttpComputeProvider>,
    environment_client: ClientTemplate,
    active_client: RwLock<ActiveClient>,
    jobs: RwLock<BTreeMap<String, ComputeJob>>,
    plans: RwLock<BTreeMap<String, ReviewedPlan>>,
    endpoint_label: Option<String>,
}

#[derive(Clone)]
struct ClientTemplate {
    client: Option<ComputeClient>,
    auth_source: AuthSource,
    configuration_error: Option<String>,
}

struct ActiveClient {
    template: ClientTemplate,
    generation: u64,
    in_flight_mutations: usize,
}

#[derive(Clone)]
struct ClientSnapshot {
    client: ComputeClient,
    generation: u64,
}

#[derive(Clone)]
struct RuntimeSnapshot {
    client: Option<ComputeClient>,
    generation: u64,
    authentication: AuthenticationMetadata,
    configuration_error: Option<String>,
}

struct MutationSnapshot<'a> {
    active_client: &'a RwLock<ActiveClient>,
    client: ComputeClient,
    generation: u64,
}

impl Drop for MutationSnapshot<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_client.write() {
            if active.generation == self.generation {
                active.in_flight_mutations = active.in_flight_mutations.saturating_sub(1);
            }
        }
    }
}

#[derive(Clone)]
struct ReviewedPlan {
    request: LaunchRequest,
    plan: LaunchPlan,
}

impl DesktopState {
    fn from_environment() -> Self {
        let base_url = std::env::var("COVENANT_COMPUTE_API_BASE")
            .unwrap_or_else(|_| DEFAULT_CONTROL_PLANE.to_owned());
        let environment_token = match std::env::var("COVENANT_COMPUTE_API_TOKEN") {
            Ok(token) => Ok(Some(token)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(ComputeError::InvalidProviderToken),
        };
        Self::from_configuration(&base_url, environment_token)
    }

    fn from_configuration(
        base_url: &str,
        environment_token: Result<Option<String>, ComputeError>,
    ) -> Self {
        let catalog = AppCatalog::builtin();
        let endpoint_label = endpoint_label(base_url);
        let base_provider = HttpComputeProvider::new(base_url);
        let environment_client = match (&base_provider, environment_token) {
            (Ok(provider), Ok(Some(token))) => match provider.clone().with_bearer_token(&token) {
                Ok(provider) => ClientTemplate {
                    client: Some(ComputeClient::new(catalog.clone(), Arc::new(provider))),
                    auth_source: AuthSource::Environment,
                    configuration_error: None,
                },
                Err(error) => ClientTemplate {
                    client: Some(ComputeClient::new(
                        catalog.clone(),
                        Arc::new(provider.clone()),
                    )),
                    auth_source: AuthSource::None,
                    configuration_error: Some(error.to_string()),
                },
            },
            (Ok(provider), Ok(None)) => ClientTemplate {
                client: Some(ComputeClient::new(
                    catalog.clone(),
                    Arc::new(provider.clone()),
                )),
                auth_source: AuthSource::None,
                configuration_error: None,
            },
            (Ok(provider), Err(error)) => ClientTemplate {
                client: Some(ComputeClient::new(
                    catalog.clone(),
                    Arc::new(provider.clone()),
                )),
                auth_source: AuthSource::None,
                configuration_error: Some(error.to_string()),
            },
            (Err(error), _) => ClientTemplate {
                client: None,
                auth_source: AuthSource::None,
                configuration_error: Some(error.to_string()),
            },
        };

        Self {
            catalog,
            base_provider: base_provider.ok(),
            active_client: RwLock::new(ActiveClient {
                template: environment_client.clone(),
                generation: 0,
                in_flight_mutations: 0,
            }),
            environment_client,
            jobs: RwLock::new(BTreeMap::new()),
            plans: RwLock::new(BTreeMap::new()),
            endpoint_label,
        }
    }

    fn client_snapshot(&self) -> Result<ClientSnapshot, CommandError> {
        let snapshot = self.runtime_snapshot()?;
        let client = snapshot.client.ok_or_else(|| CommandError {
            code: "runtime_not_configured",
            message: snapshot
                .configuration_error
                .clone()
                .unwrap_or_else(|| "compute runtime is not configured".to_owned()),
        })?;
        Ok(ClientSnapshot {
            client,
            generation: snapshot.generation,
        })
    }

    fn runtime_snapshot(&self) -> Result<RuntimeSnapshot, CommandError> {
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        Ok(RuntimeSnapshot {
            client: active.template.client.clone(),
            generation: active.generation,
            authentication: AuthenticationMetadata {
                source: active.template.auth_source,
            },
            configuration_error: active.template.configuration_error.clone(),
        })
    }

    fn mutation_snapshot(&self) -> Result<MutationSnapshot<'_>, CommandError> {
        let mut active = self
            .active_client
            .write()
            .map_err(|_| CommandError::internal())?;
        let client = active.template.client.clone().ok_or_else(|| CommandError {
            code: "runtime_not_configured",
            message: active
                .template
                .configuration_error
                .clone()
                .unwrap_or_else(|| "compute runtime is not configured".to_owned()),
        })?;
        active.in_flight_mutations = active
            .in_flight_mutations
            .checked_add(1)
            .ok_or_else(CommandError::internal)?;
        Ok(MutationSnapshot {
            active_client: &self.active_client,
            client,
            generation: active.generation,
        })
    }

    fn configure_session(&self, token: &str) -> Result<AuthenticationMetadata, CommandError> {
        let provider = self
            .base_provider
            .clone()
            .ok_or_else(|| CommandError {
                code: "runtime_not_configured",
                message: "configure a valid compute control-plane endpoint first".to_owned(),
            })?
            .with_bearer_token(token)?;
        let template = ClientTemplate {
            client: Some(ComputeClient::new(self.catalog.clone(), Arc::new(provider))),
            auth_source: AuthSource::Session,
            configuration_error: None,
        };
        self.replace_client(template)
    }

    fn clear_session(&self) -> Result<AuthenticationMetadata, CommandError> {
        self.replace_client(self.environment_client.clone())
    }

    fn replace_client(
        &self,
        template: ClientTemplate,
    ) -> Result<AuthenticationMetadata, CommandError> {
        let mut active = self
            .active_client
            .write()
            .map_err(|_| CommandError::internal())?;
        if active.in_flight_mutations != 0 {
            return Err(CommandError::mutation_in_progress());
        }
        let generation = active
            .generation
            .checked_add(1)
            .ok_or_else(CommandError::internal)?;
        let mut jobs = self.jobs.write().map_err(|_| CommandError::internal())?;
        let mut plans = self.plans.write().map_err(|_| CommandError::internal())?;
        jobs.clear();
        plans.clear();
        let source = template.auth_source;
        *active = ActiveClient {
            template,
            generation,
            in_flight_mutations: 0,
        };
        Ok(AuthenticationMetadata { source })
    }

    fn ensure_generation(&self, generation: u64) -> Result<(), CommandError> {
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        if active.generation != generation {
            return Err(CommandError::auth_changed());
        }
        Ok(())
    }

    fn store_job(&self, generation: u64, job: ComputeJob) -> Result<JobView, CommandError> {
        let view = JobView::from(&job);
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        if active.generation != generation {
            return Err(CommandError::auth_changed());
        }
        self.jobs
            .write()
            .map_err(|_| CommandError::internal())?
            .insert(job.id.clone(), job);
        Ok(view)
    }

    fn store_plan(
        &self,
        generation: u64,
        idempotency_key: String,
        reviewed: ReviewedPlan,
    ) -> Result<(), CommandError> {
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        if active.generation != generation {
            return Err(CommandError::auth_changed());
        }
        let mut plans = self.plans.write().map_err(|_| CommandError::internal())?;
        if plans.len() >= 64 && !plans.contains_key(&idempotency_key) {
            return Err(CommandError {
                code: "too_many_reviewed_plans",
                message: "start the application again before reviewing more launches".to_owned(),
            });
        }
        plans.insert(idempotency_key, reviewed);
        Ok(())
    }

    fn reviewed_plan(
        &self,
        generation: u64,
        idempotency_key: &str,
    ) -> Result<ReviewedPlan, CommandError> {
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        if active.generation != generation {
            return Err(CommandError::auth_changed());
        }
        self.plans
            .read()
            .map_err(|_| CommandError::internal())?
            .get(idempotency_key)
            .cloned()
            .ok_or_else(|| CommandError {
                code: "quote_not_reviewed",
                message: "review the exact quote again before launching".to_owned(),
            })
    }

    fn store_jobs(
        &self,
        generation: u64,
        jobs: Vec<ComputeJob>,
    ) -> Result<Vec<JobView>, CommandError> {
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        if active.generation != generation {
            return Err(CommandError::auth_changed());
        }
        let mut stored = self.jobs.write().map_err(|_| CommandError::internal())?;
        for job in jobs {
            stored.insert(job.id.clone(), job);
        }
        Ok(stored.values().map(JobView::from).collect())
    }

    fn cached_jobs(&self, generation: u64) -> Result<Vec<JobView>, CommandError> {
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        if active.generation != generation {
            return Err(CommandError::auth_changed());
        }
        Ok(self
            .jobs
            .read()
            .map_err(|_| CommandError::internal())?
            .values()
            .map(JobView::from)
            .collect())
    }

    fn open_access_if_current(
        &self,
        generation: u64,
        app: &AppHandle,
        access_url: &Url,
    ) -> Result<(), CommandError> {
        let active = self
            .active_client
            .read()
            .map_err(|_| CommandError::internal())?;
        if active.generation != generation {
            return Err(CommandError::auth_changed());
        }
        app.opener()
            .open_url(access_url.as_str(), None::<&str>)
            .map_err(|_| CommandError {
                code: "open_failed",
                message: "the operating system could not open the workload".to_owned(),
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthSource {
    None,
    Environment,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuthenticationMetadata {
    source: AuthSource,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeState {
    Connected,
    Offline,
    Degraded,
}

#[derive(Debug, Serialize)]
struct RuntimeStatus {
    state: RuntimeState,
    endpoint_label: Option<String>,
    message: Option<String>,
    authentication: AuthenticationMetadata,
    token_required: bool,
}

#[derive(Debug, Serialize)]
struct JobView {
    id: String,
    app_id: String,
    offer_id: String,
    status: JobStatus,
    maximum_usdc_micros: u64,
    access_ready: bool,
    error: Option<String>,
    receipt: Option<ComputeReceipt>,
}

impl From<&ComputeJob> for JobView {
    fn from(job: &ComputeJob) -> Self {
        Self {
            id: job.id.clone(),
            app_id: job.app_id.clone(),
            offer_id: job.offer_id.clone(),
            status: job.status,
            maximum_usdc_micros: job.maximum_usdc_micros,
            access_ready: job.status == JobStatus::Running && job.access_url.is_some(),
            error: job.error.clone(),
            receipt: job.receipt.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct CommandError {
    code: &'static str,
    message: String,
}

impl CommandError {
    fn internal() -> Self {
        Self {
            code: "internal_error",
            message: "the desktop runtime could not update its local state".to_owned(),
        }
    }

    fn auth_changed() -> Self {
        Self {
            code: "authentication_changed",
            message: "authentication changed while the request was in progress; try again"
                .to_owned(),
        }
    }

    fn mutation_in_progress() -> Self {
        Self {
            code: "request_in_progress",
            message: "wait for the current launch or stop request before changing access"
                .to_owned(),
        }
    }

    fn provider_api(error: &ProviderApiError) -> Self {
        Self {
            code: provider_command_code(error.code()),
            message: error.message().to_owned(),
        }
    }
}

impl From<ComputeError> for CommandError {
    fn from(error: ComputeError) -> Self {
        if let ComputeError::ProviderApi(api_error) = &error {
            return Self::provider_api(api_error);
        }
        let code = match error {
            ComputeError::UnknownApp(_) => "unknown_app",
            ComputeError::AppUnavailable(_) => "app_unavailable",
            ComputeError::DurationExceeded { .. } => "duration_exceeded",
            ComputeError::ZeroBudget => "invalid_budget",
            ComputeError::NoCompatibleOffer => "no_compatible_offer",
            ComputeError::ProviderTransport => "provider_unreachable",
            ComputeError::ProviderStatus(_) => "provider_error",
            ComputeError::ProviderResponse | ComputeError::ProviderResponseTooLarge => {
                "invalid_provider_response"
            }
            ComputeError::InvalidJobId => "invalid_job_id",
            ComputeError::InvalidIdempotencyKey | ComputeError::InvalidLaunchPlan => {
                "invalid_launch"
            }
            ComputeError::InvalidProviderUrl
            | ComputeError::InsecureProviderUrl
            | ComputeError::InvalidProviderToken => "invalid_provider_configuration",
            _ => "compute_error",
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

fn provider_command_code(code: &str) -> &'static str {
    match code {
        "unauthorized" => "unauthorized",
        "missing_idempotency_key" => "missing_idempotency_key",
        "invalid_launch_plan" => "invalid_launch_plan",
        "stale_offer" => "stale_offer",
        "invalid_idempotency_key" => "invalid_idempotency_key",
        "invalid_job_id" => "invalid_job_id",
        "job_not_found" => "job_not_found",
        "idempotency_conflict" => "idempotency_conflict",
        "spend_cap_exceeded" => "spend_cap_exceeded",
        "provider_unavailable" => "provider_unavailable",
        "internal_error" => "internal_error",
        _ => "provider_error",
    }
}

fn authentication_required(error: &ComputeError) -> bool {
    matches!(error, ComputeError::ProviderStatus(401 | 403))
        || matches!(error, ComputeError::ProviderApi(error) if error.status() == 401)
}

#[tauri::command]
async fn runtime_status(state: State<'_, DesktopState>) -> Result<RuntimeStatus, CommandError> {
    let snapshot = state.runtime_snapshot()?;
    let Some(client) = snapshot.client else {
        return Ok(RuntimeStatus {
            state: RuntimeState::Degraded,
            endpoint_label: state.endpoint_label.clone(),
            message: snapshot.configuration_error,
            authentication: snapshot.authentication,
            token_required: false,
        });
    };

    let result = client.offers().await;
    state.ensure_generation(snapshot.generation)?;
    Ok(match result {
        Ok(_) => RuntimeStatus {
            state: RuntimeState::Connected,
            endpoint_label: state.endpoint_label.clone(),
            message: None,
            authentication: snapshot.authentication,
            token_required: false,
        },
        Err(ComputeError::ProviderTransport) => RuntimeStatus {
            state: RuntimeState::Offline,
            endpoint_label: state.endpoint_label.clone(),
            message: Some("compute control plane is unreachable".to_owned()),
            authentication: snapshot.authentication,
            token_required: false,
        },
        Err(error) if authentication_required(&error) => RuntimeStatus {
            state: RuntimeState::Degraded,
            endpoint_label: state.endpoint_label.clone(),
            message: Some("a private-beta access token is required or was not accepted".to_owned()),
            authentication: snapshot.authentication,
            token_required: true,
        },
        Err(error) => RuntimeStatus {
            state: RuntimeState::Degraded,
            endpoint_label: state.endpoint_label.clone(),
            message: Some(error.to_string()),
            authentication: snapshot.authentication,
            token_required: false,
        },
    })
}

#[tauri::command]
fn configure_session_token(
    state: State<'_, DesktopState>,
    token: String,
) -> Result<AuthenticationMetadata, CommandError> {
    state.configure_session(&token)
}

#[tauri::command]
fn clear_session_token(
    state: State<'_, DesktopState>,
) -> Result<AuthenticationMetadata, CommandError> {
    state.clear_session()
}

#[tauri::command]
fn list_apps(state: State<'_, DesktopState>) -> Vec<ComputeApp> {
    state.catalog.apps().to_vec()
}

#[tauri::command]
async fn list_offers(state: State<'_, DesktopState>) -> Result<Vec<ComputeOffer>, CommandError> {
    let snapshot = state.client_snapshot()?;
    let offers = snapshot.client.offers().await?;
    state.ensure_generation(snapshot.generation)?;
    Ok(offers)
}

#[tauri::command]
async fn plan_job(
    state: State<'_, DesktopState>,
    request: LaunchRequest,
    idempotency_key: String,
) -> Result<LaunchPlan, CommandError> {
    if !valid_idempotency_key(&idempotency_key) {
        return Err(CommandError {
            code: "invalid_launch",
            message: "the launch idempotency key is invalid".to_owned(),
        });
    }

    let snapshot = state.client_snapshot()?;
    let plan = snapshot.client.plan(request.clone()).await?;
    state.store_plan(
        snapshot.generation,
        idempotency_key,
        ReviewedPlan {
            request,
            plan: plan.clone(),
        },
    )?;
    Ok(plan)
}

#[tauri::command]
async fn launch_job(
    state: State<'_, DesktopState>,
    request: LaunchRequest,
    idempotency_key: String,
) -> Result<JobView, CommandError> {
    let snapshot = state.mutation_snapshot()?;
    let reviewed = state.reviewed_plan(snapshot.generation, &idempotency_key)?;
    if reviewed.request != request {
        return Err(CommandError {
            code: "quote_changed",
            message: "the launch limits changed after quote review".to_owned(),
        });
    }
    let job = snapshot
        .client
        .launch_plan(&reviewed.plan, &idempotency_key)
        .await?;
    state.store_job(snapshot.generation, job)
}

#[tauri::command]
async fn get_job(state: State<'_, DesktopState>, id: String) -> Result<JobView, CommandError> {
    let snapshot = state.client_snapshot()?;
    let job = snapshot.client.job(&id).await?;
    state.store_job(snapshot.generation, job)
}

#[tauri::command]
async fn list_jobs(state: State<'_, DesktopState>) -> Result<Vec<JobView>, CommandError> {
    let snapshot = state.client_snapshot()?;
    match snapshot.client.jobs().await {
        Ok(jobs) => state.store_jobs(snapshot.generation, jobs),
        Err(error) => {
            let stored = state.cached_jobs(snapshot.generation)?;
            if stored.is_empty() {
                Err(error.into())
            } else {
                Ok(stored)
            }
        }
    }
}

#[tauri::command]
async fn cancel_job(state: State<'_, DesktopState>, id: String) -> Result<JobView, CommandError> {
    let snapshot = state.mutation_snapshot()?;
    let job = snapshot.client.cancel(&id).await?;
    state.store_job(snapshot.generation, job)
}

#[tauri::command]
async fn open_access_url(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: String,
) -> Result<(), CommandError> {
    let snapshot = state.client_snapshot()?;
    let job = snapshot.client.job(&id).await?;
    let view = state.store_job(snapshot.generation, job.clone())?;
    if !view.access_ready {
        return Err(CommandError {
            code: "access_not_ready",
            message: "the workload access endpoint is not ready".to_owned(),
        });
    }

    let access_url = job.access_url.ok_or_else(|| CommandError {
        code: "access_not_ready",
        message: "the workload access endpoint is not ready".to_owned(),
    })?;
    let access_url = validate_access_url(&access_url)?;
    state.open_access_if_current(snapshot.generation, &app, &access_url)
}

#[tauri::command]
fn open_jupyter_setup_guide(app: AppHandle) -> Result<(), CommandError> {
    app.opener()
        .open_url(JUPYTER_SETUP_GUIDE_URL, None::<&str>)
        .map_err(|_| CommandError {
            code: "open_failed",
            message: "the operating system could not open the setup guide".to_owned(),
        })
}

fn endpoint_label(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn validate_access_url(value: &str) -> Result<Url, CommandError> {
    if value.len() > 4_096 {
        return Err(invalid_access_url());
    }

    let url = Url::parse(value).map_err(|_| invalid_access_url())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_access_url());
    }

    let secure = url.scheme() == "https";
    let loopback = url.scheme() == "http"
        && match url.host() {
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            _ => false,
        };
    if !secure && !loopback {
        return Err(invalid_access_url());
    }

    Ok(url)
}

fn invalid_access_url() -> CommandError {
    CommandError {
        code: "invalid_access_url",
        message: "the provider returned an unsafe workload access endpoint".to_owned(),
    }
}

fn valid_idempotency_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DesktopState::from_environment())
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            configure_session_token,
            clear_session_token,
            list_apps,
            list_offers,
            plan_job,
            launch_job,
            get_job,
            list_jobs,
            cancel_job,
            open_access_url,
            open_jupyter_setup_guide,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Covenant Compute");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_job(id: &str) -> ComputeJob {
        ComputeJob {
            id: id.to_owned(),
            app_id: "gpu-workspace".to_owned(),
            offer_id: "offer-1".to_owned(),
            status: JobStatus::Running,
            maximum_usdc_micros: 100_000,
            access_url: None,
            error: None,
            receipt: None,
        }
    }

    #[test]
    fn endpoint_label_never_exposes_credentials_or_paths() {
        assert_eq!(
            endpoint_label("https://user:secret@compute.example/v1?token=secret"),
            Some("compute.example".to_owned())
        );
    }

    #[test]
    fn access_urls_require_https_except_for_ip_loopback() {
        assert!(validate_access_url("https://session.example/workload?id=123").is_ok());
        assert!(validate_access_url("http://127.0.0.1:8188").is_ok());
        assert!(validate_access_url("http://[::1]:8188").is_ok());
        assert!(validate_access_url("http://session.example").is_err());
        assert!(validate_access_url("file:///tmp/session").is_err());
        assert!(validate_access_url("http://localhost:8188").is_err());
    }

    #[test]
    fn access_urls_reject_embedded_credentials() {
        assert!(validate_access_url("https://user:secret@session.example").is_err());
    }

    #[test]
    fn reviewed_plan_keys_use_the_same_restricted_wire_grammar() {
        assert!(valid_idempotency_key("launch-4e732158_1"));
        assert!(!valid_idempotency_key(""));
        assert!(!valid_idempotency_key("contains whitespace"));
        assert!(!valid_idempotency_key(&"a".repeat(129)));
    }

    #[test]
    fn safe_provider_codes_survive_the_native_command_boundary() {
        assert_eq!(provider_command_code("stale_offer"), "stale_offer");
        assert_eq!(
            provider_command_code("spend_cap_exceeded"),
            "spend_cap_exceeded"
        );
        assert_eq!(
            provider_command_code("idempotency_conflict"),
            "idempotency_conflict"
        );
        assert_eq!(provider_command_code("untrusted_code"), "provider_error");
    }

    #[test]
    fn session_auth_replaces_and_restores_environment_auth() {
        let state = DesktopState::from_configuration(
            "http://127.0.0.1:8787",
            Ok(Some("environment-token".to_owned())),
        );
        assert_eq!(
            state.runtime_snapshot().unwrap().authentication.source,
            AuthSource::Environment
        );

        let metadata = state.configure_session("session-token").unwrap();
        assert_eq!(metadata.source, AuthSource::Session);
        assert_eq!(
            state.runtime_snapshot().unwrap().authentication.source,
            AuthSource::Session
        );

        let metadata = state.clear_session().unwrap();
        assert_eq!(metadata.source, AuthSource::Environment);
        assert_eq!(
            state.runtime_snapshot().unwrap().authentication.source,
            AuthSource::Environment
        );
    }

    #[test]
    fn invalid_session_auth_does_not_replace_the_active_client() {
        let state = DesktopState::from_configuration("http://127.0.0.1:8787", Ok(None));
        let generation = state.runtime_snapshot().unwrap().generation;

        assert!(state.configure_session(" token-with-whitespace").is_err());
        let snapshot = state.runtime_snapshot().unwrap();
        assert_eq!(snapshot.authentication.source, AuthSource::None);
        assert_eq!(snapshot.generation, generation);
    }

    #[test]
    fn changing_auth_clears_owner_scoped_jobs() {
        let state = DesktopState::from_configuration("http://127.0.0.1:8787", Ok(None));
        state
            .jobs
            .write()
            .unwrap()
            .insert("job-owner-a".to_owned(), running_job("job-owner-a"));

        state.configure_session("owner-b-token").unwrap();

        assert!(state.jobs.read().unwrap().is_empty());
    }

    #[test]
    fn stale_auth_generation_cannot_repopulate_jobs() {
        let state = DesktopState::from_configuration("http://127.0.0.1:8787", Ok(None));
        let old_generation = state.runtime_snapshot().unwrap().generation;
        state.configure_session("owner-b-token").unwrap();

        let error = state
            .store_job(old_generation, running_job("job-owner-a"))
            .unwrap_err();

        assert_eq!(error.code, "authentication_changed");
        assert!(state.jobs.read().unwrap().is_empty());
    }

    #[test]
    fn auth_change_waits_for_in_flight_mutations() {
        let state = DesktopState::from_configuration("http://127.0.0.1:8787", Ok(None));
        let generation = state.runtime_snapshot().unwrap().generation;
        let mutation = state.mutation_snapshot().unwrap();

        let error = state.configure_session("owner-b-token").unwrap_err();

        assert_eq!(error.code, "request_in_progress");
        let snapshot = state.runtime_snapshot().unwrap();
        assert_eq!(snapshot.generation, generation);
        assert_eq!(snapshot.authentication.source, AuthSource::None);

        drop(mutation);
        assert_eq!(
            state.configure_session("owner-b-token").unwrap().source,
            AuthSource::Session
        );
    }
}
