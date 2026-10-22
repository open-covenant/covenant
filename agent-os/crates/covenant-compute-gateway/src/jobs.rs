//! Paid jobs that take minutes rather than seconds. The customer pays once,
//! the payment settles before any work starts, and a free status URL reports
//! progress and then the result. A job that fails after it was paid is
//! refunded in full, from the merchant wallet that received the payment, to
//! the account that paid.
//!
//! Every state that money or work depends on is written to disk before the
//! step it guards: the verified payment before it is broadcast, the signed
//! coordinator job before it is submitted, and the signed refund before it is
//! sent. A restart resumes from the same bytes.
use crate::{
    bridge::{RefundBuild, RefundStatus},
    GatewayError, MemoBinding, PaymentTerms, Result, SettlementStatus, VerifiedPayment,
};
use async_trait::async_trait;
use axum::{
    extract::{Path as UrlPath, Query, State as Shared},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use covenant_compute_buyer::{BuyerConfig, BuyerError, JobRequest};
use covenant_compute_protocol::{swarm_input, JobKind, SignedJobEnvelope, SwarmOrder, SWARM_MODEL};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use uuid::Uuid;

/// How long a quote can be paid.
const QUOTE_TTL_MS: u64 = 90_000;
/// Unpaid quotes held in memory at once.
const MAX_QUOTES: usize = 1024;
/// Jobs on disk before new quotes stop. Finished jobs are deleted after
/// [`RESULT_RETENTION_MS`], so this is reached only under abuse.
const MAX_RECORDS: usize = 50_000;
/// Payments being confirmed at once, in total and from one payer. A payment
/// that never lands holds its place for a few minutes.
const MAX_PAYING: usize = 16;
const MAX_PAYING_PER_PAYER: usize = 2;
/// How long a finished job's result can be fetched, and its record is kept.
pub const RESULT_RETENTION_MS: u64 = 7 * 24 * 3600 * 1000;
/// How long a payment given up as unpaid is still watched in case it lands.
const UNPAID_WATCH_MS: u64 = 24 * 3600 * 1000;
/// How long past its coordinator deadline a job may stay unresolved before
/// the customer is refunded.
const RUN_GRACE_MS: u64 = 24 * 3600 * 1000;
/// The largest result stored for a job.
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
/// How often each kind of pending work is retried.
const RUN_RETRY: Duration = Duration::from_secs(30);
const REFUND_POLL: Duration = Duration::from_secs(15);
const REFUND_REBUILD: Duration = Duration::from_secs(120);
const UNPAID_POLL: Duration = Duration::from_secs(300);
/// How long a paid job may wait to start before it is refunded.
const PAID_START_LIMIT_MS: u64 = 3600 * 1000;
/// Refunds in a day before new sales pause. Each refund leaves a small
/// marker account on chain, so a stream of failing jobs cannot drain the
/// fee payer.
const MAX_REFUNDS_PER_DAY: usize = 20;
/// How long a check of what is on sale is reused.
const ON_SALE_CACHE: Duration = Duration::from_secs(10);

/// A kind of job sold at `/v1/jobs/<kind>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A sourced research report written by IMD's agent swarm.
    Research,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Research => "research",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "research" => Some(Kind::Research),
            _ => None,
        }
    }
    fn description(self) -> &'static str {
        match self {
            Kind::Research => "A sourced research report on a question you set, written by IMD's agent swarm and delivered with a receipt signed by the operator who bought it. Pay once; the status URL is free and returns the report.",
        }
    }
    fn tags(self) -> Value {
        match self {
            Kind::Research => json!(["research", "report", "agents", "receipts"]),
        }
    }
    /// The checked request, or why it is refused. Nothing is quoted for a
    /// request that would be refused after payment.
    pub fn validate(self, request: &Value) -> Result<()> {
        match self {
            Kind::Research => {
                let fields = request
                    .as_object()
                    .ok_or_else(|| GatewayError::Invalid("request must be a JSON object".into()))?;
                if fields.keys().any(|key| key != "objective") {
                    return Err(GatewayError::Invalid(
                        "a research request takes only \"objective\"".into(),
                    ));
                }
                let objective = fields
                    .get("objective")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::Invalid("\"objective\" is required".into()))?;
                let length = objective.trim().chars().count();
                if !(20..=4000).contains(&length)
                    || objective
                        .chars()
                        .any(|c| c.is_control() && !c.is_whitespace())
                {
                    return Err(GatewayError::Invalid(
                        "\"objective\" must be 20 to 4000 characters of text".into(),
                    ));
                }
                research_order(request)
                    .validate()
                    .map_err(|e| GatewayError::Invalid(e.to_string()))
            }
        }
    }
    fn example(self) -> Value {
        match self {
            Kind::Research => {
                json!({"objective":"Write a sourced report on how AI agents are given spending authority today: session keys, spending limits and allowlists. For each, explain what it protects against and where it fails."})
            }
        }
    }
    fn input_schema(self) -> Value {
        match self {
            Kind::Research => {
                json!({"type":"object","additionalProperties":false,"required":["objective"],
                "properties":{"objective":{"type":"string","minLength":20,"maxLength":4000,
                    "description":"What the report should answer. IMD's swarm researches it and writes a sourced Markdown report."}}})
            }
        }
    }
}

/// The IMD order a research request becomes.
fn research_order(request: &Value) -> SwarmOrder {
    SwarmOrder::new(
        "job.open",
        json!({"objective":request["objective"].as_str().unwrap_or_default().trim(),
            "skill":"research-report",
            "outputs":[{"name":"report","path":"artifacts/report.md","mediaType":"text/markdown"}],
            "github":false}),
    )
}

/// What one kind of job costs and how it is bought on the coordinator.
#[derive(Clone, Debug)]
pub struct Offer {
    pub kind: Kind,
    /// What the customer pays, in USDC base units.
    pub price_atomic: u64,
    /// What the gateway offers the operator. Never more than the price.
    pub offer_micro_usdc: u64,
    pub deadline_ms: u64,
    /// Operators whose results may be delivered. Any other result is refunded.
    pub approved_operators: Vec<String>,
    /// Paid jobs of this kind in progress at once; quotes stop beyond it.
    pub max_running: usize,
}

/// The parts of an offer a paid job keeps, so it finishes on the terms it
/// was sold on even if the offer changes or stops.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Purchase {
    pub offer_micro_usdc: u64,
    pub deadline_ms: u64,
    pub approved_operators: Vec<String>,
}

/// Payment, settlement and refunds for paid jobs.
#[async_trait]
pub trait JobsRail: Send + Sync {
    fn network(&self) -> &str;
    fn asset(&self) -> &str;
    fn pay_to(&self) -> &str;
    fn fee_payer(&self) -> &str;
    async fn check_price(&self, resource_url: &str, amount: u64, now: u64) -> Result<()>;
    fn authorization_id(&self, header: &str) -> Result<String>;
    async fn verify(&self, header: &str, terms: &PaymentTerms) -> Result<VerifiedPayment>;
    async fn settle(
        &self,
        payment: &VerifiedPayment,
        terms: &PaymentTerms,
    ) -> Result<SettlementStatus>;
    async fn refund(&self, payment: &VerifiedPayment, terms: &PaymentTerms) -> Result<RefundBuild>;
    async fn refund_status(
        &self,
        payment: &VerifiedPayment,
        terms: &PaymentTerms,
        refund: &str,
    ) -> Result<RefundStatus>;
}

#[async_trait]
impl JobsRail for crate::bridge::Bridge {
    fn network(&self) -> &str {
        crate::PaymentRail::network(self)
    }
    fn asset(&self) -> &str {
        crate::PaymentRail::asset(self)
    }
    fn pay_to(&self) -> &str {
        crate::PaymentRail::pay_to(self)
    }
    fn fee_payer(&self) -> &str {
        crate::PaymentRail::fee_payer(self)
    }
    async fn check_price(&self, resource_url: &str, amount: u64, now: u64) -> Result<()> {
        self.check_job_price(resource_url, amount, now).await
    }
    fn authorization_id(&self, header: &str) -> Result<String> {
        crate::PaymentRail::authorization_id(self, header)
    }
    async fn verify(&self, header: &str, terms: &PaymentTerms) -> Result<VerifiedPayment> {
        crate::PaymentRail::verify(self, header, terms).await
    }
    async fn settle(
        &self,
        payment: &VerifiedPayment,
        terms: &PaymentTerms,
    ) -> Result<SettlementStatus> {
        crate::PaymentRail::settle(self, payment, terms).await
    }
    async fn refund(&self, payment: &VerifiedPayment, terms: &PaymentTerms) -> Result<RefundBuild> {
        crate::bridge::Bridge::refund(self, payment, terms).await
    }
    async fn refund_status(
        &self,
        payment: &VerifiedPayment,
        terms: &PaymentTerms,
        refund: &str,
    ) -> Result<RefundStatus> {
        crate::bridge::Bridge::refund_status(self, payment, terms, refund).await
    }
}

/// A verified result, ready to show the customer.
#[derive(Debug)]
pub struct RunOutput {
    pub job_id: Uuid,
    pub operator: String,
    pub result: Value,
}

/// Why a run did not produce a result.
#[derive(Debug)]
pub enum RunFailure {
    /// The job ended without a usable result: the coordinator refused or
    /// refunded it, or it could not be delivered. The customer is refunded.
    Refund(String),
    /// The outcome is not known yet (the coordinator was unreachable, or the
    /// receipt is late). The same signed job is tried again later.
    Retry(String),
}

/// Buys a paid job's work on the Compute coordinator.
#[async_trait]
pub trait JobRunner: Send + Sync {
    /// Whether only approved operators can win this job right now: one
    /// serves it, no generic operator could take it, and no more operators
    /// serve it than are approved.
    async fn available(
        &self,
        kind: Kind,
        offer_micro_usdc: u64,
        approved: &[String],
    ) -> Result<bool>;
    /// Signs the coordinator job for a paid request without submitting it.
    fn sign(&self, kind: Kind, purchase: &Purchase, request: &Value) -> Result<SignedJobEnvelope>;
    /// Submits the signed job (repeating it is safe) and waits for its
    /// verified result.
    async fn run(&self, envelope: &SignedJobEnvelope)
        -> std::result::Result<RunOutput, RunFailure>;
}

/// The coordinator buyer that paid jobs are bought with. It has its own
/// identity and balance, apart from the one that buys inference.
pub struct CoordinatorRunner {
    pub config: BuyerConfig,
    pub identity: Arc<LocalIdentity>,
    pub http: reqwest::Client,
}

#[async_trait]
impl JobRunner for CoordinatorRunner {
    async fn available(
        &self,
        kind: Kind,
        offer_micro_usdc: u64,
        approved: &[String],
    ) -> Result<bool> {
        let model = match kind {
            Kind::Research => SWARM_MODEL,
        };
        let view = covenant_compute_buyer::capacity(&self.http, &self.config)
            .await
            .map_err(|e| GatewayError::Unavailable(e.to_string()))?;
        let mut served = false;
        for row in view.entries.iter().filter(|row| {
            row.kind == JobKind::BatchJob
                && row.operators > 0
                && row.min_ask_micro_usdc <= offer_micro_usdc
        }) {
            // The coordinator lets a generic batch node take any model, so
            // while one could win this job, it is not sold.
            if row.model == "any" {
                return Ok(false);
            }
            // Capacity counts operators without naming them; more than are
            // approved means someone else advertises this job.
            if row.model == model {
                if row.operators > approved.len() {
                    return Ok(false);
                }
                served = true;
            }
        }
        Ok(served)
    }
    fn sign(&self, kind: Kind, purchase: &Purchase, request: &Value) -> Result<SignedJobEnvelope> {
        let job = match kind {
            Kind::Research => JobRequest {
                kind: JobKind::BatchJob,
                input: swarm_input(&research_order(request)),
                model: Some(SWARM_MODEL.into()),
                gpu_class: None,
                min_vram_gb: None,
                min_reputation_bps: None,
                price_micro_usdc: purchase.offer_micro_usdc,
                deadline_ms: purchase.deadline_ms,
            },
        };
        covenant_compute_buyer::sign_envelope(&self.config, &self.identity, job)
            .map_err(|e| GatewayError::Invalid(e.to_string()))
    }
    async fn run(
        &self,
        envelope: &SignedJobEnvelope,
    ) -> std::result::Result<RunOutput, RunFailure> {
        let outcome = covenant_compute_buyer::dispatch_signed_prepaid(
            &self.http,
            &self.config,
            &self.identity,
            envelope.clone(),
        )
        .await
        .map_err(|error| match &error {
            // The coordinator refunded this buyer: the job was not served.
            BuyerError::NotServed { .. } => RunFailure::Refund(error.to_string()),
            // The coordinator's verdict on the job itself: malformed, not
            // fundable, not allowed or conflicting. It never landed.
            BuyerError::SubmitRefused {
                status: 400 | 402 | 403 | 409 | 422,
                ..
            } => RunFailure::Refund(error.to_string()),
            // The coordinator charged for a result that does not verify.
            BuyerError::Verification(_) => RunFailure::Refund(error.to_string()),
            // Anything else, a redirect or an intermediary's refusal
            // included, may have landed; the same job is submitted again.
            _ => RunFailure::Retry(error.to_string()),
        })?;
        Ok(RunOutput {
            job_id: outcome.envelope.payload.job_id,
            operator: outcome.receipt.signer_pubkey_b58.clone(),
            result: json!({"envelope":outcome.envelope,"output":outcome.output,
                "output_json":serde_json::to_string(&outcome.output).unwrap_or_default(),
                "receipt":outcome.receipt}),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct StoredRefund {
    transaction_id: String,
    encoded: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum State {
    /// Verified and stored; the payment may be in flight.
    Paying { payment: VerifiedPayment },
    /// The payment finalized. Nothing has been sent to the coordinator.
    Paid {
        payment: VerifiedPayment,
        transaction_id: String,
    },
    /// The signed coordinator job, stored before it is first submitted.
    Running {
        payment: VerifiedPayment,
        transaction_id: String,
        envelope: Box<SignedJobEnvelope>,
        started_at_ms: u64,
    },
    /// The result is in the job's result file until the retention ends.
    Completed {
        payer: String,
        transaction_id: String,
        completed_at_ms: u64,
    },
    /// The payment is owed back in full.
    Refunding {
        payment: VerifiedPayment,
        transaction_id: String,
        reason: String,
        refund: Option<StoredRefund>,
    },
    Refunded {
        payer: String,
        transaction_id: String,
        reason: String,
        refund_transaction_id: String,
    },
    /// The payment was judged unable to land, so nothing was charged and
    /// nothing ran. It is watched for a day in case it lands after all.
    Unpaid {
        payment: Option<VerifiedPayment>,
        since_ms: u64,
    },
}
impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Paying { .. } => "paying",
            State::Paid { .. } | State::Running { .. } => "running",
            State::Completed { .. } => "completed",
            State::Refunding { .. } => "refunding",
            State::Refunded { .. } => "refunded",
            State::Unpaid { .. } => "unpaid",
        }
    }
    /// Paid for and owed work or money.
    fn open(&self) -> bool {
        matches!(
            self,
            State::Paying { .. }
                | State::Paid { .. }
                | State::Running { .. }
                | State::Refunding { .. }
        ) || matches!(
            self,
            State::Unpaid {
                payment: Some(_),
                ..
            }
        )
    }
    fn working(&self) -> bool {
        matches!(self, State::Paid { .. } | State::Running { .. })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
    id: Uuid,
    kind: Kind,
    /// The customer's request. Cleared once the job ends.
    request: Option<Value>,
    request_sha256: String,
    /// Kept after the job ends so a reused payment is still recognized.
    authorization_id: String,
    payer: String,
    purchase: Purchase,
    terms: PaymentTerms,
    created_at_ms: u64,
    updated_at_ms: u64,
    state: State,
}

struct Quote {
    kind: Kind,
    request: Value,
    terms: PaymentTerms,
}

/// One file per job, each replaced atomically and fsynced with its directory.
/// A finished job's result is a file of its own, read outside the lock.
struct Store {
    directory: PathBuf,
    _lock: File,
    records: BTreeMap<Uuid, Record>,
}
impl Store {
    fn open(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = private_file(&directory.join("lock"), false)?;
        lock.try_lock()
            .map_err(|_| GatewayError::Unavailable("job store already open".into()))?;
        Ok(Self {
            directory: directory.into(),
            _lock: lock,
            records: read_records(directory)?,
        })
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.as_file_mut().write_all(bytes)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join(name))
            .map_err(|e| e.error)?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
    fn save(&mut self, record: Record) -> Result<()> {
        self.write(
            &format!("{}.json", record.id),
            &serde_json::to_vec(&record)?,
        )?;
        self.records.insert(record.id, record);
        Ok(())
    }
    fn result_path(&self, id: Uuid) -> PathBuf {
        self.directory.join(format!("{id}.result"))
    }
    fn remove(&mut self, id: Uuid) -> Result<()> {
        for path in [
            self.directory.join(format!("{id}.json")),
            self.result_path(id),
        ] {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        File::open(&self.directory)?.sync_all()?;
        self.records.remove(&id);
        Ok(())
    }
    fn working(&self, kind: Kind) -> usize {
        self.records
            .values()
            .filter(|r| r.kind == kind && r.state.working())
            .count()
    }
}

fn private_file(path: &Path, truncate: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// Every job record in a store directory. A record that does not read back
/// may hold a payment, so it stops the gateway rather than being skipped.
fn read_records(directory: &Path) -> Result<BTreeMap<Uuid, Record>> {
    let mut records = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|stem| Uuid::parse_str(stem).ok())
        else {
            continue;
        };
        let record: Record = serde_json::from_slice(&std::fs::read(&path)?)?;
        if record.id != id {
            return Err(GatewayError::Unavailable(format!(
                "job record {id} names another job"
            )));
        }
        records.insert(id, record);
    }
    Ok(records)
}

/// Fails if a job store holds paid work or money owed. Jobs can then only
/// be turned off by finishing them, never by dropping their configuration.
pub fn require_no_open_jobs(directory: &Path) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    if let Some(record) = read_records(directory)?.values().find(|r| r.state.open()) {
        return Err(GatewayError::Unavailable(format!(
            "job {} is still open; configure the jobs merchant to finish it",
            record.id
        )));
    }
    Ok(())
}

/// The status URL's token for a job: only the payer is given it.
fn status_token(key: &[u8; 32], id: Uuid) -> String {
    let mut hash = Sha256::new();
    hash.update(key);
    hash.update(b"covenant.job.status\0");
    hash.update(id.as_bytes());
    hash.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The key status tokens are derived from, created once per store.
fn status_key(directory: &Path) -> Result<[u8; 32]> {
    let path = directory.join("status.key");
    if let Ok(bytes) = std::fs::read(&path) {
        return bytes
            .try_into()
            .map_err(|_| GatewayError::Unavailable("job status key is damaged".into()));
    }
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    let mut file = private_file(&path, true)?;
    file.write_all(&key)?;
    file.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(key)
}

/// What a request to a job endpoint gets back.
#[derive(Debug)]
pub enum JobReply {
    /// A fresh quote: the 402 body.
    PaymentRequired(Value),
    /// The job's status, and the settled payment once there is one.
    Status {
        view: Value,
        settlement: Option<Value>,
    },
}

pub struct Jobs {
    rail: Arc<dyn JobsRail>,
    runner: Arc<dyn JobRunner>,
    offers: BTreeMap<Kind, Offer>,
    /// `https://host` the job endpoints are served from.
    origin: String,
    status_key: [u8; 32],
    store: Mutex<Store>,
    quotes: Mutex<BTreeMap<Uuid, (Quote, u64)>>,
    quote_checks: tokio::sync::Semaphore,
    /// Jobs with a run in progress in this process.
    active: Mutex<BTreeSet<Uuid>>,
    /// When each job's next attempt at pending work is due.
    due: Mutex<BTreeMap<Uuid, Instant>>,
    on_sale: Mutex<Option<(Instant, Vec<Kind>)>>,
    settlement_wait: Duration,
}

impl Jobs {
    /// Opens the job store. Every open job must be on the configured rail:
    /// one paid to another merchant, or sponsored by another fee payer,
    /// could never settle or be refunded here.
    pub fn open(
        directory: &Path,
        origin: &str,
        rail: Arc<dyn JobsRail>,
        runner: Arc<dyn JobRunner>,
        offers: Vec<Offer>,
    ) -> Result<Self> {
        let parsed = reqwest::Url::parse(origin)
            .map_err(|_| GatewayError::Invalid("invalid job origin".into()))?;
        if parsed.scheme() != "https" || parsed.path() != "/" || parsed.query().is_some() {
            return Err(GatewayError::Invalid(
                "job origin must be https://host".into(),
            ));
        }
        let mut by_kind = BTreeMap::new();
        for offer in offers {
            if offer.price_atomic == 0
                || offer.offer_micro_usdc == 0
                || offer.offer_micro_usdc > offer.price_atomic
                || offer.max_running == 0
                || offer.approved_operators.is_empty()
                || by_kind.insert(offer.kind, offer).is_some()
            {
                return Err(GatewayError::Invalid("invalid job offer".into()));
            }
        }
        let store = Store::open(directory)?;
        for record in store.records.values().filter(|r| r.state.open()) {
            let terms = &record.terms;
            if terms.network != rail.network()
                || terms.asset != rail.asset()
                || terms.pay_to != rail.pay_to()
                || terms.fee_payer != rail.fee_payer()
            {
                return Err(GatewayError::Unavailable(format!(
                    "job {} has an open payment on another payment rail",
                    record.id
                )));
            }
        }
        Ok(Self {
            rail,
            runner,
            offers: by_kind,
            origin: parsed.as_str().trim_end_matches('/').into(),
            status_key: status_key(directory)?,
            store: Mutex::new(store),
            quotes: Mutex::new(BTreeMap::new()),
            quote_checks: tokio::sync::Semaphore::new(4),
            active: Mutex::new(BTreeSet::new()),
            due: Mutex::new(BTreeMap::new()),
            on_sale: Mutex::new(None),
            settlement_wait: Duration::ZERO,
        })
    }
    /// How long a paid request waits for its payment to finalize before
    /// answering with the job's status.
    pub fn with_settlement_wait(mut self, wait: Duration) -> Self {
        self.settlement_wait = wait;
        self
    }
    pub fn offers(&self) -> impl Iterator<Item = &Offer> {
        self.offers.values()
    }
    pub fn resource_url(&self, kind: Kind) -> String {
        format!("{}/v1/jobs/{}", self.origin, kind.name())
    }
    fn status_url(&self, id: Uuid) -> String {
        format!(
            "{}/v1/jobs/{id}?token={}",
            self.origin,
            status_token(&self.status_key, id)
        )
    }

    /// Whether a job can be sold right now: the payment verifier admits its
    /// price, and only operators that are meant to run it could win it.
    async fn sellable(&self, offer: &Offer, now: u64) -> Result<()> {
        let url = self.resource_url(offer.kind);
        let ready = async {
            self.rail.check_price(&url, offer.price_atomic, now).await?;
            if !self
                .runner
                .available(
                    offer.kind,
                    offer.offer_micro_usdc,
                    &offer.approved_operators,
                )
                .await?
            {
                return Err(GatewayError::Unavailable(format!(
                    "no {} operator is available right now",
                    offer.kind.name()
                )));
            }
            Ok(())
        };
        tokio::time::timeout(Duration::from_secs(5), ready)
            .await
            .map_err(|_| GatewayError::Unavailable("job readiness timed out".into()))?
    }

    /// The offers on sale right now. Discovery reads are frequent and
    /// anonymous, so the answer is reused for a few seconds.
    async fn available(&self, now: u64) -> Vec<&Offer> {
        let mut cache = self.on_sale.lock().await;
        if let Some((at, kinds)) = cache.as_ref() {
            if at.elapsed() < ON_SALE_CACHE {
                return self
                    .offers
                    .values()
                    .filter(|o| kinds.contains(&o.kind))
                    .collect();
            }
        }
        let mut available = Vec::new();
        for offer in self.offers.values() {
            if self.sellable(offer, now).await.is_ok() {
                available.push(offer);
            }
        }
        *cache = Some((Instant::now(), available.iter().map(|o| o.kind).collect()));
        available
    }

    /// Discovery manifest entries for the jobs on sale now.
    pub async fn manifest_endpoints(&self, now: u64) -> Vec<Value> {
        let payment = json!({"scheme":"exact","network":self.rail.network(),"asset":self.rail.asset(),
            "assetDecimals":6,"payTo":self.rail.pay_to(),"feePayer":self.rail.fee_payer()});
        self.available(now)
            .await
            .into_iter()
            .map(|offer| {
                json!({"path":format!("/v1/jobs/{}", offer.kind.name()),"url":self.resource_url(offer.kind),
                    "method":"POST","description":offer.kind.description(),"payment":payment,
                    "price":{"currency":"USDC","unit":"micro_usdc","billing":"fixed_job",
                        "price_micro_usdc":offer.price_atomic},
                    "inputSchema":offer.kind.input_schema(),
                    "statusPath":"/v1/jobs/{id}","statusMethod":"GET",
                    "paymentRequiredHeader":"Payment-Required","paymentSignatureHeader":"Payment-Signature"})
            })
            .collect()
    }

    /// OpenAPI paths for the jobs on sale now, and the free status read.
    pub async fn openapi_paths(&self, now: u64) -> Vec<(String, Value)> {
        let available = self.available(now).await;
        if available.is_empty() {
            return Vec::new();
        }
        let status_schema = json!({"type":"object","required":["id","object","kind","status","status_url"],
            "properties":{"id":{"type":"string","format":"uuid"},"object":{"const":"covenant.job"},
                "kind":{"type":"string"},
                "status":{"enum":["paying","running","completed","refunding","refunded","unpaid"]},
                "status_url":{"type":"string","format":"uri"},
                "result":{"type":"object","description":"Present once completed: the deliverable and the operator's signed receipt."},
                "refund":{"type":"object","description":"Present once refunded: the refund transaction and amount."},
                "message":{"type":"string"}}});
        let mut paths: Vec<(String, Value)> = available
            .into_iter()
            .map(|offer| {
                let kind = offer.kind;
                (format!("/v1/jobs/{}", kind.name()), json!({"post":{
                    "operationId":format!("create{}Job", match kind { Kind::Research => "Research" }),
                    "summary":kind.description(),
                    "description":"Submit the request without Payment-Signature to get an HTTP 402 quote for the fixed price. Submit the identical body with the x402 payment to start the job. The payment settles before any work starts. The response is the job's status; GET its status_url, which is free, until the status is completed. A job that does not complete is refunded in full to the paying address.",
                    "security":[],
                    "x-payment-info":{"protocols":["x402"],"price":{"mode":"fixed","currency":"USD",
                        "amount":format!("{}.{:06}", offer.price_atomic / 1_000_000, offer.price_atomic % 1_000_000)}},
                    "parameters":[{"name":"Payment-Signature","in":"header","required":false,
                        "description":"Base64 x402 v2 payment for the quote. Omit to get a quote.","schema":{"type":"string"}}],
                    "requestBody":{"required":true,"content":{"application/json":{"schema":kind.input_schema(),"example":kind.example()}}},
                    "responses":{
                        "202":{"description":"The job's status. Payment-Response reports the settled payment once it is final.",
                            "headers":{"Payment-Response":{"description":"Base64 x402 settlement response.","schema":{"type":"string"}}},
                            "content":{"application/json":{"schema":status_schema}}},
                        "400":{"description":"Invalid request body."},
                        "402":{"description":"x402 v2 payment requirements for a fresh quote, or a refused payment.",
                            "headers":{"Payment-Required":{"description":"Base64 x402 v2 payment requirements.","schema":{"type":"string"}}}},
                        "503":{"description":"This kind of job is at capacity or unavailable."}}}}))
            })
            .collect();
        paths.push(("/v1/jobs/{id}".into(), json!({"get":{
            "operationId":"getJob","summary":"A paid job's status and, once completed, its result. Free.",
            "security":[],
            "parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],
            "responses":{"200":{"description":"The job's status.","content":{"application/json":{"schema":status_schema}}},
                "404":{"description":"No such job."}}}})));
        paths
    }

    /// The x402 `bazaar` declaration for a job kind. The example is fixed, so
    /// a customer's request never reaches a facilitator's catalog.
    pub fn bazaar(&self, kind: Kind) -> Value {
        let example = json!({"id":"5b0e8a52-2f7e-4c55-9a2d-6c0b0f3e9a11","object":"covenant.job","kind":kind.name(),
            "status":"running","status_url":format!("{}/v1/jobs/5b0e8a52-2f7e-4c55-9a2d-6c0b0f3e9a11", self.origin)});
        json!({
            "info":{"input":{"type":"http","method":"POST","bodyType":"json","body":kind.example()},
                "output":{"type":"json","example":example}},
            "schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["input"],
                "properties":{
                    "input":{"type":"object","additionalProperties":false,"required":["type","method","bodyType","body"],
                        "properties":{"type":{"type":"string","const":"http"},"method":{"type":"string","enum":["POST"]},
                            "bodyType":{"type":"string","enum":["json"]},"body":kind.input_schema()}},
                    "output":{"type":"object","required":["type"],"properties":{"type":{"type":"string"},
                        "example":{"type":"object","required":["id","status","status_url"]}}}}}
        })
    }

    /// The 402 body for a quote.
    fn challenge(&self, kind: Kind, terms: &PaymentTerms) -> Value {
        let icon = format!("{}{}", self.origin, crate::branding::ICON_PATH);
        json!({"x402Version":2,
            "resource":{"url":terms.resource_url,"description":kind.description(),"mimeType":"application/json",
                "serviceName":"Covenant Compute","tags":kind.tags(),"iconUrl":icon},
            "accepts":[terms.accepted()],
            "extensions":{"bazaar":self.bazaar(kind)}})
    }

    pub async fn handle(
        self: &Arc<Self>,
        kind: Kind,
        request: Value,
        payment_header: Option<&str>,
        now: u64,
    ) -> Result<JobReply> {
        let Some(header) = payment_header else {
            let offer = self.offers.get(&kind).ok_or_else(|| {
                GatewayError::Invalid(format!("{} jobs are not sold here", kind.name()))
            })?;
            return self.quote(offer, request, now).await;
        };
        let quote_id = crate::payment_payload(header)?["accepted"]["extra"]["quoteId"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(|| GatewayError::Payment("invalid payment quote".into()))?;
        // A repeated paid request reports on the job it started.
        let existing = self.store.lock().await.records.get(&quote_id).cloned();
        if let Some(record) = existing {
            if record.kind != kind
                || record.authorization_id != self.rail.authorization_id(header)?
            {
                return Err(GatewayError::Payment("authorization changed".into()));
            }
            return Ok(self.status_reply(&record).await);
        }
        self.pay(kind, request, header, quote_id, now).await
    }

    async fn quote(&self, offer: &Offer, request: Value, now: u64) -> Result<JobReply> {
        offer.kind.validate(&request)?;
        let digest = crate::request_digest(&request)?;
        let _check = self
            .quote_checks
            .try_acquire()
            .map_err(|_| GatewayError::Unavailable("quote admission busy".into()))?;
        {
            let store = self.store.lock().await;
            if store.working(offer.kind) >= offer.max_running {
                return Err(GatewayError::Unavailable(format!(
                    "{} jobs are at capacity; try again in a few minutes",
                    offer.kind.name()
                )));
            }
            if store.records.len() >= MAX_RECORDS {
                return Err(GatewayError::Unavailable("job store is full".into()));
            }
            let day_ago = now.saturating_sub(24 * 3600 * 1000);
            let refunds = store
                .records
                .values()
                .filter(|r| match r.state {
                    State::Refunding { .. } => true,
                    State::Refunded { .. } => r.updated_at_ms >= day_ago,
                    _ => false,
                })
                .count();
            if refunds >= MAX_REFUNDS_PER_DAY {
                return Err(GatewayError::Unavailable(
                    "paid jobs are paused; try again later".into(),
                ));
            }
        }
        if self.quotes.lock().await.len() >= MAX_QUOTES {
            return Err(GatewayError::Unavailable("quote admission busy".into()));
        }
        self.sellable(offer, now).await?;
        let terms = PaymentTerms {
            quote_id: Uuid::new_v4(),
            request_sha256: digest,
            network: self.rail.network().into(),
            asset: self.rail.asset().into(),
            pay_to: self.rail.pay_to().into(),
            amount_atomic: offer.price_atomic,
            asset_decimals: 6,
            expires_at_ms: now.saturating_add(QUOTE_TTL_MS),
            fee_payer: self.rail.fee_payer().into(),
            resource_url: self.resource_url(offer.kind),
            batch_pricing: None,
            memo_binding: MemoBinding::V3,
            token_domain: None,
        };
        let challenge = self.challenge(offer.kind, &terms);
        let mut quotes = self.quotes.lock().await;
        quotes.retain(|_, (_, expires)| *expires > now);
        if quotes.len() >= MAX_QUOTES {
            return Err(GatewayError::Unavailable("quote admission busy".into()));
        }
        let expires = terms.expires_at_ms;
        quotes.insert(
            terms.quote_id,
            (
                Quote {
                    kind: offer.kind,
                    request,
                    terms,
                },
                expires,
            ),
        );
        Ok(JobReply::PaymentRequired(challenge))
    }

    async fn pay(
        self: &Arc<Self>,
        kind: Kind,
        request: Value,
        header: &str,
        quote_id: Uuid,
        now: u64,
    ) -> Result<JobReply> {
        let started = Instant::now();
        let (quoted_kind, terms, quoted_request) = {
            let quotes = self.quotes.lock().await;
            let (quote, expires) = quotes
                .get(&quote_id)
                .ok_or_else(|| GatewayError::Payment("unknown or expired quote".into()))?;
            if now >= *expires {
                return Err(GatewayError::Payment("quote expired".into()));
            }
            (quote.kind, quote.terms.clone(), quote.request.clone())
        };
        if quoted_kind != kind || terms.request_sha256 != crate::request_digest(&request)? {
            return Err(GatewayError::Invalid(
                "quote bound to another request".into(),
            ));
        }
        let offer = self.offers.get(&kind).ok_or_else(|| {
            GatewayError::Invalid(format!("{} jobs are not sold here", kind.name()))
        })?;
        // The resource and extensions a payment echoes are not signed, and a
        // facilitator catalogs them. Send the ones this gateway issued.
        let issued = issued_header(header, &self.challenge(kind, &terms))?;
        let payment = self.rail.verify(&issued, &terms).await?;
        let now = crate::after_wait(now, started);
        {
            let mut store = self.store.lock().await;
            if store
                .records
                .values()
                .any(|r| r.authorization_id == payment.authorization_id)
            {
                return Err(GatewayError::Payment("transaction already assigned".into()));
            }
            if store.records.contains_key(&quote_id) {
                return Err(GatewayError::Payment("quote already paid".into()));
            }
            let paying = store
                .records
                .values()
                .filter(|r| matches!(r.state, State::Paying { .. }));
            let (all, mine) = paying.fold((0, 0), |(all, mine), r| {
                (all + 1, mine + usize::from(r.payer == payment.payer))
            });
            if all >= MAX_PAYING || mine >= MAX_PAYING_PER_PAYER {
                return Err(GatewayError::Unavailable(
                    "too many payments are being confirmed; try again in a minute".into(),
                ));
            }
            if store.working(kind) >= offer.max_running {
                return Err(GatewayError::Unavailable(format!(
                    "{} jobs are at capacity; try again in a few minutes",
                    kind.name()
                )));
            }
            store.save(Record {
                id: quote_id,
                kind,
                request: Some(quoted_request),
                request_sha256: terms.request_sha256.clone(),
                authorization_id: payment.authorization_id.clone(),
                payer: payment.payer.clone(),
                purchase: Purchase {
                    offer_micro_usdc: offer.offer_micro_usdc,
                    deadline_ms: offer.deadline_ms,
                    approved_operators: offer.approved_operators.clone(),
                },
                terms,
                created_at_ms: now,
                updated_at_ms: now,
                state: State::Paying { payment },
            })?;
        }
        self.quotes.lock().await.remove(&quote_id);
        // Hold the request open while the payment finalizes, so a standard
        // client sees the job start in the response to its paid request.
        let deadline = started + self.settlement_wait;
        loop {
            self.advance(quote_id).await;
            let record = self.record(quote_id).await?;
            if matches!(record.state, State::Paid { .. }) {
                self.start(quote_id).await;
            }
            if !matches!(record.state, State::Paying { .. }) || Instant::now() >= deadline {
                if matches!(record.state, State::Unpaid { .. }) {
                    return Err(GatewayError::Payment("payment did not land".into()));
                }
                return Ok(self.status_reply(&record).await);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn record(&self, id: Uuid) -> Result<Record> {
        self.store
            .lock()
            .await
            .records
            .get(&id)
            .cloned()
            .ok_or_else(|| GatewayError::Invalid("unknown job".into()))
    }

    /// Writes `state` if the job's current state is one `from` accepts.
    async fn transition(
        &self,
        id: Uuid,
        from: impl Fn(&State) -> bool,
        state: State,
    ) -> Result<bool> {
        let mut store = self.store.lock().await;
        let Some(record) = store.records.get(&id).cloned() else {
            return Ok(false);
        };
        if !from(&record.state) {
            return Ok(false);
        }
        let ended = !state.open();
        store.save(Record {
            request: if ended { None } else { record.request },
            updated_at_ms: crate::now_ms().max(record.updated_at_ms),
            state,
            ..record
        })?;
        Ok(true)
    }

    /// Whether pending work on this job is due, marking the next attempt.
    async fn due(&self, id: Uuid, every: Duration) -> bool {
        let mut due = self.due.lock().await;
        let now = Instant::now();
        if due.get(&id).is_some_and(|at| now < *at) {
            return false;
        }
        due.insert(id, now + every);
        true
    }

    /// Moves one job forward as far as it can go without waiting on work.
    async fn advance(&self, id: Uuid) {
        let Ok(record) = self.record(id).await else {
            return;
        };
        let result = match &record.state {
            State::Paying { payment } => self.settle(&record, payment).await,
            State::Refunding {
                payment,
                transaction_id,
                reason,
                refund,
            } => {
                self.refund(&record, payment, transaction_id, reason, refund.as_ref())
                    .await
            }
            State::Unpaid {
                payment: Some(payment),
                since_ms,
            } => self.watch_unpaid(&record, payment, *since_ms).await,
            _ => Ok(()),
        };
        if let Err(error) = result {
            eprintln!("job {id}: {error}");
        }
    }

    async fn settle(&self, record: &Record, payment: &VerifiedPayment) -> Result<()> {
        let paying = |s: &State| matches!(s, State::Paying { .. });
        match self.rail.settle(payment, &record.terms).await? {
            SettlementStatus::Finalized { transaction_id } => {
                let paid = State::Paid {
                    payment: payment.clone(),
                    transaction_id,
                };
                self.transition(record.id, paying, paid).await?;
            }
            SettlementStatus::Failed => {
                let unpaid = State::Unpaid {
                    payment: Some(payment.clone()),
                    since_ms: crate::now_ms(),
                };
                self.transition(record.id, paying, unpaid).await?;
            }
            SettlementStatus::Pending => {}
        }
        Ok(())
    }

    /// A payment judged unable to land is checked again for a day. If it
    /// landed after all, the customer was charged for a job that never ran,
    /// so it is refunded.
    async fn watch_unpaid(
        &self,
        record: &Record,
        payment: &VerifiedPayment,
        since_ms: u64,
    ) -> Result<()> {
        let unpaid = |s: &State| {
            matches!(
                s,
                State::Unpaid {
                    payment: Some(_),
                    ..
                }
            )
        };
        if crate::now_ms().saturating_sub(since_ms) > UNPAID_WATCH_MS {
            let settled = State::Unpaid {
                payment: None,
                since_ms,
            };
            self.transition(record.id, unpaid, settled).await?;
            return Ok(());
        }
        if !self.due(record.id, UNPAID_POLL).await {
            return Ok(());
        }
        if let SettlementStatus::Finalized { transaction_id } =
            self.rail.settle(payment, &record.terms).await?
        {
            let refunding = State::Refunding {
                payment: payment.clone(),
                transaction_id,
                reason: "The payment landed after the job had been given up.".into(),
                refund: None,
            };
            if self.transition(record.id, unpaid, refunding).await? {
                self.due.lock().await.remove(&record.id);
            }
        }
        Ok(())
    }

    async fn refund(
        &self,
        record: &Record,
        payment: &VerifiedPayment,
        transaction_id: &str,
        reason: &str,
        refund: Option<&StoredRefund>,
    ) -> Result<()> {
        let refunding = |refund: Option<StoredRefund>| State::Refunding {
            payment: payment.clone(),
            transaction_id: transaction_id.into(),
            reason: reason.into(),
            refund,
        };
        let refunded = |refund_transaction_id: String| State::Refunded {
            payer: payment.payer.clone(),
            transaction_id: transaction_id.into(),
            reason: reason.into(),
            refund_transaction_id,
        };
        let Some(stored) = refund.cloned() else {
            if !self.due(record.id, REFUND_POLL).await {
                return Ok(());
            }
            let unbuilt = |s: &State| matches!(s, State::Refunding { refund: None, .. });
            let next = match self.rail.refund(payment, &record.terms).await? {
                RefundBuild::Built {
                    transaction_id,
                    encoded,
                } => refunding(Some(StoredRefund {
                    transaction_id,
                    encoded,
                })),
                RefundBuild::AlreadyRefunded { transaction_id } => refunded(transaction_id),
            };
            if self.transition(record.id, unbuilt, next).await? {
                // Check the new refund on the next pass, not a poll later.
                self.due.lock().await.remove(&record.id);
            }
            return Ok(());
        };
        if !self.due(record.id, REFUND_POLL).await {
            return Ok(());
        }
        let this = stored.clone();
        let current =
            move |s: &State| matches!(s, State::Refunding { refund: Some(r), .. } if *r == this);
        match self
            .rail
            .refund_status(payment, &record.terms, &stored.encoded)
            .await?
        {
            RefundStatus::Finalized { transaction_id } => {
                self.transition(record.id, current, refunded(transaction_id))
                    .await?;
            }
            RefundStatus::Void => {
                // Build the next one after a pause, so a refund that cannot
                // land (an empty merchant wallet, a closed account) is not
                // rebuilt every few seconds.
                if self.transition(record.id, current, refunding(None)).await? {
                    self.due
                        .lock()
                        .await
                        .insert(record.id, Instant::now() + REFUND_REBUILD);
                }
            }
            RefundStatus::Pending => {}
        }
        Ok(())
    }

    /// Runs a paid job to its end: signs it if it has not been signed, then
    /// submits it and waits for the result.
    async fn run(self: Arc<Self>, id: Uuid) {
        if let Err(error) = self.run_once(id).await {
            eprintln!("job {id}: {error}");
        }
        self.active.lock().await.remove(&id);
    }

    async fn run_once(&self, id: Uuid) -> Result<()> {
        let record = self.record(id).await?;
        let refund =
            |payment: VerifiedPayment, transaction_id: String, reason: String| State::Refunding {
                payment,
                transaction_id,
                reason,
                refund: None,
            };
        let paid = |s: &State| matches!(s, State::Paid { .. });
        let running = |s: &State| matches!(s, State::Running { .. });
        let (payment, transaction_id, envelope, started_at_ms) = match record.state {
            State::Paid {
                payment,
                transaction_id,
            } => {
                // Nothing is spent yet, so a job no approved operator can
                // win is refunded rather than sold to whoever is cheapest.
                // A job that cannot start within the hour is refunded too.
                let waited_too_long =
                    crate::now_ms().saturating_sub(record.updated_at_ms) > PAID_START_LIMIT_MS;
                let capacity = self
                    .offers
                    .get(&record.kind)
                    .map_or(1, |offer| offer.max_running);
                let running_now = {
                    let store = self.store.lock().await;
                    store
                        .records
                        .values()
                        .filter(|r| {
                            r.kind == record.kind && matches!(r.state, State::Running { .. })
                        })
                        .count()
                };
                let ready = if running_now >= capacity {
                    Err(GatewayError::Unavailable("waiting for a free slot".into()))
                } else {
                    self.runner
                        .available(
                            record.kind,
                            record.purchase.offer_micro_usdc,
                            &record.purchase.approved_operators,
                        )
                        .await
                };
                match ready {
                    Ok(true) => {}
                    Ok(false) => {
                        let reason = "No operator that runs this job was available.".to_string();
                        self.transition(id, paid, refund(payment, transaction_id, reason))
                            .await?;
                        return Ok(());
                    }
                    Err(_) if waited_too_long => {
                        let reason = "The job could not start in time.".to_string();
                        self.transition(id, paid, refund(payment, transaction_id, reason))
                            .await?;
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
                let request = record.request.clone().unwrap_or_default();
                let envelope = match self.runner.sign(record.kind, &record.purchase, &request) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        let reason = format!("The job could not be prepared: {error}");
                        self.transition(id, paid, refund(payment, transaction_id, reason))
                            .await?;
                        return Ok(());
                    }
                };
                let started_at_ms = crate::now_ms();
                let signed = State::Running {
                    payment: payment.clone(),
                    transaction_id: transaction_id.clone(),
                    envelope: Box::new(envelope.clone()),
                    started_at_ms,
                };
                if !self.transition(id, paid, signed).await? {
                    return Ok(());
                }
                (payment, transaction_id, envelope, started_at_ms)
            }
            State::Running {
                payment,
                transaction_id,
                envelope,
                started_at_ms,
            } => (payment, transaction_id, *envelope, started_at_ms),
            _ => return Ok(()),
        };
        let state = match self.runner.run(&envelope).await {
            Ok(output)
                if !record
                    .purchase
                    .approved_operators
                    .contains(&output.operator) =>
            {
                refund(
                    payment,
                    transaction_id,
                    "The result came from an operator this gateway does not deliver from.".into(),
                )
            }
            Ok(output) => {
                let result = serde_json::to_vec(&present(record.kind, output))?;
                if result.len() > MAX_RESULT_BYTES {
                    refund(
                        payment,
                        transaction_id,
                        "The result was too large to deliver.".into(),
                    )
                } else {
                    // The result is on disk before the job says it is done.
                    let store = self.store.lock().await;
                    store.write(&format!("{id}.result"), &result)?;
                    drop(store);
                    State::Completed {
                        payer: payment.payer,
                        transaction_id,
                        completed_at_ms: crate::now_ms(),
                    }
                }
            }
            Err(RunFailure::Refund(reason)) => refund(payment, transaction_id, reason),
            Err(RunFailure::Retry(reason)) => {
                // A job that stays unresolved long past its deadline is
                // refunded; the coordinator has refunded or paid it by then.
                let limit = started_at_ms
                    .saturating_add(envelope.payload.deadline_ms)
                    .saturating_add(RUN_GRACE_MS);
                if crate::now_ms() <= limit {
                    return Err(GatewayError::Unavailable(reason));
                }
                refund(
                    payment,
                    transaction_id,
                    "The job did not finish in time.".into(),
                )
            }
        };
        self.transition(id, running, state).await?;
        Ok(())
    }

    /// One pass over every job. Run it every few seconds.
    pub async fn tick(self: &Arc<Self>, now: u64) {
        self.quotes
            .lock()
            .await
            .retain(|_, (_, expires)| *expires > now);
        let jobs: Vec<(Uuid, &'static str, bool, u64)> = self
            .store
            .lock()
            .await
            .records
            .values()
            .map(|r| (r.id, r.state.name(), r.state.open(), r.updated_at_ms))
            .collect();
        for (id, state, open, updated_at_ms) in jobs {
            match state {
                "paying" | "refunding" | "unpaid" if open => self.advance(id).await,
                "running" => self.start(id).await,
                _ if !open && now.saturating_sub(updated_at_ms) > RESULT_RETENTION_MS => {
                    let removed = self.store.lock().await.remove(id);
                    if let Err(error) = removed {
                        eprintln!("job {id}: {error}");
                    }
                    self.due.lock().await.remove(&id);
                }
                _ => {}
            }
        }
    }

    /// Starts a run for a paid job unless one is in progress or waiting out
    /// its retry delay.
    async fn start(self: &Arc<Self>, id: Uuid) {
        if self.active.lock().await.contains(&id) || !self.due(id, RUN_RETRY).await {
            return;
        }
        if self.active.lock().await.insert(id) {
            tokio::spawn(self.clone().run(id));
        }
    }

    async fn status_reply(&self, record: &Record) -> JobReply {
        let settlement = match &record.state {
            State::Paid { transaction_id, .. }
            | State::Running { transaction_id, .. }
            | State::Refunding { transaction_id, .. }
            | State::Completed { transaction_id, .. }
            | State::Refunded { transaction_id, .. } => Some(transaction_id.as_str()),
            _ => None,
        }
        .map(|transaction| json!({"success":true,"transaction":transaction,"network":record.terms.network,"payer":record.payer}));
        JobReply::Status {
            view: self.view(record).await,
            settlement,
        }
    }

    /// The job as its status URL shows it.
    async fn view(&self, record: &Record) -> Value {
        let mut view = json!({"id":record.id,"object":"covenant.job","kind":record.kind.name(),
            "status":record.state.name(),"status_url":self.status_url(record.id),
            "created_at_ms":record.created_at_ms,"updated_at_ms":record.updated_at_ms,
            "price":{"amount":record.terms.amount_atomic.to_string(),"asset":record.terms.asset,
                "network":record.terms.network,"decimals":6}});
        let payment = |transaction: &str| json!({"transaction":transaction,"payer":record.payer});
        match &record.state {
            State::Paying { .. } => {
                view["message"] =
                    json!("The payment is being confirmed. The job starts once it is final.");
            }
            State::Paid { transaction_id, .. } | State::Running { transaction_id, .. } => {
                view["payment"] = payment(transaction_id);
                view["message"] = json!("The job is running. Check this URL again in a minute.");
            }
            State::Completed { transaction_id, .. } => {
                view["payment"] = payment(transaction_id);
                let path = self.store.lock().await.result_path(record.id);
                match tokio::fs::read(path)
                    .await
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                {
                    Some(result) => view["result"] = result,
                    None => view["message"] = json!("The result is no longer kept."),
                }
            }
            State::Refunding {
                transaction_id,
                reason,
                ..
            } => {
                view["payment"] = payment(transaction_id);
                view["message"] = json!(format!(
                    "The job did not complete, so the payment is being returned. {reason}"
                ));
            }
            State::Refunded {
                transaction_id,
                reason,
                refund_transaction_id,
                ..
            } => {
                view["payment"] = payment(transaction_id);
                view["refund"] = json!({"transaction":refund_transaction_id,"amount":record.terms.amount_atomic.to_string()});
                view["message"] = json!(format!(
                    "The job did not complete and the payment was returned. {reason}"
                ));
            }
            State::Unpaid { .. } => {
                view["message"] = json!(
                    "The payment never landed, so nothing was charged and the job did not run."
                );
            }
        }
        view
    }

    /// The job's status for a holder of its status token.
    pub async fn status(&self, id: Uuid, token: &str) -> Option<Value> {
        let expected = status_token(&self.status_key, id);
        let matches = expected.len() == token.len()
            && expected
                .bytes()
                .zip(token.bytes())
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0;
        if !matches {
            return None;
        }
        let record = self.store.lock().await.records.get(&id).cloned()?;
        Some(self.view(&record).await)
    }
}

/// A finished job's result, as the customer reads it.
fn present(kind: Kind, output: RunOutput) -> Value {
    // The same fields an inference answer carries, so a receipt is checked the
    // same way: the signed job, the exact output bytes and the receipt.
    let mut result = json!({"covenant":{"job_id":output.job_id,"operator":output.operator,
        "envelope":output.result["envelope"],"receipt":output.result["receipt"],
        "output":output.result["output"],"output_json":output.result["output_json"]}});
    if kind == Kind::Research {
        let contents: Vec<Content> =
            serde_json::from_value(output.result["output"].clone()).unwrap_or_default();
        let summary = contents.iter().find_map(|block| match block {
            Content::Json { value } if value.get("outcome").is_some() => Some(value),
            _ => None,
        });
        if let Some(summary) = summary {
            let files = summary["outcome"]["files"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if let Some(report) = files
                .iter()
                .find(|file| file["name"] == "report")
                .and_then(|file| file["content"].as_str())
            {
                result["report"] = json!(report);
            }
            result["files"] = json!(files
                .iter()
                .map(|file| json!({"name":file["name"],"path":file["path"],"media_type":file["mediaType"],
                    "bytes":file["bytes"],"sha256":file["hash"],"url":file["url"]}))
                .collect::<Vec<_>>());
            result["imd"] =
                json!({"job_id":summary["swarm"]["jobId"],"explorer":summary["swarm"]["explorer"]});
        }
    }
    result
}

/// The payment header with the resource and extensions this gateway issued.
fn issued_header(header: &str, challenge: &Value) -> Result<String> {
    let mut payload = crate::payment_payload(header)?;
    let fields = payload
        .as_object_mut()
        .ok_or_else(|| GatewayError::Payment("invalid payment payload".into()))?;
    fields.insert("resource".into(), challenge["resource"].clone());
    fields.insert("extensions".into(), challenge["extensions"].clone());
    Ok(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload)?))
}

pub fn spawn_worker(jobs: Arc<Jobs>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(2));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            jobs.tick(crate::now_ms()).await;
        }
    })
}

fn error_response(error: GatewayError) -> Response {
    let status = match &error {
        GatewayError::Invalid(_) => StatusCode::BAD_REQUEST,
        GatewayError::Payment(_) => StatusCode::PAYMENT_REQUIRED,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(json!({"error":error.to_string()}))).into_response()
}

pub(crate) async fn post_http(
    Shared(gateway): Shared<Arc<crate::Gateway>>,
    UrlPath(kind): UrlPath<String>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let (Some(jobs), Some(kind)) = (gateway.jobs.as_ref(), Kind::parse(&kind)) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"no such job kind"})),
        )
            .into_response();
    };
    if !gateway.admissions_open() {
        return error_response(GatewayError::Unavailable("new admissions disabled".into()));
    }
    let payment = headers
        .get("payment-signature")
        .and_then(|value| value.to_str().ok());
    match jobs.handle(kind, request, payment, crate::now_ms()).await {
        Ok(JobReply::PaymentRequired(challenge)) => {
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&challenge).unwrap_or_default());
            let mut response = (StatusCode::PAYMENT_REQUIRED, Json(challenge)).into_response();
            if let Ok(value) = encoded.parse() {
                response.headers_mut().insert("payment-required", value);
            }
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
        Ok(JobReply::Status { view, settlement }) => {
            let mut response = (StatusCode::ACCEPTED, Json(view)).into_response();
            if let Some(encoded) = settlement.and_then(|s| {
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&s).ok()?)
                    .parse()
                    .ok()
            }) {
                response.headers_mut().insert("payment-response", encoded);
            }
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
pub(crate) struct StatusQuery {
    token: Option<String>,
}

/// A job's status, for whoever holds the token its status URL carries.
pub(crate) async fn status_http(
    Shared(gateway): Shared<Arc<crate::Gateway>>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<StatusQuery>,
) -> Response {
    let found = match (gateway.jobs.as_ref(), Uuid::parse_str(&id), query.token) {
        (Some(jobs), Ok(id), Some(token)) => jobs.status(id, &token).await,
        _ => None,
    };
    let mut response = match found {
        Some(view) => Json(view).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error":"no such job"}))).into_response(),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
