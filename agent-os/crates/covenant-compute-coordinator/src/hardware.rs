//! Signed one-shot hardware challenges and durable, lease-bound evidence.
//! Merely enabling these diagnostic endpoints does not release a stronger tier.
use crate::{jobs::JobPhase, state::CoordinatorState};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path as RoutePath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use covenant_compute_attestation::{
    binding::{
        self, HardwareChallenge, HardwareEvidence, HardwareProfile, HardwareScope, HardwareVerdict,
        MAX_EVIDENCE_BYTES,
    },
    vendor_types::TrustClass,
};
use covenant_compute_protocol::{
    parse_lease_terms,
    phala::{parse_phala_ephemeral_lease_spec, parse_phala_lease_spec},
    JobKind,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum HardwareError {
    #[error("hardware request rejected: {0}")]
    Rejected(String),
    #[error("hardware service unavailable: {0}")]
    Unavailable(String),
}
impl IntoResponse for HardwareError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::Rejected(_) => (StatusCode::FORBIDDEN, self.to_string()),
            Self::Unavailable(_) => {
                tracing::error!(error = %self, "hardware service unavailable");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "hardware service unavailable".into(),
                )
            }
        };
        (status, Json(serde_json::json!({"error": message}))).into_response()
    }
}
type Result<T> = std::result::Result<T, HardwareError>;
fn rejected(s: impl ToString) -> HardwareError {
    HardwareError::Rejected(s.to_string())
}
fn unavailable(s: impl ToString) -> HardwareError {
    HardwareError::Unavailable(s.to_string())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Row {
    challenge: HardwareChallenge,
    consumed: bool,
    evidence: Option<HardwareEvidence>,
    verdict: Option<HardwareVerdict>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceOwner {
    operator: String,
    expires_at_ms: u64,
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u8,
    workload_sha256: String,
    watermark_ms: u64,
    rows: BTreeMap<Uuid, Row>,
    devices: BTreeMap<String, DeviceOwner>,
}
struct Book {
    path: PathBuf,
    _lock: File,
    data: Snapshot,
    healthy: bool,
}
impl Book {
    fn open(path: &Path, workload_sha256: String) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| unavailable("journal parent missing"))?;
        std::fs::create_dir_all(parent).map_err(unavailable)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(path.with_extension("hardware.lock"))
            .map_err(unavailable)?;
        lock.try_lock()
            .map_err(|_| unavailable("hardware journal already open"))?;
        let data = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Snapshot>(&bytes).map_err(unavailable)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Snapshot {
                version: 1,
                workload_sha256: workload_sha256.clone(),
                watermark_ms: 0,
                rows: BTreeMap::new(),
                devices: BTreeMap::new(),
            },
            Err(e) => return Err(unavailable(e)),
        };
        if data.version != 1 || data.workload_sha256 != workload_sha256 {
            return Err(unavailable("hardware journal configuration mismatch"));
        }
        Ok(Self {
            path: path.into(),
            _lock: lock,
            data,
            healthy: true,
        })
    }
    fn save(&mut self) -> Result<()> {
        if !self.healthy {
            return Err(unavailable(
                "hardware journal write failed; restart required",
            ));
        }
        let saved = (|| -> std::io::Result<()> {
            let parent = self.path.parent().unwrap();
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            tmp.write_all(&serde_json::to_vec(&self.data)?)?;
            tmp.as_file().sync_all()?;
            tmp.persist(&self.path).map_err(|e| e.error)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if saved.is_err() {
            self.healthy = false;
        }
        saved.map_err(unavailable)
    }
    fn advance(&mut self, now: u64) -> Result<()> {
        if !self.healthy || now < self.data.watermark_ms {
            return Err(unavailable("journal unavailable or clock moved backwards"));
        }
        self.data.watermark_ms = now;
        self.data
            .rows
            .retain(|_, row| row.challenge.scope.lease_expires_at_ms > now);
        self.data
            .devices
            .retain(|_, owner| owner.expires_at_ms > now);
        Ok(())
    }
}
#[derive(Clone)]
pub struct HardwareService {
    state: Arc<CoordinatorState>,
    book: Arc<Mutex<Book>>,
    workload_sha256: String,
    verify_slots: Arc<tokio::sync::Semaphore>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceResponse {
    pub evidence: HardwareEvidence,
    pub verdict: HardwareVerdict,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeRequest {
    #[serde(default)]
    host_key_index: usize,
}
impl HardwareService {
    pub fn open(
        state: Arc<CoordinatorState>,
        path: &Path,
        workload_sha256: String,
    ) -> Result<Self> {
        if workload_sha256.len() != 64 || !workload_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(rejected("workload digest must be 32-byte hex"));
        }
        let workload_sha256 = workload_sha256.to_ascii_lowercase();
        Self::configured(state, path, workload_sha256)
    }
    /// Require a buyer-signed workload for every evidence request. The empty
    /// journal policy marker is distinct from the legacy fixed-workload mode.
    pub fn open_signed_leases(state: Arc<CoordinatorState>, path: &Path) -> Result<Self> {
        Self::configured(state, path, String::new())
    }
    fn configured(
        state: Arc<CoordinatorState>,
        path: &Path,
        workload_sha256: String,
    ) -> Result<Self> {
        Ok(Self {
            state,
            book: Arc::new(Mutex::new(Book::open(path, workload_sha256.clone())?)),
            workload_sha256,
            verify_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            clock: Arc::new(epoch_ms),
        })
    }
    fn scope(&self, job_id: Uuid, key_index: usize, now: u64) -> Result<HardwareScope> {
        let job = self
            .state
            .jobs()
            .get(job_id)
            .ok_or_else(|| rejected("unknown lease"))?;
        if job.envelope.payload.kind != JobKind::LeaseSession
            || job.phase != JobPhase::Accepted
            || job.close_requested_at_ms.is_some()
        {
            return Err(rejected("lease is not live"));
        }
        let terms = parse_lease_terms(&job.envelope.payload.input)
            .map_err(rejected)?
            .ok_or_else(|| rejected("lease terms missing"))?;
        let accepted = job
            .accepted_at_ms
            .ok_or_else(|| rejected("lease not accepted"))?;
        if now < accepted || now < job.envelope.payload.issued_at_ms {
            return Err(rejected("lease clock moved backwards"));
        }
        let mut end = accepted
            .saturating_add(terms.max_duration_secs.saturating_mul(1000))
            .min(
                job.envelope
                    .payload
                    .issued_at_ms
                    .saturating_add(job.envelope.payload.deadline_ms),
            );
        let phala = parse_phala_lease_spec(&job.envelope.payload.input).map_err(rejected)?;
        let ephemeral =
            parse_phala_ephemeral_lease_spec(&job.envelope.payload.input).map_err(rejected)?;
        if phala.is_some() && ephemeral.is_some() {
            return Err(rejected("mixed Phala lease profiles"));
        }
        if phala.is_none() && ephemeral.is_none() && self.workload_sha256.is_empty() {
            return Err(rejected("buyer-signed workload pin required"));
        }
        if let Some(spec) = &phala {
            spec.validate_for_job(&job.envelope.payload)
                .map_err(rejected)?;
            if spec.operator_pubkey_b58 != job.operator_pubkey_b58 {
                return Err(rejected(
                    "lease operator differs from buyer-signed workload",
                ));
            }
            end = end.min(spec.lease_expires_at_ms);
        }
        if let Some(spec) = &ephemeral {
            spec.validate_for_job(&job.envelope.payload)
                .map_err(rejected)?;
            if spec.operator_pubkey_b58 != job.operator_pubkey_b58
                || !self.workload_sha256.is_empty()
            {
                return Err(rejected(
                    "ephemeral leases require assigned operator and signed-lease mode",
                ));
            }
            end = end.min(spec.lease_expires_at_ms);
        }
        if now >= end {
            return Err(rejected("paid lease window expired"));
        }
        let access = job
            .lease_access
            .ok_or_else(|| rejected("structured SSH identity is required"))?;
        access.validate().map_err(rejected)?;
        if access.job_id != job_id || access.ready_at_ms > now {
            return Err(rejected("lease access is not current"));
        }
        let ssh = access
            .ssh
            .ok_or_else(|| rejected("structured SSH identity is required"))?;
        ssh.validate().map_err(rejected)?;
        if let Some(spec) = &phala {
            if ssh.lease_generation != spec.lease_generation
                || ssh.transport != Some(covenant_compute_protocol::ssh::SshTransport::Tls)
                || ssh.port != 443
                || ssh.username != "covenant"
            {
                return Err(rejected(
                    "Phala channel differs from the signed lease profile",
                ));
            }
            let suffix = format!("-1022.{}", covenant_compute_protocol::phala::PHALA_GATEWAY);
            let instance = ssh
                .host
                .strip_suffix(&suffix)
                .ok_or_else(|| rejected("unexpected Phala SSH endpoint"))?;
            if instance.len() != 40
                || !instance
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(rejected("invalid Phala instance endpoint"));
            }
        }
        if let Some(spec) = &ephemeral {
            if ssh.lease_generation != spec.lease_generation
                || ssh.transport
                    != Some(covenant_compute_protocol::ssh::SshTransport::OutboundRelay)
                || ssh.host != spec.relay.host().map_err(rejected)?
                || ssh.port != 443
                || ssh.username != "covenant"
                || ssh.observed_host_keys.len() != 1
            {
                return Err(rejected(
                    "ephemeral Phala channel differs from the signed lease profile",
                ));
            }
        }
        let key = ssh
            .observed_host_keys
            .get(key_index)
            .cloned()
            .ok_or_else(|| rejected("host key missing"))?;
        Ok(HardwareScope {
            profile: if ephemeral.is_some() {
                HardwareProfile::PhalaEphemeralDstack061
            } else {
                HardwareProfile::Standard
            },
            job_id,
            operator_pubkey_b58: job.operator_pubkey_b58,
            buyer_pubkey_b58: job.envelope.payload.buyer.pubkey_base58(),
            lease_generation: ssh.lease_generation,
            host: ssh.host,
            port: ssh.port,
            transport: ssh.transport,
            username: ssh.username,
            guest_channel_key: key,
            workload_sha256: phala
                .map(|spec| spec.workload_sha256)
                .or_else(|| ephemeral.map(|spec| spec.workload_sha256))
                .unwrap_or_else(|| self.workload_sha256.clone()),
            lease_expires_at_ms: end,
        })
    }
    fn authorize(&self, job: Uuid, actor: &str) -> Result<()> {
        let row = self
            .state
            .jobs()
            .get(job)
            .ok_or_else(|| rejected("unknown lease"))?;
        if actor != row.operator_pubkey_b58 && actor != row.envelope.payload.buyer.pubkey_base58() {
            return Err(rejected(
                "request is not signed by the lease operator or buyer",
            ));
        }
        Ok(())
    }
    pub fn challenge(
        &self,
        job: Uuid,
        actor: &str,
        key_index: usize,
        now: u64,
    ) -> Result<HardwareChallenge> {
        let permit = self.read_permit(job, actor)?;
        self.challenge_admitted(job, actor, key_index, now, permit)
    }
    fn challenge_admitted(
        &self,
        job: Uuid,
        actor: &str,
        key_index: usize,
        now: u64,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<HardwareChallenge> {
        self.authorize(job, actor)?;
        let scope = self.scope(job, key_index, now)?;
        let mut book = self.book.lock();
        book.advance(now)?;
        if let Some(row) = book.data.rows.get(&job) {
            if !row.consumed && row.challenge.expires_at_ms > now && row.challenge.scope == scope {
                let challenge = row.challenge.clone();
                book.save()?;
                return Ok(challenge);
            }
        }
        if book.data.rows.len() >= 1024 && !book.data.rows.contains_key(&job) {
            return Err(unavailable("hardware challenge capacity reached"));
        }
        let challenge = HardwareChallenge::new(scope, now).map_err(rejected)?;
        book.data.rows.insert(
            job,
            Row {
                challenge: challenge.clone(),
                consumed: false,
                evidence: None,
                verdict: None,
            },
        );
        book.save()?;
        Ok(challenge)
    }
    async fn challenge_blocking(
        &self,
        job: Uuid,
        actor: &str,
        key_index: usize,
        requested_at: u64,
    ) -> Result<HardwareChallenge> {
        let permit = self.read_permit(job, actor)?;
        let service = self.clone();
        let actor = actor.to_owned();
        tokio::task::spawn_blocking(move || {
            let now = service.worker_time(requested_at)?;
            service.challenge_admitted(job, &actor, key_index, now, permit)
        })
        .await
        .map_err(unavailable)?
    }
    pub async fn submit(
        &self,
        job: Uuid,
        actor: &str,
        evidence: HardwareEvidence,
        now: u64,
    ) -> Result<EvidenceResponse> {
        let permit = self.read_permit(job, actor)?;
        if evidence.challenge.scope.operator_pubkey_b58 != actor
            || evidence.challenge.scope.job_id != job
        {
            return Err(rejected(
                "only the assigned operator may submit hardware evidence",
            ));
        }
        let service = self.clone();
        let actor = actor.to_owned();
        tokio::task::spawn_blocking(move || {
            let now = service.worker_time(now)?;
            service.submit_admitted(job, &actor, evidence, now, permit)
        })
        .await
        .map_err(unavailable)?
    }
    fn submit_admitted(
        &self,
        job: Uuid,
        actor: &str,
        evidence: HardwareEvidence,
        now: u64,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<EvidenceResponse> {
        self.authorize(job, actor)?;
        if evidence.challenge.scope.operator_pubkey_b58 != actor
            || evidence.challenge.scope.job_id != job
        {
            return Err(rejected(
                "only the assigned operator may submit hardware evidence",
            ));
        }
        self.match_live_scope(&evidence.challenge.scope, now)?;
        let cached = {
            let mut book = self.book.lock();
            book.advance(now)?;
            let row = book
                .data
                .rows
                .get_mut(&job)
                .ok_or_else(|| rejected("challenge missing"))?;
            if row.challenge != evidence.challenge {
                return Err(rejected("challenge changed"));
            }
            if row.consumed {
                if let (Some(previous), Some(verdict)) = (&row.evidence, &row.verdict) {
                    if serde_json::to_vec(previous).map_err(unavailable)?
                        == serde_json::to_vec(&evidence).map_err(unavailable)?
                        && now < verdict.expires_at_ms
                    {
                        true
                    } else {
                        return Err(rejected("challenge already consumed"));
                    }
                } else {
                    return Err(rejected("challenge already consumed"));
                }
            } else {
                evidence.challenge.validate(now).map_err(rejected)?;
                row.consumed = true;
                book.save()?;
                false
            }
        };
        if cached {
            return self.evidence_admitted(job, actor, now, permit);
        }
        // Keep the shared permit through verification and the final journal
        // commit; all filesystem and vendor work stays in this bounded worker.
        let _permit = permit;
        let verify_at = self.worker_time(now)?;
        // match_live_scope above derives the profile exclusively from the signed
        // job. Remote evidence cannot opt itself into a different policy.
        let verdict = binding::verify_evidence_for_profile(
            &evidence,
            verify_at,
            evidence.challenge.scope.profile,
        )
        .map_err(rejected)?;
        let completed_at = (self.clock)();
        self.validate_completion(&verdict, completed_at)?;
        let mut book = self.book.lock();
        book.advance(completed_at)?;
        if let Some(gpu) = &verdict.gpu_identity {
            if book
                .data
                .devices
                .get(gpu)
                .is_some_and(|p| p.operator != actor && p.expires_at_ms > completed_at)
            {
                return Err(rejected("attested GPU is bound to another live operator"));
            }
        }
        let row = book
            .data
            .rows
            .get_mut(&job)
            .ok_or_else(|| rejected("challenge removed"))?;
        if row.challenge != evidence.challenge {
            return Err(rejected("challenge superseded"));
        }
        row.evidence = Some(evidence.clone());
        row.verdict = Some(verdict.clone());
        if let Some(gpu) = &verdict.gpu_identity {
            book.data.devices.insert(
                gpu.clone(),
                DeviceOwner {
                    operator: actor.into(),
                    expires_at_ms: verdict.expires_at_ms,
                },
            );
        }
        book.save()?;
        Ok(EvidenceResponse { evidence, verdict })
    }
    fn validate_completion(&self, verdict: &HardwareVerdict, now: u64) -> Result<()> {
        // A report must still be within its collection window after blocking
        // verification. CPU-pool scheduling cannot extend a challenge.
        verdict.challenge.validate(now).map_err(rejected)?;
        if now < verdict.verified_at_ms || now >= verdict.expires_at_ms {
            return Err(rejected(
                "hardware verification expired or clock moved backwards",
            ));
        }
        self.match_live_scope(&verdict.challenge.scope, now)
    }
    fn match_live_scope(&self, scope: &HardwareScope, now: u64) -> Result<()> {
        for index in 0..8 {
            if self.scope(scope.job_id, index, now).ok().as_ref() == Some(scope) {
                return Ok(());
            }
        }
        Err(rejected(
            "current lease/endpoint/key differs from hardware scope",
        ))
    }
    pub fn evidence(&self, job: Uuid, actor: &str, now: u64) -> Result<EvidenceResponse> {
        let permit = self.read_permit(job, actor)?;
        self.evidence_admitted(job, actor, now, permit)
    }
    /// Current durable lease scope, for the key broker's final post-verification
    /// check. This is not an attestation verdict and cannot authorize a release.
    pub(crate) fn key_broker_live_scope(
        &self,
        job: Uuid,
        buyer: &str,
        now: u64,
    ) -> Result<HardwareScope> {
        let scope = self.scope(job, 0, now)?;
        if scope.buyer_pubkey_b58 != buyer
            || scope.profile != HardwareProfile::PhalaEphemeralDstack061
        {
            return Err(rejected(
                "key release requires this buyer's ephemeral lease",
            ));
        }
        Ok(scope)
    }
    /// Relay access requires a fresh independently reverified coordinator
    /// verdict and the original operator-provisioned application/instance pins.
    /// Run on a bounded blocking worker, never under the relay-state lock.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn require_relay_stream(
        &self,
        job: Uuid,
        buyer: &str,
        challenge_id: Uuid,
        expected_app_id: &str,
        expected_instance_id: &str,
        now: u64,
    ) -> Result<()> {
        let response = self.evidence(job, buyer, now)?;
        let verdict = response.verdict;
        let scope = &verdict.challenge.scope;
        if scope.profile != HardwareProfile::PhalaEphemeralDstack061
            || scope.buyer_pubkey_b58 != buyer
            || verdict.challenge.challenge_id != challenge_id
            || verdict.phala_identity.as_ref().is_none_or(|identity| {
                identity.app_id != expected_app_id || identity.instance_id != expected_instance_id
            })
        {
            return Err(rejected(
                "relay stream differs from verified buyer, challenge or boot identity",
            ));
        }
        Ok(())
    }
    fn read_permit(&self, job: Uuid, actor: &str) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.authorize(job, actor)?;
        self.verify_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| unavailable("hardware verification capacity reached"))
    }
    fn worker_time(&self, requested_at: u64) -> Result<u64> {
        let now = (self.clock)();
        if now < requested_at {
            return Err(unavailable("hardware verification clock moved backwards"));
        }
        Ok(now)
    }
    async fn evidence_blocking(
        &self,
        job: Uuid,
        actor: &str,
        requested_at: u64,
    ) -> Result<EvidenceResponse> {
        let permit = self.read_permit(job, actor)?;
        self.evidence_blocking_admitted(job, actor, requested_at, permit)
            .await
    }
    async fn evidence_blocking_admitted(
        &self,
        job: Uuid,
        actor: &str,
        requested_at: u64,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<EvidenceResponse> {
        let service = self.clone();
        let actor = actor.to_owned();
        tokio::task::spawn_blocking(move || {
            let now = service.worker_time(requested_at)?;
            service.evidence_admitted(job, &actor, now, permit)
        })
        .await
        .map_err(unavailable)?
    }
    fn evidence_admitted(
        &self,
        job: Uuid,
        actor: &str,
        now: u64,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<EvidenceResponse> {
        self.authorize(job, actor)?;
        let result = {
            let mut book = self.book.lock();
            book.advance(now)?;
            // Persist read-time progress too: a restart must not make an
            // already observed later clock disappear from the rollback guard.
            book.save()?;
            let row = book
                .data
                .rows
                .get(&job)
                .ok_or_else(|| rejected("no hardware evidence"))?;
            EvidenceResponse {
                evidence: row
                    .evidence
                    .clone()
                    .ok_or_else(|| rejected("hardware challenge has no verified evidence"))?,
                verdict: row
                    .verdict
                    .clone()
                    .ok_or_else(|| rejected("hardware verdict missing"))?,
            }
        };
        let verdict = &result.verdict;
        self.match_live_scope(&verdict.challenge.scope, now)?;
        if now < verdict.verified_at_ms
            || now >= verdict.expires_at_ms
            || verdict.reference_sha256
                != binding::reference_digest_for_profile(verdict.challenge.scope.profile)
        {
            return Err(rejected(
                "hardware verdict expired or reference set changed",
            ));
        }
        // Journal contention must not preserve the request's older vendor
        // validation time either; refresh immediately before crypto checks.
        let verify_at = self.worker_time(now)?;
        binding::verify_saved_verdict_for_profile(
            &result.evidence,
            verdict,
            &verdict.challenge.scope,
            verify_at,
            verdict.challenge.scope.profile,
        )
        .map_err(rejected)?;
        let completed_at = (self.clock)();
        // Saved verdicts may outlive the collection challenge, but never their
        // own expiry, vendor validity, or the current lease identity.
        if completed_at < verify_at || completed_at >= verdict.expires_at_ms {
            return Err(rejected(
                "hardware verdict expired or clock moved backwards",
            ));
        }
        self.match_live_scope(&verdict.challenge.scope, completed_at)?;
        let mut book = self.book.lock();
        book.advance(completed_at)?;
        if book.data.rows.get(&job).is_none_or(|row| {
            row.challenge != result.evidence.challenge || row.verdict.as_ref() != Some(verdict)
        }) {
            return Err(rejected("hardware verdict changed during verification"));
        }
        book.save()?;
        Ok(result)
    }
    /// Called before workspace grants or any future secret release. Self-claims
    /// and expired vendor results can never satisfy a stronger floor.
    pub fn require_floor(&self, job: Uuid, buyer: &str, floor: TrustClass, now: u64) -> Result<()> {
        if floor > binding::SERVED_TRUST_CEILING {
            return Err(rejected("requested hardware trust class is not released"));
        }
        let result = self.evidence(job, buyer, now)?;
        result
            .verdict
            .require_floor(&result.verdict.challenge.scope, floor, now)
            .map_err(rejected)
    }
}
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn build_from_env(
    state: Arc<CoordinatorState>,
    journal_path: &Path,
) -> Result<Option<Arc<HardwareService>>> {
    let per_lease = match std::env::var("COVENANT_COMPUTE_HARDWARE_LEASE_PINS") {
        Ok(value) if value == "1" => true,
        Ok(value) if value == "0" => false,
        Err(std::env::VarError::NotPresent) => false,
        _ => {
            return Err(rejected(
                "COVENANT_COMPUTE_HARDWARE_LEASE_PINS must be 0 or 1",
            ))
        }
    };
    if per_lease {
        if std::env::var("COVENANT_COMPUTE_HARDWARE_WORKLOAD_SHA256")
            .ok()
            .is_some_and(|value| !value.is_empty())
        {
            return Err(rejected(
                "choose signed lease pins or one diagnostic workload pin",
            ));
        }
        return HardwareService::open_signed_leases(
            state,
            &journal_path.with_extension("hardware.json"),
        )
        .map(|service| Some(Arc::new(service)));
    }
    let Some(digest) = std::env::var("COVENANT_COMPUTE_HARDWARE_WORKLOAD_SHA256")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    HardwareService::open(state, &journal_path.with_extension("hardware.json"), digest)
        .map(|s| Some(Arc::new(s)))
}
pub fn router(service: Arc<HardwareService>) -> Router {
    Router::new()
        .route("/federation/hardware/:job/challenge", post(challenge_http))
        .route("/federation/hardware/:job/evidence", post(evidence_http))
        .route("/federation/hardware/:job/verdict", get(verdict_http))
        .layer(DefaultBodyLimit::max(MAX_EVIDENCE_BYTES))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(service)
}
fn authenticate(
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: u64,
) -> Result<String> {
    let actor = headers
        .get("x-covenant-hardware-actor")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| rejected("actor missing"))?;
    let signed = headers
        .get("x-covenant-hardware-signed-at")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| rejected("signature time missing"))?;
    let signature = headers
        .get("x-covenant-hardware-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| rejected("signature missing"))?;
    binding::verify_request(actor, signature, method, path, body, signed, now).map_err(rejected)?;
    Ok(actor.into())
}
async fn challenge_http(
    State(s): State<Arc<HardwareService>>,
    RoutePath(job): RoutePath<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<HardwareChallenge>> {
    let now = epoch_ms();
    let actor = authenticate(
        &headers,
        "POST",
        &format!("/federation/hardware/{job}/challenge"),
        &body,
        now,
    )?;
    let request: ChallengeRequest = serde_json::from_slice(&body).map_err(rejected)?;
    s.challenge_blocking(job, &actor, request.host_key_index, now)
        .await
        .map(Json)
}
async fn evidence_http(
    State(s): State<Arc<HardwareService>>,
    RoutePath(job): RoutePath<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EvidenceResponse>> {
    let now = epoch_ms();
    let actor = authenticate(
        &headers,
        "POST",
        &format!("/federation/hardware/{job}/evidence"),
        &body,
        now,
    )?;
    let evidence = serde_json::from_slice(&body).map_err(rejected)?;
    s.submit(job, &actor, evidence, now).await.map(Json)
}
async fn verdict_http(
    State(s): State<Arc<HardwareService>>,
    RoutePath(job): RoutePath<Uuid>,
    headers: HeaderMap,
) -> Result<Json<EvidenceResponse>> {
    let now = epoch_ms();
    let actor = authenticate(
        &headers,
        "GET",
        &format!("/federation/hardware/{job}/verdict"),
        &[],
        now,
    )?;
    s.evidence_blocking(job, &actor, now).await.map(Json)
}

#[cfg(test)]
#[path = "hardware_tests.rs"]
mod tests;
