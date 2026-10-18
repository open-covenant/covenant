//! Durable seller-side exact x402 gateway. The seller fronts compute from a
//! bounded reserve. Only a verified successful result can trigger settlement;
//! only finalized, read-back settlement releases that result to the customer.
#![deny(unsafe_code)]
pub mod bridge;
pub mod buyer;

use async_trait::async_trait;
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use covenant_compute_protocol::SignedJobEnvelope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, GatewayError>;
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("payment rejected: {0}")]
    Payment(String),
    #[error("gateway unavailable: {0}")]
    Unavailable(String),
    #[error("storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaymentTerms {
    pub quote_id: Uuid,
    pub request_sha256: String,
    pub network: String,
    pub asset: String,
    pub pay_to: String,
    pub amount_atomic: u64,
    pub asset_decimals: u8,
    /// Deadline for *new* payment admission, not for reconciliation.
    pub expires_at_ms: u64,
    pub fee_payer: String,
    pub resource_url: String,
}
impl PaymentTerms {
    pub fn memo(&self) -> String {
        format!("covenant.x402.v2:{}:{}", self.quote_id, self.request_sha256)
    }
    pub fn accepted(&self) -> Value {
        json!({"scheme":"exact","network":self.network,"asset":self.asset,
            "amount":self.amount_atomic.to_string(),"payTo":self.pay_to,"maxTimeoutSeconds":90,
            "extra":{"feePayer":self.fee_payer,"memo":self.memo(),"quoteId":self.quote_id,
            "requestSha256":self.request_sha256,"expiresAtMs":self.expires_at_ms}})
    }
    pub fn requirements(&self) -> Value {
        json!({"x402Version":2,"resource":{"url":self.resource_url,
            "description":"Covenant inference with a verified operator receipt","mimeType":"application/json"},
            "accepts":[self.accepted()]})
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedPayment {
    pub payer: String,
    pub authorization_id: String,
    pub transaction_id: String,
    /// A signed transaction, never a private signing key. Stored only on private disk.
    pub encoded_transaction: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettlementStatus {
    Pending,
    Finalized { transaction_id: String },
    Failed,
}

#[async_trait]
pub trait PaymentRail: Send + Sync {
    fn network(&self) -> &str;
    fn asset(&self) -> &str;
    fn pay_to(&self) -> &str;
    fn fee_payer(&self) -> &str {
        ""
    }
    fn resource_url(&self) -> &str {
        "https://localhost/v1/chat/completions"
    }
    fn asset_decimals(&self) -> u8 {
        6
    }
    /// Must cryptographically bind the payer, merchant, asset, amount and
    /// request memo. Simulation is admission only and never a receipt.
    fn authorization_id(&self, encoded_header: &str) -> Result<String>;
    async fn verify(&self, encoded_header: &str, terms: &PaymentTerms) -> Result<VerifiedPayment>;
    /// Idempotently reconcile before broadcasting. Finalized requires chain
    /// readback of the exact authorization and successful execution.
    async fn settle(
        &self,
        payment: &VerifiedPayment,
        terms: &PaymentTerms,
    ) -> Result<SettlementStatus>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedExecution {
    pub buyer: String,
    pub coordinator: String,
    pub job_envelopes: Vec<SignedJobEnvelope>,
    pub maximum_cost_micro_usdc: u64,
}
#[derive(Debug)]
pub struct ExecutionFailure {
    pub message: String,
    /// Only a coordinator refusal or verified unpaid terminal state proves
    /// this. Transport/timeout/verification failure is always uncertain.
    pub definitely_unpaid: bool,
    /// A verified native success that cannot be sold to the customer (for
    /// example an unapproved operator). Keep the full seller reserve cost,
    /// stop this purchase, and never charge the customer or buy a replacement.
    pub terminal_seller_loss: bool,
}
#[async_trait]
pub trait Executor: Send + Sync {
    /// No network submission here: all child ids must be signed and persisted
    /// together before the first execution, including batch requests.
    fn prepare(&self, request: &Value) -> Result<PreparedExecution>;
    async fn execute(
        &self,
        prepared: &PreparedExecution,
    ) -> std::result::Result<Value, ExecutionFailure>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Phase {
    Quoted,
    Executing {
        payment: VerifiedPayment,
        prepared: PreparedExecution,
    },
    Settling {
        payment: VerifiedPayment,
        prepared: PreparedExecution,
        output: Value,
    },
    Complete {
        authorization_id: String,
        payer: String,
        transaction_id: String,
        output: Option<Value>,
        completed_at_ms: u64,
        reserve_cost_micro_usdc: u64,
    },
    /// Never purchase a replacement after a known compute failure or an
    /// expired/failed payment. Keep the request binding as a replay tombstone.
    Failed {
        reason: String,
        reserve_loss_micro_usdc: u64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    terms: PaymentTerms,
    phase: Phase,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    namespace: String,
    watermark_ms: u64,
    reserve_micro_usdc: u64,
    entries: BTreeMap<Uuid, Entry>,
}
struct Disk {
    directory: PathBuf,
    // Stable inode, exclusive for the lifetime of this server. A second
    // process cannot submit jobs against a stale in-memory journal.
    _lock: File,
    journal: Journal,
    healthy: bool,
}
impl Disk {
    fn open(
        directory: &Path,
        namespace: String,
        reserve: u64,
        allow_reserve_increase: bool,
    ) -> Result<Self> {
        if reserve == 0 {
            return Err(GatewayError::Invalid("reserve must be positive".into()));
        }
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(directory.join("lock"))?;
        lock.try_lock()
            .map_err(|_| GatewayError::Unavailable("gateway journal already open".into()))?;
        let mut journal = match std::fs::read(directory.join("gateway.json")) {
            Ok(bytes) => serde_json::from_slice::<Journal>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Journal {
                version: 2,
                namespace: namespace.clone(),
                watermark_ms: 0,
                reserve_micro_usdc: reserve,
                entries: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        if journal.version != 2
            || journal.namespace != namespace
            || reserve < journal.reserve_micro_usdc
            || (reserve > journal.reserve_micro_usdc && !allow_reserve_increase)
        {
            return Err(GatewayError::Unavailable(
                "journal identity/rail/reserve configuration changed".into(),
            ));
        }
        journal.reserve_micro_usdc = reserve;
        let mut disk = Self {
            directory: directory.into(),
            _lock: lock,
            journal,
            healthy: true,
        };
        disk.save()?;
        Ok(disk)
    }
    fn save(&mut self) -> Result<()> {
        if !self.healthy {
            return Err(GatewayError::Unavailable(
                "journal write previously failed; restart for reconciliation".into(),
            ));
        }
        let result = self.save_snapshot();
        if result.is_err() {
            self.healthy = false;
        }
        result
    }
    fn save_snapshot(&self) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&serde_json::to_vec(&self.journal)?)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join("gateway.json"))
            .map_err(|e| e.error)?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
    fn advance(&mut self, now: u64) -> Result<()> {
        if now < self.journal.watermark_ms {
            return Err(GatewayError::Unavailable("clock moved backwards".into()));
        }
        self.journal.watermark_ms = now;
        self.journal
            .entries
            .retain(|_, e| !matches!(e.phase, Phase::Quoted) || e.terms.expires_at_ms >= now);
        for e in self.journal.entries.values_mut() {
            if let Phase::Complete {
                output,
                completed_at_ms,
                ..
            } = &mut e.phase
            {
                if now.saturating_sub(*completed_at_ms) > 300_000 {
                    *output = None;
                }
            }
        }
        self.save()
    }
    fn used_reserve(&self) -> u64 {
        self.journal.entries.values().fold(0u64, |n, e| {
            n.saturating_add(match &e.phase {
                Phase::Executing { prepared, .. } | Phase::Settling { prepared, .. } => {
                    prepared.maximum_cost_micro_usdc
                }
                Phase::Failed {
                    reserve_loss_micro_usdc,
                    ..
                } => *reserve_loss_micro_usdc,
                Phase::Complete {
                    reserve_cost_micro_usdc,
                    ..
                } => *reserve_cost_micro_usdc,
                _ => 0,
            })
        })
    }
}

pub struct Gateway {
    rail: Arc<dyn PaymentRail>,
    executor: Arc<dyn Executor>,
    disk: Mutex<Disk>,
    sale_price_atomic: u64,
    admissions_enabled: bool,
    admissions_directory: PathBuf,
}
#[derive(Debug)]
pub enum GatewayReply {
    PaymentRequired(PaymentTerms),
    Pending {
        quote_id: Uuid,
    },
    Complete {
        output: Value,
        transaction_id: String,
        payer: String,
        network: String,
    },
    Gone,
    Failed(String),
}
impl Gateway {
    /// The caller must use a buyer with its durable daily budget enabled.
    /// `namespace` must include the buyer public key and coordinator endpoint.
    pub fn open(
        directory: &Path,
        namespace: &str,
        reserve: u64,
        sale_price: u64,
        rail: Arc<dyn PaymentRail>,
        executor: Arc<dyn Executor>,
    ) -> Result<Self> {
        Self::open_with_reserve_increase(
            directory, namespace, reserve, sale_price, rail, executor, false,
        )
    }
    /// Increasing a funded lifetime ceiling preserves every reservation and
    /// replay record; operators must explicitly opt in after verifying funds.
    pub fn open_with_reserve_increase(
        directory: &Path,
        namespace: &str,
        reserve: u64,
        sale_price: u64,
        rail: Arc<dyn PaymentRail>,
        executor: Arc<dyn Executor>,
        allow_reserve_increase: bool,
    ) -> Result<Self> {
        if sale_price == 0 {
            return Err(GatewayError::Invalid("sale price must be positive".into()));
        }
        let namespace = format!(
            "{namespace}\0{}\0{}\0{}\0{}\0{}\0{sale_price}",
            rail.network(),
            rail.asset(),
            rail.pay_to(),
            rail.fee_payer(),
            rail.resource_url()
        );
        Ok(Self {
            disk: Mutex::new(Disk::open(
                directory,
                namespace,
                reserve,
                allow_reserve_increase,
            )?),
            rail,
            executor,
            sale_price_atomic: sale_price,
            admissions_enabled: true,
            admissions_directory: directory.into(),
        })
    }
    pub fn with_admissions_enabled(mut self, enabled: bool) -> Self {
        self.admissions_enabled = enabled;
        self
    }
    /// Supervisor latches are checked for each new quote/payment, including
    /// after asynchronous verification. Missing is the only permissive state;
    /// unreadable paths, symlinks and directories all fail closed. Existing
    /// purchases are deliberately independent so settlement can recover.
    pub fn admissions_open(&self) -> bool {
        self.admissions_enabled
            && ["admissions.disabled", "admissions.starting"]
                .iter()
                .all(|name| {
                    matches!(
                        std::fs::symlink_metadata(self.admissions_directory.join(name)),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound
                    )
                })
    }
    fn require_admissions(&self) -> Result<()> {
        if !self.admissions_open() {
            return Err(GatewayError::Unavailable("new admissions disabled".into()));
        }
        Ok(())
    }
    /// Run on a timer even with no traffic. It removes cached plaintext output
    /// after five minutes while preserving permanent replay tombstones.
    pub async fn purge_expired(&self, now: u64) -> Result<()> {
        self.disk.lock().await.advance(now)
    }
    pub async fn handle(
        &self,
        request: Value,
        quote_id: Option<Uuid>,
        payment_header: Option<&str>,
        now: u64,
    ) -> Result<GatewayReply> {
        let digest = request_digest(&request)?;
        let mut disk = self.disk.lock().await;
        disk.advance(now)?;
        let quote_id = if quote_id.is_some_and(|id| disk.journal.entries.contains_key(&id)) {
            quote_id.unwrap()
        } else {
            self.require_admissions()?;
            if payment_header.is_some() {
                return Err(GatewayError::Payment("unknown or expired quote".into()));
            }
            // Validate before producing a payment offer, but never dispatch.
            let _ = self.executor.prepare(&request)?;
            if disk.journal.entries.len() >= 100_000
                || disk
                    .journal
                    .entries
                    .values()
                    .filter(|e| matches!(e.phase, Phase::Quoted))
                    .count()
                    >= 1024
            {
                return Err(GatewayError::Unavailable(
                    "journal admission limit reached".into(),
                ));
            }
            let terms = PaymentTerms {
                quote_id: quote_id.unwrap_or_else(Uuid::new_v4),
                request_sha256: digest.clone(),
                network: self.rail.network().into(),
                asset: self.rail.asset().into(),
                pay_to: self.rail.pay_to().into(),
                amount_atomic: self.sale_price_atomic,
                asset_decimals: self.rail.asset_decimals(),
                expires_at_ms: now.saturating_add(90_000),
                fee_payer: self.rail.fee_payer().into(),
                resource_url: self.rail.resource_url().into(),
            };
            disk.journal.entries.insert(
                terms.quote_id,
                Entry {
                    terms: terms.clone(),
                    phase: Phase::Quoted,
                },
            );
            disk.save()?;
            return Ok(GatewayReply::PaymentRequired(terms));
        };
        let entry = disk
            .journal
            .entries
            .get(&quote_id)
            .cloned()
            .ok_or_else(|| GatewayError::Invalid("unknown or expired quote".into()))?;
        if entry.terms.request_sha256 != digest {
            return Err(GatewayError::Invalid(
                "quote bound to another request".into(),
            ));
        }
        let mut phase = entry.phase;
        if matches!(phase, Phase::Quoted) {
            self.require_admissions()?;
            let Some(header) = payment_header else {
                return Ok(GatewayReply::PaymentRequired(entry.terms));
            };
            if now > entry.terms.expires_at_ms {
                return Err(GatewayError::Payment("quote expired".into()));
            }
            let payment = self.rail.verify(header, &entry.terms).await?;
            self.require_admissions()?;
            // A transaction may only authorize one quote even across alternate
            // request bodies or a restarted process.
            if disk.journal.entries.values().any(|e| match &e.phase {
                Phase::Executing { payment: p, .. } | Phase::Settling { payment: p, .. } => {
                    p.authorization_id == payment.authorization_id
                }
                Phase::Complete {
                    authorization_id, ..
                } => authorization_id == &payment.authorization_id,
                _ => false,
            }) {
                return Err(GatewayError::Payment("transaction already assigned".into()));
            }
            let prepared = self.executor.prepare(&request)?;
            if prepared.job_envelopes.is_empty()
                || prepared.maximum_cost_micro_usdc == 0
                || disk
                    .used_reserve()
                    .checked_add(prepared.maximum_cost_micro_usdc)
                    .map_or(true, |used| used > disk.journal.reserve_micro_usdc)
            {
                return Err(GatewayError::Unavailable(
                    "compute reserve exhausted or quote below execution ceiling".into(),
                ));
            }
            phase = Phase::Executing { payment, prepared };
            disk.journal.entries.get_mut(&quote_id).unwrap().phase = phase.clone();
            disk.save()?; // durable payer + chain + asset + request + stable job ids
        } else if payment_header.is_none() {
            // A quote id is public; resumption and result fetch require the
            // original signed authorization, not knowledge of that id.
            return Err(GatewayError::Payment(
                "original payment authorization required".into(),
            ));
        }
        // Authenticate retries without re-simulation (an already settled tx
        // would fail simulation on duplicate use). The exact header is hashed
        // and bound by the canonical transaction bytes below.
        if let Some(header) = payment_header {
            let retry_id = self.rail.authorization_id(header)?;
            let expected = match &phase {
                Phase::Executing { payment, .. } | Phase::Settling { payment, .. } => {
                    Some(&payment.authorization_id)
                }
                Phase::Complete {
                    authorization_id, ..
                } => Some(authorization_id),
                _ => None,
            };
            if expected.is_some_and(|id| id != &retry_id) {
                return Err(GatewayError::Payment("authorization changed".into()));
            }
        }
        self.progress(&mut disk, quote_id, entry.terms, phase, now)
            .await
    }
    async fn progress(
        &self,
        disk: &mut Disk,
        quote_id: Uuid,
        terms: PaymentTerms,
        mut phase: Phase,
        now: u64,
    ) -> Result<GatewayReply> {
        if let Phase::Executing { payment, prepared } = phase.clone() {
            match self.executor.execute(&prepared).await {
                Ok(mut output) => {
                    if let Some(covenant) =
                        output.get_mut("covenant").and_then(Value::as_object_mut)
                    {
                        covenant.insert("request_sha256".into(), json!(terms.request_sha256));
                        covenant.insert("quote_id".into(), json!(quote_id));
                    }
                    phase = Phase::Settling {
                        payment,
                        prepared,
                        output,
                    };
                }
                Err(failure) if failure.definitely_unpaid => {
                    phase = Phase::Failed {
                        reason: failure.message,
                        reserve_loss_micro_usdc: 0,
                    };
                }
                Err(failure) if failure.terminal_seller_loss => {
                    phase = Phase::Failed {
                        reason: failure.message,
                        reserve_loss_micro_usdc: prepared.maximum_cost_micro_usdc,
                    };
                }
                Err(_) => return Ok(GatewayReply::Pending { quote_id }),
            }
            disk.journal.entries.get_mut(&quote_id).unwrap().phase = phase.clone();
            disk.save()?;
        }
        if let Phase::Settling {
            payment,
            prepared,
            output,
        } = phase.clone()
        {
            // The Settling record was fsynced before any broadcast. Unknown
            // sends stay here; the rail reconciles the same signature first.
            match self.rail.settle(&payment, &terms).await {
                Ok(SettlementStatus::Finalized { transaction_id }) => {
                    phase = Phase::Complete {
                        authorization_id: payment.authorization_id,
                        payer: payment.payer,
                        transaction_id,
                        output: Some(output),
                        completed_at_ms: now,
                        reserve_cost_micro_usdc: prepared.maximum_cost_micro_usdc,
                    }
                }
                Ok(SettlementStatus::Failed) => {
                    phase = Phase::Failed {
                        reason: "payment failed; no output released".into(),
                        reserve_loss_micro_usdc: prepared.maximum_cost_micro_usdc,
                    }
                }
                Ok(SettlementStatus::Pending) | Err(_) => {
                    return Ok(GatewayReply::Pending { quote_id })
                }
            }
            disk.journal.entries.get_mut(&quote_id).unwrap().phase = phase.clone();
            disk.save()?;
        }
        Ok(match phase {
            Phase::Complete {
                output: Some(output),
                transaction_id,
                payer,
                ..
            } => GatewayReply::Complete {
                output,
                transaction_id,
                payer,
                network: terms.network,
            },
            Phase::Complete { output: None, .. } => GatewayReply::Gone,
            Phase::Failed { reason, .. } => GatewayReply::Failed(reason),
            _ => GatewayReply::Pending { quote_id },
        })
    }
    /// Reconcile persisted work even if the HTTP client has disconnected. A
    /// restart uses the same signed jobs and exact signed payment bytes.
    pub async fn recover_pending(&self, now: u64) -> Result<()> {
        let mut disk = self.disk.lock().await;
        disk.advance(now)?;
        let pending = disk
            .journal
            .entries
            .iter()
            .filter(|(_, e)| matches!(e.phase, Phase::Executing { .. } | Phase::Settling { .. }))
            .map(|(id, e)| (*id, e.clone()))
            .collect::<Vec<_>>();
        for (id, entry) in pending {
            let _ = self
                .progress(&mut disk, id, entry.terms, entry.phase, now)
                .await?;
        }
        Ok(())
    }
}

pub fn request_digest(value: &Value) -> Result<String> {
    // serde_json's default map representation is sorted, giving the same
    // digest across whitespace/object-key permutations without altering arrays.
    let digest = Sha256::digest(serde_json::to_vec(value)?);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn spawn_retention(gateway: Arc<Gateway>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            ticks.tick().await;
            let _ = gateway.recover_pending(now_ms()).await;
        }
    })
}
pub fn router(gateway: Arc<Gateway>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(handle_http))
        .route("/health", get(|State(gateway): State<Arc<Gateway>>| async move { Json(json!({"status":"ok", "service":"covenant-compute-x402", "protocol":"x402-v2", "admissions_enabled":gateway.admissions_open(), "revision":option_env!("COVENANT_BUILD_REVISION").unwrap_or("unknown")})) }))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(gateway)
}
fn payment_payload(header: &str) -> Result<Value> {
    if header.len() > 16_384 {
        return Err(GatewayError::Payment("payment header too large".into()));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(header)
        .map_err(|_| GatewayError::Payment("invalid payment header encoding".into()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| GatewayError::Payment("invalid payment payload".into()))
}
async fn handle_http(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let payment = headers
        .get("payment-signature")
        .and_then(|v| v.to_str().ok());
    let id = if let Some(header) = payment {
        match payment_payload(header).ok().and_then(|p| {
            p["accepted"]["extra"]["quoteId"]
                .as_str()
                .and_then(|v| Uuid::parse_str(v).ok())
        }) {
            Some(id) => Some(id),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"invalid payment quote"})),
                )
                    .into_response()
            }
        }
    } else if let Some(id) = headers.get("idempotency-key") {
        match id.to_str().ok().and_then(|v| Uuid::parse_str(v).ok()) {
            Some(id) => Some(id),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"Idempotency-Key must be a UUID"})),
                )
                    .into_response()
            }
        }
    } else {
        None
    };
    match gateway.handle(request, id, payment, now_ms()).await {
        Ok(GatewayReply::PaymentRequired(terms)) => {
            let requirements = terms.requirements();
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&requirements).unwrap());
            let mut response = (StatusCode::PAYMENT_REQUIRED, Json(requirements)).into_response();
            response
                .headers_mut()
                .insert("payment-required", encoded.parse().unwrap());
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
        Ok(GatewayReply::Pending { quote_id }) => (
            StatusCode::ACCEPTED,
            [("retry-after", "2")],
            Json(json!({"status":"pending","quote_id":quote_id})),
        )
            .into_response(),
        Ok(GatewayReply::Complete {
            output,
            transaction_id,
            payer,
            network,
        }) => {
            let settlement = json!({"success":true,"transaction":transaction_id,"network":network,"payer":payer});
            let mut response = Json(output).into_response();
            response.headers_mut().insert(
                "payment-response",
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&settlement).unwrap())
                    .parse()
                    .unwrap(),
            );
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
        Ok(GatewayReply::Gone) => (
            StatusCode::GONE,
            Json(json!({"error":"response expired; payment will not be repeated"})),
        )
            .into_response(),
        Ok(GatewayReply::Failed(reason)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":reason})),
        )
            .into_response(),
        Err(e) => {
            let status = match &e {
                GatewayError::Invalid(_) => StatusCode::BAD_REQUEST,
                GatewayError::Payment(_) => StatusCode::PAYMENT_REQUIRED,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            (status, Json(json!({"error":e.to_string()}))).into_response()
        }
    }
}

#[cfg(test)]
mod tests;
