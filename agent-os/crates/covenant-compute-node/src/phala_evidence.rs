//! Lease-bound collection after the coordinator has recorded access. A consumed
//! challenge or an ambiguous submission is never retried as a new purchase.
#[path = "phala/evidence_relay.rs"]
mod relay;
use crate::executor::ExecutorError;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_compute_attestation::{
    binding::{
        self, GuestEvidence, HardwareChallenge, HardwareEvidence, HardwareProfile, HardwareScope,
        HardwareVerdict, MAX_EVIDENCE_BYTES,
    },
    vendor_types::TdxEventEntry,
};
use covenant_compute_protocol::{
    phala,
    ssh::{SshAccess, SshHostKey, SshTransport},
    JobEnvelopePayload, LeaseAccess,
};
use covenant_identity::LocalIdentity;
use dcap_qvl::{
    collateral::{CollateralClient, INTEL_PCS_URL},
    configs::DefaultConfig,
    http::{HttpClient, HttpResponse},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn failed(message: impl ToString) -> ExecutorError {
    ExecutorError::Failed(message.to_string())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis() as u64)
        .unwrap_or(0)
}
#[async_trait]
pub trait PhalaEvidence: Send + Sync {
    fn ephemeral_available(&self) -> bool {
        false
    }
    fn ephemeral_configured(&self, _job: &JobEnvelopePayload) -> Result<(), ExecutorError> {
        Err(failed("verified ephemeral relay is not configured"))
    }
    async fn prepare_ephemeral(
        &self,
        _job: &JobEnvelopePayload,
        _app_id: &str,
    ) -> Result<(), ExecutorError> {
        Err(failed("verified ephemeral relay is not configured"))
    }
    async fn ephemeral_ready(
        &self,
        _job: &JobEnvelopePayload,
        _app_id: &str,
        _previous: Option<&VerifiedPhalaAccess>,
    ) -> Result<VerifiedPhalaAccess, ExecutorError> {
        Err(failed("verified ephemeral relay is not configured"))
    }
    async fn ephemeral_after_access(
        &self,
        _job: &JobEnvelopePayload,
        _access: &LeaseAccess,
        _pin: &VerifiedPhalaAccess,
    ) -> Result<(), ExecutorError> {
        Err(failed("verified ephemeral relay is not configured"))
    }
    async fn after_access(
        &self,
        job: &JobEnvelopePayload,
        access: &LeaseAccess,
        collector_token: &str,
    ) -> Result<(), ExecutorError>;
}
/// A local full-verifier result. The provider and relay cannot construct this
/// authority; the backend persists it before publishing useful access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPhalaAccess {
    pub access: SshAccess,
    pub app_id: String,
    pub instance_id: String,
}
pub struct PhalaEvidenceClient {
    http: reqwest::Client,
    base: String,
    identity: Arc<LocalIdentity>,
    evidence_root: PathBuf,
    relay_origin: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceResponse {
    evidence: HardwareEvidence,
    verdict: HardwareVerdict,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Collected {
    schema_version: u8,
    verification_status: String,
    challenge: HardwareChallenge,
    identity: Value,
    dstack_quote: Quote,
}
#[derive(Deserialize)]
struct Quote {
    quote: String,
    report_data: String,
    event_log: String,
    vm_config: String,
}

fn private_directory(path: &Path) -> Result<(), ExecutorError> {
    std::fs::create_dir_all(path).map_err(|_| failed("create private evidence directory"))?;
    let meta = std::fs::symlink_metadata(path).map_err(|_| failed("read evidence directory"))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(failed("evidence directory must not be a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| failed("secure evidence directory"))?;
    }
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| failed("sync private evidence directory"))?;
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| failed("persist private evidence directory entry"))?;
    }
    Ok(())
}
fn save(path: &Path, bytes: &[u8]) -> Result<(), ExecutorError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| failed("evidence already exists or cannot be saved"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| failed("persist evidence"))?;
    File::open(
        path.parent()
            .ok_or_else(|| failed("evidence parent missing"))?,
    )
    .and_then(|parent| parent.sync_all())
    .map_err(|_| failed("sync evidence directory"))?;
    Ok(())
}
async fn body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(u16, Vec<u8>), ExecutorError> {
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(failed("evidence HTTP response exceeds bound"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| failed("incomplete evidence response"))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(failed("evidence HTTP response exceeds bound"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, bytes))
}

impl PhalaEvidenceClient {
    pub fn new(
        coordinator_url: &str,
        identity: Arc<LocalIdentity>,
        evidence_root: PathBuf,
    ) -> Result<Self, ExecutorError> {
        let url =
            reqwest::Url::parse(coordinator_url).map_err(|_| failed("invalid coordinator URL"))?;
        let loopback = matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        );
        if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(failed("hardware coordinator requires a bare HTTPS origin"));
        }
        private_directory(&evidence_root)?;
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent("Covenant-Compute-Outbound/1")
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(Duration::from_secs(8))
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|_| failed("build evidence HTTP client"))?,
            base: url.origin().ascii_serialization(),
            identity,
            evidence_root,
            relay_origin: None,
        })
    }
    pub fn with_ephemeral_relay_origin(mut self, origin: String) -> Result<Self, ExecutorError> {
        phala::PhalaRelayConfig {
            url: origin.clone(),
            session_id: uuid::Uuid::new_v4().to_string(),
        }
        .validate()
        .map_err(failed)?;
        self.relay_origin = Some(origin);
        Ok(self)
    }
    async fn coordinator(
        &self,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), ExecutorError> {
        if bytes.len() > MAX_EVIDENCE_BYTES {
            return Err(failed("oversized hardware request"));
        }
        let now = now_ms();
        let sig =
            binding::sign_request(&self.identity, "POST", path, &bytes, now).map_err(failed)?;
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .header(
                "x-covenant-hardware-actor",
                self.identity.agent_id().pubkey_base58(),
            )
            .header("x-covenant-hardware-signed-at", now.to_string())
            .header("x-covenant-hardware-signature", sig)
            .header("Content-Type", "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|_| failed("hardware request outcome unknown"))?;
        body(response, MAX_EVIDENCE_BYTES + 32 * 1024).await
    }
    async fn collect(
        &self,
        job: &JobEnvelopePayload,
        access: &LeaseAccess,
        token: &str,
    ) -> Result<(), ExecutorError> {
        let spec = phala::parse_phala_lease_spec(&job.input)
            .map_err(failed)?
            .ok_or_else(|| failed("signed Phala workload missing"))?;
        spec.validate_for_job(job).map_err(failed)?;
        access.validate().map_err(failed)?;
        let ssh = access
            .ssh
            .as_ref()
            .ok_or_else(|| failed("Phala SSH access missing"))?;
        if spec.operator_pubkey_b58 != self.identity.agent_id().pubkey_base58()
            || access.job_id != job.job_id
            || ssh.lease_generation != spec.lease_generation
            || ssh.observed_host_keys.len() != 1
            || ssh.port != 443
            || ssh.username != "covenant"
            || ssh.transport != Some(covenant_compute_protocol::ssh::SshTransport::Tls)
            || token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(failed("hardware collection lease or token mismatch"));
        }
        let suffix = format!("-1022.{}", phala::PHALA_GATEWAY);
        let instance = ssh
            .host
            .strip_suffix(&suffix)
            .filter(|value| {
                value.len() == 40
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or_else(|| failed("unexpected Phala instance endpoint"))?;
        let scope = HardwareScope {
            profile: Default::default(),
            job_id: job.job_id,
            operator_pubkey_b58: spec.operator_pubkey_b58,
            buyer_pubkey_b58: job.buyer.pubkey_base58(),
            lease_generation: spec.lease_generation,
            host: ssh.host.clone(),
            port: ssh.port,
            transport: ssh.transport,
            username: ssh.username.clone(),
            guest_channel_key: ssh.observed_host_keys[0].clone(),
            workload_sha256: spec.workload_sha256,
            lease_expires_at_ms: spec.lease_expires_at_ms,
        };
        let directory = self.evidence_root.join(job.job_id.to_string());
        std::fs::create_dir(&directory).map_err(|_| {
            failed("evidence run already exists; reconcile original run instead of retrying")
        })?;
        private_directory(&directory)?;
        save(
            &directory.join("scope.json"),
            &serde_json::to_vec(&scope).map_err(failed)?,
        )?;
        let path = format!("/federation/hardware/{}/challenge", job.job_id);
        let mut challenge = None;
        // Access publication is asynchronous. Only an explicit not-yet-live
        // refusal is retried; network-unknown outcomes are retained and fail.
        for _ in 0..20 {
            let (status, bytes) = self
                .coordinator(&path, br#"{"host_key_index":0}"#.to_vec())
                .await?;
            if status == 200 {
                let value: HardwareChallenge = serde_json::from_slice(&bytes).map_err(failed)?;
                value.validate(now_ms()).map_err(failed)?;
                if value.scope != scope {
                    return Err(failed("coordinator challenge scope mismatch"));
                }
                save(&directory.join("challenge.json"), &bytes)?;
                challenge = Some(value);
                break;
            }
            let error: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if status != 403
                || error["error"]
                    .as_str()
                    .is_none_or(|e| !e.ends_with("structured SSH identity is required"))
            {
                return Err(failed(format!(
                    "hardware challenge refused (HTTP {status})"
                )));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let challenge =
            challenge.ok_or_else(|| failed("coordinator did not record lease access"))?;
        let url = format!("https://{instance}-18090.{}/quote", phala::PHALA_GATEWAY);
        // Mark the exact request durably before the guest consumes its nonce.
        save(
            &directory.join("quote-request.json"),
            &serde_json::to_vec(&challenge).map_err(failed)?,
        )?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&challenge)
            .send()
            .await
            .map_err(|_| failed("collector quote outcome unknown; nonce will not be retried"))?;
        let (status, bytes) = body(response, MAX_EVIDENCE_BYTES).await?;
        save(&directory.join("collector-response.json"), &bytes)?;
        if status != 200 {
            return Err(failed(format!("collector refused quote (HTTP {status})")));
        }
        let collected: Collected = serde_json::from_slice(&bytes).map_err(failed)?;
        let (quote, events) = prepare(&collected, &challenge)?;
        let transport = IntelHttp {
            client: self.http.clone(),
            requests: Arc::new(AtomicUsize::new(0)),
        };
        let collateral = tokio::time::timeout(
            Duration::from_secs(70),
            CollateralClient::<DefaultConfig, _>::new(transport, INTEL_PCS_URL).fetch(&quote),
        )
        .await
        .map_err(|_| failed("Intel collateral fetch timed out"))?
        .map_err(|_| failed("Intel collateral fetch failed"))?;
        let collateral_json = serde_json::to_string(&collateral).map_err(failed)?;
        if collateral_json.len() > 128 * 1024 {
            return Err(failed("collateral exceeds bound"));
        }
        save(
            &directory.join("collateral.json"),
            collateral_json.as_bytes(),
        )?;
        let evidence = HardwareEvidence {
            challenge,
            guest: GuestEvidence::Tdx {
                quote_base64: STANDARD.encode(quote),
                collateral_json,
                events,
            },
            gpu_cc: None,
        };
        let exact = serde_json::to_vec(&evidence).map_err(failed)?;
        if exact.len() > MAX_EVIDENCE_BYTES {
            return Err(failed("evidence exceeds bound"));
        }
        let owned = evidence.clone();
        let verdict =
            tokio::task::spawn_blocking(move || binding::verify_evidence(&owned, now_ms()))
                .await
                .map_err(failed)?
                .map_err(failed)?;
        if verdict.guest_identity != format!("tdx/{instance}") {
            return Err(failed(
                "verified guest identity differs from provisioned endpoint",
            ));
        }
        save(&directory.join("evidence.json"), &exact)?;
        save(
            &directory.join("local-verdict.json"),
            &serde_json::to_vec(&verdict).map_err(failed)?,
        )?;
        let (status, bytes) = self
            .coordinator(
                &format!("/federation/hardware/{}/evidence", job.job_id),
                exact.clone(),
            )
            .await?;
        save(&directory.join("submission-response.json"), &bytes)?;
        if status != 200 {
            return Err(failed(format!(
                "hardware submission refused (HTTP {status})"
            )));
        }
        let accepted: EvidenceResponse = serde_json::from_slice(&bytes).map_err(failed)?;
        if serde_json::to_vec(&accepted.evidence).map_err(failed)? != exact {
            return Err(failed("coordinator returned different evidence"));
        }
        binding::verify_saved_verdict(&accepted.evidence, &accepted.verdict, &scope, now_ms())
            .map_err(failed)?;
        Ok(())
    }
}
#[async_trait]
impl PhalaEvidence for PhalaEvidenceClient {
    fn ephemeral_available(&self) -> bool {
        self.relay_origin.is_some()
    }
    fn ephemeral_configured(&self, job: &JobEnvelopePayload) -> Result<(), ExecutorError> {
        self.ephemeral_spec(job).map(|_| ())
    }
    async fn prepare_ephemeral(
        &self,
        job: &JobEnvelopePayload,
        app_id: &str,
    ) -> Result<(), ExecutorError> {
        self.open_relay_session(job, app_id).await
    }
    async fn ephemeral_ready(
        &self,
        job: &JobEnvelopePayload,
        app_id: &str,
        previous: Option<&VerifiedPhalaAccess>,
    ) -> Result<VerifiedPhalaAccess, ExecutorError> {
        self.verify_ephemeral_readiness(job, app_id, previous).await
    }
    async fn ephemeral_after_access(
        &self,
        job: &JobEnvelopePayload,
        access: &LeaseAccess,
        pin: &VerifiedPhalaAccess,
    ) -> Result<(), ExecutorError> {
        self.submit_ephemeral_evidence(job, access, pin).await
    }
    async fn after_access(
        &self,
        job: &JobEnvelopePayload,
        access: &LeaseAccess,
        collector_token: &str,
    ) -> Result<(), ExecutorError> {
        self.collect(job, access, collector_token).await
    }
}
fn prepare(
    collected: &Collected,
    challenge: &HardwareChallenge,
) -> Result<(Vec<u8>, Vec<TdxEventEntry>), ExecutorError> {
    if collected.schema_version != 1
        || collected.verification_status != "unverified"
        || collected.challenge != *challenge
    {
        return Err(failed("collector challenge mismatch"));
    }
    challenge.validate(now_ms()).map_err(failed)?;
    let q = &collected.dstack_quote;
    if q.quote.len() > 48 * 1024 || q.event_log.len() > MAX_EVIDENCE_BYTES {
        return Err(failed("quote or event log exceeds bound"));
    }
    let quote = hex::decode(&q.quote).map_err(failed)?;
    if STANDARD.encode(&quote).len() > 32 * 1024
        || hex::decode(&q.report_data).map_err(failed)?
            != challenge.guest_report_data().map_err(failed)?
    {
        return Err(failed("quote report-data mismatch or oversized quote"));
    }
    let events: Vec<TdxEventEntry> = serde_json::from_str(&q.event_log).map_err(failed)?;
    if events.len() > 256 {
        return Err(failed("too many runtime events"));
    }
    binding::parse_events(&events).map_err(failed)?;
    let scope = &challenge.scope;
    for (field, value) in [
        ("host", json!(scope.host)),
        ("port", json!(scope.port)),
        ("username", json!(scope.username)),
        ("transport", json!(scope.transport)),
        ("guest_channel_key", json!(scope.guest_channel_key)),
    ] {
        if collected.identity.get(field) != Some(&value) {
            return Err(failed("collector channel identity mismatch"));
        }
    }
    let info = &collected.identity["guest_info"];
    if info["compose_hash"].as_str() != Some(&scope.workload_sha256)
        || info["vm_config"].as_str() != Some(&q.vm_config)
    {
        return Err(failed("collector workload or VM config changed"));
    }
    Ok((quote, events))
}

#[derive(Clone)]
struct IntelHttp {
    client: reqwest::Client,
    requests: Arc<AtomicUsize>,
}
fn intel_url(raw: &str) -> anyhow::Result<()> {
    anyhow::ensure!(raw.len() <= 8192, "collateral URL exceeds bound");
    let url = reqwest::Url::parse(raw)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "collateral URL must be plain HTTPS"
    );
    let allowed = match url.host_str() {
        Some("api.trustedservices.intel.com") => {
            url.path().starts_with("/sgx/certification/v4/")
                || url.path().starts_with("/tdx/certification/v4/")
        }
        Some("certificates.trustedservices.intel.com") => {
            url.path() == "/IntelSGXRootCA.der" && url.query().is_none()
        }
        _ => false,
    };
    anyhow::ensure!(allowed, "collateral URL outside fixed Intel allowlist");
    Ok(())
}
impl HttpClient for IntelHttp {
    async fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
        intel_url(url)?;
        anyhow::ensure!(
            self.requests.fetch_add(1, Ordering::Relaxed) < 8,
            "too many collateral requests"
        );
        let response = self.client.get(url).send().await?;
        anyhow::ensure!(
            response.headers().len() <= 64,
            "too many collateral headers"
        );
        let mut header_bytes = 0;
        let mut headers = BTreeMap::new();
        for (name, value) in response.headers() {
            header_bytes += name.as_str().len() + value.as_bytes().len();
            anyhow::ensure!(header_bytes <= 64 * 1024, "collateral headers exceed bound");
            headers.insert(name.as_str().into(), value.to_str()?.into());
        }
        let (status, body) = body(response, 256 * 1024).await?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collateral_cannot_redirect_to_guest_selected_services() {
        for url in [
            "http://api.trustedservices.intel.com/sgx/certification/v4/pckcrl",
            "https://api.trustedservices.intel.com.evil.example/sgx/certification/v4/pckcrl",
            "https://user@api.trustedservices.intel.com/sgx/certification/v4/pckcrl",
            "https://api.trustedservices.intel.com:8443/sgx/certification/v4/pckcrl",
            "https://certificates.trustedservices.intel.com/IntelSGXRootCA.der?redirect=evil",
            "https://127.0.0.1/sgx/certification/v4/pckcrl",
        ] {
            assert!(intel_url(url).is_err(), "{url}");
        }
        assert!(intel_url(
            "https://api.trustedservices.intel.com/tdx/certification/v4/tcb?fmspc=001122334455"
        )
        .is_ok());
        assert!(
            intel_url("https://certificates.trustedservices.intel.com/IntelSGXRootCA.der").is_ok()
        );
    }
    #[test]
    fn evidence_records_are_private_durable_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run");
        private_directory(&path).unwrap();
        let record = path.join("quote-request.json");
        save(&record, b"original").unwrap();
        assert!(save(&record, b"replacement").is_err());
        assert_eq!(std::fs::read(&record).unwrap(), b"original");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&record).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let link = directory.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(private_directory(&link).is_err());
        }
    }
}
