//! Durable seller-side exact x402 gateway. The seller fronts compute from a
//! bounded reserve. Only a verified successful result can trigger settlement;
//! only finalized, read-back settlement releases that result to the customer.
#![deny(unsafe_code)]
pub mod batch;
mod branding;
pub mod bridge;
pub mod buyer;
pub mod discovery;
pub mod pricing;

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
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_GATEWAY_JOURNAL_BYTES: usize = 64 * 1024 * 1024;
struct SizeCounter(usize);
impl Write for SizeCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("journal size overflow"))?;
        if self.0 > MAX_GATEWAY_JOURNAL_BYTES {
            return Err(std::io::Error::other("gateway journal size limit exceeded"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn bounded_journal_read(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_GATEWAY_JOURNAL_BYTES as u64 {
        return Err(std::io::Error::other("gateway journal exceeds size limit"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_GATEWAY_JOURNAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_GATEWAY_JOURNAL_BYTES {
        return Err(std::io::Error::other("gateway journal exceeds size limit"));
    }
    Ok(bytes)
}
pub type Result<T> = std::result::Result<T, GatewayError>;

/// Longest a paid request is held open for its payment to finalize (Solana
/// finality is about 13 s). Standard x402 clients never retry a 202, so
/// without this they never see the result.
pub const SETTLEMENT_WAIT: std::time::Duration = std::time::Duration::from_secs(25);
/// How long a completed result stays fetchable with its payment authorization.
const OUTPUT_RETENTION_MS: u64 = 300_000;
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

/// How the payment memo commits to the quote and request. Saved quotes keep
/// the form they were issued with, so a recovered payment is checked against
/// the memo the payer signed.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoBinding {
    /// `covenant.x402.v2:<quote>:<request sha256>`. Its 118 bytes need about
    /// 43k compute units, more than the official x402 client's 20k limit.
    #[default]
    V2,
    /// 32 hex characters (about 13k compute units in total), so any standard
    /// x402 wallet can pay. See [`short_memo`].
    V3,
}

/// First 16 bytes of SHA-256 over `covenant.x402.v3:<quote>:<request sha256>`,
/// as lowercase hex. Anyone holding the quote can recompute it.
pub fn short_memo(quote_id: Uuid, request_sha256: &str) -> String {
    let digest = Sha256::digest(format!("covenant.x402.v3:{quote_id}:{request_sha256}"));
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// EIP-712 domain of an EVM token, as its contract reports `name()` and `version()`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenDomain {
    pub name: String,
    pub version: String,
}

/// The official EVM client signs `validBefore = now + maxTimeoutSeconds`.
/// Five minutes covers the job and its settlement.
pub const EVM_TIMEOUT_SECONDS: u64 = 300;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_pricing: Option<batch::BatchPricingManifest>,
    /// Absent in journals written before v3 memos, which therefore read as V2.
    #[serde(default)]
    pub memo_binding: MemoBinding,
    /// Set for EVM payments (EIP-3009), which carry no memo or fee payer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_domain: Option<TokenDomain>,
}
impl PaymentTerms {
    pub fn memo(&self) -> String {
        match self.memo_binding {
            MemoBinding::V2 => {
                format!("covenant.x402.v2:{}:{}", self.quote_id, self.request_sha256)
            }
            MemoBinding::V3 => short_memo(self.quote_id, &self.request_sha256),
        }
    }
    pub fn accepted(&self) -> Value {
        if let Some(domain) = &self.token_domain {
            return json!({"scheme":"exact","network":self.network,"asset":self.asset,
                "amount":self.amount_atomic.to_string(),"payTo":self.pay_to,"maxTimeoutSeconds":EVM_TIMEOUT_SECONDS,
                "extra":{"name":domain.name,"version":domain.version,"quoteId":self.quote_id,
                "requestSha256":self.request_sha256,"expiresAtMs":self.expires_at_ms}});
        }
        let mut accepted = json!({"scheme":"exact","network":self.network,"asset":self.asset,
            "amount":self.amount_atomic.to_string(),"payTo":self.pay_to,"maxTimeoutSeconds":90,
            "extra":{"feePayer":self.fee_payer,"memo":self.memo(),"quoteId":self.quote_id,
            "requestSha256":self.request_sha256,"expiresAtMs":self.expires_at_ms}});
        if let Some(manifest) = &self.batch_pricing {
            accepted["extra"]["batchPricing"] = json!(manifest);
        }
        accepted
    }
    pub fn requirements(&self) -> Value {
        // serviceName, tags and iconUrl are what Bazaar catalogs show for the seller.
        let mut resource = json!({"url":self.resource_url,
            "description":"Text inference for AI agents, with a receipt signed by the operator who ran it.",
            "mimeType":"application/json","serviceName":"Covenant Compute",
            "tags":["inference","llm","ai","receipts"]});
        if let Ok(mut icon) = reqwest::Url::parse(&self.resource_url) {
            icon.set_path(branding::ICON_PATH);
            icon.set_query(None);
            resource["iconUrl"] = json!(icon.as_str());
        }
        json!({"x402Version":2,"resource":resource,"accepts":[self.accepted()]})
    }
}

/// One quote's payment options: the primary rail's terms, then any other
/// rails offered for the same quote and amount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    pub terms: PaymentTerms,
    pub alternates: Vec<PaymentTerms>,
}
impl Quote {
    pub fn requirements(&self) -> Value {
        let mut required = self.terms.requirements();
        if let Some(accepts) = required["accepts"].as_array_mut() {
            accepts.extend(self.alternates.iter().map(PaymentTerms::accepted));
        }
        required
    }
    /// The 402 body. Batches are not public yet, so only single requests
    /// carry the Bazaar declaration that facilitators catalog.
    pub fn challenge(&self, bazaar: Option<Value>) -> Value {
        let mut required = self.requirements();
        if let (None, Some(bazaar)) = (&self.terms.batch_pricing, bazaar) {
            required["extensions"] = json!({"bazaar":bazaar});
        }
        required
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
    fn token_domain(&self) -> Option<TokenDomain> {
        None
    }
    /// Read-only admission checks. Persisted executions and settlement must
    /// remain independent of current catalog configuration and availability.
    async fn check_offers(&self, _offers: &[discovery::DiscoveryOffer], _now: u64) -> Result<()> {
        Ok(())
    }
    async fn check_price(&self, _amount_atomic: u64, _now: u64) -> Result<()> {
        Ok(())
    }
    async fn check_batch_price(
        &self,
        manifest: &batch::BatchPricingManifest,
        now: u64,
    ) -> Result<()> {
        if self.token_domain().is_some() {
            return Err(GatewayError::Unavailable(
                "batches require the Solana payment rail".into(),
            ));
        }
        self.check_price(manifest.total()?, now).await
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<batch::BatchIdentity>,
    pub buyer: String,
    pub coordinator: String,
    pub job_envelopes: Vec<SignedJobEnvelope>,
    pub maximum_cost_micro_usdc: u64,
}
pub struct ExecutionQuote {
    pub batch_pricing: Option<batch::BatchPricingManifest>,
    pub prepared: PreparedExecution,
    pub amount_atomic: u64,
    pub expires_at_ms: u64,
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
    /// Pure request validation, before any live dependency lookup or signing.
    fn validate_request(&self, _request: &Value) -> Result<()> {
        Ok(())
    }
    /// No network submission here: all child ids must be signed and persisted
    /// together before the first execution, including batch requests.
    fn prepare(&self, request: &Value) -> Result<PreparedExecution>;
    fn quote(&self, request: &Value, legacy_price: u64, now: u64) -> Result<ExecutionQuote> {
        Ok(ExecutionQuote {
            batch_pricing: None,
            prepared: self.prepare(request)?,
            amount_atomic: legacy_price,
            expires_at_ms: now.saturating_add(90_000),
        })
    }
    /// Recover a pre-upgrade quote using the original flat native offer. Never
    /// reinterpret a saved retail authorization through a newly configured rate.
    fn prepare_legacy(&self, request: &Value) -> Result<PreparedExecution> {
        self.prepare(request)
    }
    fn validate_admission(&self, _prepared: &PreparedExecution, _now: u64) -> Result<()> {
        Ok(())
    }
    fn batch_supported(&self) -> bool {
        false
    }
    fn validate_batch_budget(&self, _amount: u64, _now: u64) -> Result<()> {
        Ok(())
    }
    fn discard_progress(&self, _batch_id: Option<Uuid>) -> Result<()> {
        Ok(())
    }
    fn models(&self, _now: u64, _legacy_price: u64) -> Vec<Value> {
        Vec::new()
    }
    async fn discovery_offers(
        &self,
        _now: u64,
        _legacy_price: u64,
    ) -> Result<Vec<discovery::DiscoveryOffer>> {
        Ok(Vec::new())
    }
    async fn check_available(
        &self,
        _request: &Value,
        _now: u64,
        _legacy_price: u64,
        _remaining: u64,
    ) -> Result<()> {
        Ok(())
    }
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
    /// Other rails offered for a quote not yet paid; cleared once one is paid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    alternates: Vec<PaymentTerms>,
    phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quoted_execution: Option<PreparedExecution>,
}
impl Entry {
    fn quote(&self) -> Quote {
        Quote {
            terms: self.terms.clone(),
            alternates: self.alternates.clone(),
        }
    }
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
    fn require_quote_room(&self) -> Result<()> {
        let mut size = SizeCounter(0);
        serde_json::to_writer(&mut size, &self.journal)?;
        if size.0 > MAX_GATEWAY_JOURNAL_BYTES - 16 * 1024 * 1024
            || self.journal.entries.len() >= 100_000
            || self
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
        Ok(())
    }
    fn require_output_room(&self, prepared: &PreparedExecution) -> Result<()> {
        if prepared.operation.is_none() {
            return Ok(());
        }
        let mut size = SizeCounter(0);
        serde_json::to_writer(&mut size, &self.journal)?;
        let pending = self.journal.entries.values().filter(|entry| matches!(&entry.phase, Phase::Executing { prepared, .. } if prepared.operation.is_some())).count();
        if size
            .0
            .saturating_add((pending + 1).saturating_mul(10 * 1024 * 1024))
            > MAX_GATEWAY_JOURNAL_BYTES
        {
            return Err(GatewayError::Unavailable(
                "batch result journal capacity exhausted".into(),
            ));
        }
        Ok(())
    }
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
        let mut journal = match bounded_journal_read(&directory.join("gateway.json")) {
            Ok(bytes) => serde_json::from_slice::<Journal>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Journal {
                version: 6,
                namespace: namespace.clone(),
                watermark_ms: 0,
                reserve_micro_usdc: reserve,
                entries: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        if !matches!(journal.version, 2..=6)
            || journal.namespace != namespace
            || reserve < journal.reserve_micro_usdc
            || (reserve > journal.reserve_micro_usdc && !allow_reserve_increase)
        {
            return Err(GatewayError::Unavailable(
                "journal identity/rail/reserve configuration changed".into(),
            ));
        }
        // v3 freezes tariffs; v4 preserves memo forms; production v5 preserves
        // Base payment options. The unreleased batch candidate also used v5.
        // v6 retains both independent fields and stops either older binary
        // from discarding the other feature during recovery.
        journal.version = 6;
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
        serde_json::to_writer(&mut SizeCounter(0), &self.journal)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(file.as_file_mut(), &self.journal)?;
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
                if now.saturating_sub(*completed_at_ms) > OUTPUT_RETENTION_MS {
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
    alternates: Vec<Arc<dyn PaymentRail>>,
    executor: Arc<dyn Executor>,
    disk: Mutex<Disk>,
    sale_price_atomic: u64,
    admissions_enabled: bool,
    admissions_directory: PathBuf,
    quote_preflights: tokio::sync::Semaphore,
    discovery_reads: tokio::sync::Semaphore,
    settlement_wait: std::time::Duration,
    settlement_waiters: tokio::sync::Semaphore,
}
#[derive(Debug)]
pub enum GatewayReply {
    PaymentRequired(Quote),
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
            alternates: Vec::new(),
            executor,
            sale_price_atomic: sale_price,
            admissions_enabled: true,
            admissions_directory: directory.into(),
            quote_preflights: tokio::sync::Semaphore::new(4),
            discovery_reads: tokio::sync::Semaphore::new(4),
            settlement_wait: std::time::Duration::ZERO,
            settlement_waiters: tokio::sync::Semaphore::new(16),
        })
    }
    pub fn with_admissions_enabled(mut self, enabled: bool) -> Self {
        self.admissions_enabled = enabled;
        self
    }
    /// Offer another rail beside the primary for every new quote, at the same
    /// price, one rail per network. It stays out of the journal namespace, so
    /// adding it keeps existing quotes and payments; see `check_open_payments`
    /// before removing or changing it.
    pub fn with_alternate_rail(mut self, rail: Arc<dyn PaymentRail>) -> Result<Self> {
        if std::iter::once(&self.rail)
            .chain(&self.alternates)
            .any(|existing| existing.network() == rail.network())
        {
            return Err(GatewayError::Invalid(format!(
                "a rail for {} is already configured",
                rail.network()
            )));
        }
        self.alternates.push(rail);
        Ok(self)
    }
    /// The configured rail these terms were quoted on.
    fn rail_for(&self, terms: &PaymentTerms) -> Result<&Arc<dyn PaymentRail>> {
        std::iter::once(&self.rail)
            .chain(&self.alternates)
            .find(|rail| {
                rail.network() == terms.network
                    && rail.asset() == terms.asset
                    && rail.pay_to() == terms.pay_to
                    && rail.fee_payer() == terms.fee_payer
                    && if terms.batch_pricing.is_some() {
                        rail.token_domain().is_none()
                            && batch::resource_url(rail.resource_url())
                                .is_ok_and(|url| url == terms.resource_url)
                    } else {
                        rail.resource_url() == terms.resource_url
                    }
                    && rail.token_domain() == terms.token_domain
            })
            .ok_or_else(|| {
                GatewayError::Unavailable(format!("no payment rail for {}", terms.network))
            })
    }
    /// Fails if a payment in flight, or a paid result its buyer can still
    /// fetch, has no configured rail (Base removed, or its merchant changed).
    /// Without it the payment never settles and the result cannot be fetched.
    pub async fn check_open_payments(&self, now: u64) -> Result<()> {
        let mut disk = self.disk.lock().await;
        disk.advance(now)?;
        for entry in disk.journal.entries.values() {
            let open = match &entry.phase {
                Phase::Executing { .. } | Phase::Settling { .. } => true,
                Phase::Complete { output, .. } => output.is_some(),
                _ => false,
            };
            if open && self.rail_for(&entry.terms).is_err() {
                return Err(GatewayError::Unavailable(format!(
                    "quote {} has an open payment on {} and no matching rail is configured",
                    entry.terms.quote_id, entry.terms.network
                )));
            }
        }
        Ok(())
    }
    /// Holds the paid request that dispatched the job open, polling its
    /// settlement, for up to `wait` from arrival before answering 202. The
    /// journal lock is released between polls.
    pub fn with_settlement_wait(mut self, wait: std::time::Duration) -> Self {
        self.settlement_wait = wait;
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
        let started = std::time::Instant::now();
        self.disk.lock().await.advance(after_wait(now, started))
    }
    pub async fn handle(
        &self,
        request: Value,
        quote_id: Option<Uuid>,
        payment_header: Option<&str>,
        now: u64,
    ) -> Result<GatewayReply> {
        let handling_started = std::time::Instant::now();
        let digest = request_digest(&request)?;
        let mut disk = self.disk.lock().await;
        let now = after_wait(now, handling_started);
        disk.advance(now)?;
        let quote_id = if quote_id.is_some_and(|id| disk.journal.entries.contains_key(&id)) {
            quote_id.unwrap()
        } else {
            self.require_admissions()?;
            if payment_header.is_some() {
                return Err(GatewayError::Payment("unknown or expired quote".into()));
            }
            self.executor.validate_request(&request)?;
            disk.require_quote_room()?;
            let _permit = self
                .quote_preflights
                .try_acquire()
                .map_err(|_| GatewayError::Unavailable("quote admission busy".into()))?;
            let availability_started = std::time::Instant::now();
            let availability_now = now;
            let remaining = disk
                .journal
                .reserve_micro_usdc
                .saturating_sub(disk.used_reserve());
            // Anonymous catalog dependencies must not hold the mutex needed
            // to recover or settle an already authorized purchase.
            drop(disk);
            self.executor
                .check_available(&request, now, self.sale_price_atomic, remaining)
                .await?;
            self.require_admissions()?;
            let now = after_wait(now, availability_started);
            // Validate before producing a payment offer, but never dispatch.
            let quote = self.executor.quote(&request, self.sale_price_atomic, now)?;
            if quote.amount_atomic == 0
                || quote.prepared.job_envelopes.is_empty()
                || quote.prepared.maximum_cost_micro_usdc == 0
                || quote.prepared.maximum_cost_micro_usdc > remaining
                || quote.amount_atomic < quote.prepared.maximum_cost_micro_usdc
                || quote.expires_at_ms <= now
            {
                return Err(GatewayError::Unavailable(
                    "quote cannot cover execution or has expired".into(),
                ));
            }
            let mut offered = Vec::new();
            if let Some(manifest) = &quote.batch_pricing {
                if manifest.total()? != quote.amount_atomic {
                    return Err(GatewayError::Invalid("batch retail total mismatch".into()));
                }
                self.rail.check_batch_price(manifest, now).await?;
            } else {
                self.rail.check_price(quote.amount_atomic, now).await?;
            }
            // Initial batches remain Solana-only. Base single-request offers
            // retain their independently checked availability and exact terms.
            if quote.batch_pricing.is_none() {
                for rail in &self.alternates {
                    if rail.check_price(quote.amount_atomic, now).await.is_ok() {
                        offered.push(rail.clone());
                    }
                }
            }
            disk = self.disk.lock().await;
            let now = after_wait(availability_now, availability_started);
            disk.advance(now)?;
            self.require_admissions()?;
            // A concurrent request may have persisted this id while live
            // checks ran. Never replace its original terms or child envelope.
            if let Some(entry) = quote_id.and_then(|id| disk.journal.entries.get(&id)) {
                if entry.terms.request_sha256 != digest {
                    return Err(GatewayError::Invalid(
                        "quote bound to another request".into(),
                    ));
                }
                return if matches!(entry.phase, Phase::Quoted) {
                    Ok(GatewayReply::PaymentRequired(entry.quote()))
                } else {
                    Err(GatewayError::Payment(
                        "original payment authorization required".into(),
                    ))
                };
            }
            self.executor.validate_admission(&quote.prepared, now)?;
            if quote.expires_at_ms <= now {
                return Err(GatewayError::Unavailable(
                    "quote expired during readiness check".into(),
                ));
            }
            disk.require_quote_room()?;
            if quote.prepared.maximum_cost_micro_usdc
                > disk
                    .journal
                    .reserve_micro_usdc
                    .saturating_sub(disk.used_reserve())
            {
                return Err(GatewayError::Unavailable(
                    "compute reserve changed during quote admission".into(),
                ));
            }
            let terms = PaymentTerms {
                quote_id: quote_id.unwrap_or_else(Uuid::new_v4),
                request_sha256: digest.clone(),
                network: self.rail.network().into(),
                asset: self.rail.asset().into(),
                pay_to: self.rail.pay_to().into(),
                amount_atomic: quote.amount_atomic,
                asset_decimals: self.rail.asset_decimals(),
                expires_at_ms: quote.expires_at_ms.min(now.saturating_add(90_000)),
                fee_payer: self.rail.fee_payer().into(),
                resource_url: if quote.batch_pricing.is_some() {
                    batch::resource_url(self.rail.resource_url())?
                } else {
                    self.rail.resource_url().into()
                },
                batch_pricing: quote.batch_pricing,
                memo_binding: MemoBinding::V3,
                token_domain: self.rail.token_domain(),
            };
            let alternates = offered
                .iter()
                .map(|rail| PaymentTerms {
                    network: rail.network().into(),
                    asset: rail.asset().into(),
                    pay_to: rail.pay_to().into(),
                    asset_decimals: rail.asset_decimals(),
                    fee_payer: rail.fee_payer().into(),
                    resource_url: rail.resource_url().into(),
                    token_domain: rail.token_domain(),
                    ..terms.clone()
                })
                .collect();
            let entry = Entry {
                terms,
                alternates,
                phase: Phase::Quoted,
                quoted_execution: Some(quote.prepared),
            };
            let reply = entry.quote();
            disk.journal.entries.insert(reply.terms.quote_id, entry);
            disk.save()?;
            return Ok(GatewayReply::PaymentRequired(reply));
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
        let mut terms = entry.terms.clone();
        let mut phase = entry.phase.clone();
        if matches!(phase, Phase::Quoted) {
            self.require_admissions()?;
            self.executor.validate_request(&request)?;
            let Some(header) = payment_header else {
                return Ok(GatewayReply::PaymentRequired(entry.quote()));
            };
            if now >= entry.terms.expires_at_ms {
                return Err(GatewayError::Payment("quote expired".into()));
            }
            if let Some(prepared) = &entry.quoted_execution {
                self.executor.validate_admission(prepared, now)?;
            }
            // With several rails offered, the payment names the one it uses,
            // and that rail's terms become the entry's terms from here on.
            if !entry.alternates.is_empty() {
                let network = payment_payload(header)?["accepted"]["network"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_default();
                terms = std::iter::once(&entry.terms)
                    .chain(&entry.alternates)
                    .find(|offer| offer.network == network)
                    .cloned()
                    .ok_or_else(|| GatewayError::Payment("payment network not offered".into()))?;
            }
            let verification_started = std::time::Instant::now();
            let payment = self.rail_for(&terms)?.verify(header, &terms).await?;
            self.require_admissions()?;
            let admission_now = after_wait(now, verification_started);
            if admission_now >= terms.expires_at_ms {
                return Err(GatewayError::Payment(
                    "quote expired during verification".into(),
                ));
            }
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
            let prepared = match entry.quoted_execution {
                Some(prepared) => prepared,
                None => self.executor.prepare_legacy(&request)?,
            };
            self.executor.validate_admission(&prepared, admission_now)?;
            if prepared.job_envelopes.is_empty()
                || prepared.maximum_cost_micro_usdc == 0
                || prepared.maximum_cost_micro_usdc > terms.amount_atomic
                || disk
                    .used_reserve()
                    .checked_add(prepared.maximum_cost_micro_usdc)
                    .is_none_or(|used| used > disk.journal.reserve_micro_usdc)
            {
                return Err(GatewayError::Unavailable(
                    "compute reserve exhausted or quote below execution ceiling".into(),
                ));
            }
            disk.require_output_room(&prepared)?;
            phase = Phase::Executing { payment, prepared };
            let saved = disk.journal.entries.get_mut(&quote_id).unwrap();
            saved.terms = terms.clone();
            saved.alternates.clear();
            saved.phase = phase.clone();
            saved.quoted_execution = None;
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
            let retry_id = self.rail_for(&terms)?.authorization_id(header)?;
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
        let ran_job = matches!(phase, Phase::Executing { .. });
        let reply = self
            .progress(&mut disk, quote_id, terms, phase, now)
            .await?;
        // Only the request that ran the job waits for its payment to finalize,
        // so retries of a stuck payment cannot hold the gateway open.
        let settling = matches!(
            disk.journal.entries.get(&quote_id).map(|e| &e.phase),
            Some(Phase::Settling { .. })
        );
        if !ran_job || !settling || !matches!(reply, GatewayReply::Pending { .. }) {
            return Ok(reply);
        }
        drop(disk);
        self.await_settlement(quote_id, handling_started, now).await
    }
    async fn await_settlement(
        &self,
        quote_id: Uuid,
        started: std::time::Instant,
        now: u64,
    ) -> Result<GatewayReply> {
        let pending = GatewayReply::Pending { quote_id };
        let Ok(_waiter) = self.settlement_waiters.try_acquire() else {
            return Ok(pending);
        };
        let Some(deadline) = started.checked_add(self.settlement_wait) else {
            return Ok(pending);
        };
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(pending);
            }
            tokio::time::sleep(left.min(std::time::Duration::from_secs(1))).await;
            let mut disk = self.disk.lock().await;
            let now = after_wait(now, started).max(disk.journal.watermark_ms);
            disk.advance(now)?;
            let Some(entry) = disk.journal.entries.get(&quote_id).cloned() else {
                return Ok(pending);
            };
            // Recovery may have finished it meanwhile; progress returns that.
            let reply = self
                .progress(&mut disk, quote_id, entry.terms, entry.phase, now)
                .await?;
            if !matches!(reply, GatewayReply::Pending { .. }) {
                return Ok(reply);
            }
        }
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
                        prepared: prepared.clone(),
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
            self.executor.discard_progress(
                terms
                    .batch_pricing
                    .as_ref()
                    .map(|manifest| manifest.batch_id),
            )?;
        }
        if let Phase::Settling {
            payment,
            prepared,
            output,
        } = phase.clone()
        {
            self.executor.discard_progress(
                terms
                    .batch_pricing
                    .as_ref()
                    .map(|manifest| manifest.batch_id),
            )?;
            // The Settling record was fsynced before any broadcast. Unknown
            // sends stay here; the rail reconciles the same signature first.
            // Startup refuses a journal whose open payments lack their rail;
            // nothing is ever settled on another one.
            let Ok(rail) = self.rail_for(&terms) else {
                return Ok(GatewayReply::Pending { quote_id });
            };
            match rail.settle(&payment, &terms).await {
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
        let started = std::time::Instant::now();
        let mut disk = self.disk.lock().await;
        let now = after_wait(now, started);
        disk.advance(now)?;
        for entry in disk.journal.entries.values().filter(|entry| {
            matches!(
                entry.phase,
                Phase::Settling { .. } | Phase::Complete { .. } | Phase::Failed { .. }
            )
        }) {
            self.executor.discard_progress(
                entry
                    .terms
                    .batch_pricing
                    .as_ref()
                    .map(|manifest| manifest.batch_id),
            )?;
        }
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

fn after_wait(now: u64, started: std::time::Instant) -> u64 {
    now.saturating_add(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
}

pub fn request_digest(value: &Value) -> Result<String> {
    // Workspace feature unification can enable serde_json/preserve_order. Sort
    // recursively so canonical request identity does not depend on that feature.
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let ordered: BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), sorted(v))).collect();
                Value::Object(ordered.into_iter().collect())
            }
            Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
            _ => value.clone(),
        }
    }
    let digest = Sha256::digest(serde_json::to_vec(&sorted(value))?);
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
        // A long journal hold must not be followed by a burst of recoveries.
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            let _ = gateway.recover_pending(now_ms()).await;
        }
    })
}
pub fn router(gateway: Arc<Gateway>) -> Router {
    Router::new()
        .merge(branding::router())
        .route("/v1/chat/completions", post(handle_single_http))
        .route("/v1/inference/batches", post(handle_batch_http))
        .route("/.well-known/x402.json", get(discovery::manifest_http))
        .route("/openapi.json", get(discovery::openapi_http))
        .route("/v1/models", get(discovery::models_http))
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
async fn handle_single_http(
    state: State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if request.get("requests").is_some() || request.get("version").is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"use /v1/inference/batches for batches"})),
        )
            .into_response();
    }
    handle_http(state, headers, Json(request)).await
}
async fn handle_batch_http(
    state: State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if !state.0.executor.batch_supported() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"batch execution unavailable"})),
        )
            .into_response();
    }
    if let Err(error) = batch::parse(&request) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    handle_http(state, headers, Json(request)).await
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
    let bazaar = discovery::bazaar(&request);
    match gateway.handle(request, id, payment, now_ms()).await {
        Ok(GatewayReply::PaymentRequired(quote)) => {
            let requirements = quote.challenge(bazaar);
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

#[cfg(test)]
mod batch_tests;
