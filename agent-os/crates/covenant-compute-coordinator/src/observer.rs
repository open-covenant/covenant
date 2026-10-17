//! Read-only, chain-first lease reconciliation. This observer never sends a
//! transaction and never repairs either book. In particular, a failed RPC read
//! is lost visibility, not evidence that an account or its money disappeared.
//!
//! LeaseTerms always lives on L1. A delegated meter is zeroed and owned by the
//! delegation program on L1; its current bytes must be read from the pinned
//! validator's ER. Only a program-owned L1 meter is settlement evidence.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue},
    Extension, Json,
};
use base64::Engine as _;
use covenant_compute_protocol::{parse_lease_terms, JobKind};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{CoordinatorState, JobPhase};

const TERMS_LEN: usize = 223;
const METER_LEN: usize = 130;
const MAX_RPC_BYTES: usize = 16 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 20_000;
const DELEGATION_PROGRAM: &str = "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh";

/// These are public chain identities, never signing keys. Explicit pins stop a
/// misconfigured observer from reporting success after scanning another market.
#[derive(Clone, Debug)]
pub struct ObserverConfig {
    pub genesis_hash: String,
    pub rpc_url: String,
    pub er_rpc_url: String,
    pub program_id: String,
    pub coordinator: String,
    pub renter: String,
    pub mint: String,
    pub validator: String,
    pub interval: Duration,
    pub grace: Duration,
}

impl ObserverConfig {
    pub fn from_env() -> Result<Option<Self>, String> {
        match std::env::var("COVENANT_COMPUTE_OBSERVER") {
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Ok(v) if v == "0" => return Ok(None),
            Ok(v) if v == "1" => {}
            _ => return Err("COVENANT_COMPUTE_OBSERVER must be 0 or 1".into()),
        }
        let required = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{name} is required when the observer is enabled"))
        };
        let seconds = |name: &str, default: u64| -> Result<Duration, String> {
            let n = match std::env::var(name) {
                Ok(value) => value
                    .parse::<u64>()
                    .map_err(|_| format!("{name} must be seconds"))?,
                Err(std::env::VarError::NotPresent) => default,
                Err(_) => return Err(format!("{name} is invalid")),
            };
            if !(1..=86_400).contains(&n) {
                return Err(format!("{name} must be between 1 and 86400 seconds"));
            }
            Ok(Duration::from_secs(n))
        };
        let config = Self {
            genesis_hash: required("COVENANT_COMPUTE_OBSERVER_GENESIS_HASH")?,
            rpc_url: required("COVENANT_COMPUTE_OBSERVER_RPC_URL")?,
            er_rpc_url: required("COVENANT_COMPUTE_OBSERVER_ER_RPC_URL")?,
            program_id: required("COVENANT_COMPUTE_OBSERVER_PROGRAM")?,
            coordinator: required("COVENANT_COMPUTE_OBSERVER_COORDINATOR")?,
            renter: required("COVENANT_COMPUTE_OBSERVER_RENTER")?,
            mint: required("COVENANT_COMPUTE_OBSERVER_MINT")?,
            validator: required("COVENANT_COMPUTE_OBSERVER_VALIDATOR")?,
            interval: seconds("COVENANT_COMPUTE_OBSERVER_INTERVAL_SECS", 60)?,
            grace: seconds("COVENANT_COMPUTE_OBSERVER_GRACE_SECS", 120)?,
        };
        config.validate()?;
        Ok(Some(config))
    }

    pub fn validate(&self) -> Result<(), String> {
        for value in [
            &self.program_id,
            &self.coordinator,
            &self.renter,
            &self.mint,
            &self.validator,
        ] {
            key(value)?;
        }
        if !(20..=64).contains(&self.genesis_hash.len())
            || bs58::decode(&self.genesis_hash).into_vec().is_err()
        {
            return Err("observer genesis hash must be a base58 genesis identity".into());
        }
        for value in [&self.rpc_url, &self.er_rpc_url] {
            let url = reqwest::Url::parse(value).map_err(|_| "observer RPC URL is invalid")?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err("observer RPC URL must use HTTP or HTTPS".into());
            }
        }
        if self.interval.is_zero() || self.grace.is_zero() {
            return Err("observer interval and grace must be positive".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserverStatus {
    Disabled,
    NotObserved,
    Healthy,
    Degraded,
    Blind,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    RpcUnavailable,
    InvalidChainResponse,
    UnreadableAccount,
    UnreadableMeter,
    OrphanedFundedLease,
    OrphanedSettledLease,
    LocalSettlementMissing,
    ExpiredUnsettledLease,
    LocalTerminalChainOpen,
    LocalLiveChainSettled,
    PartialSettlement,
    TermsMismatch,
    SettlementMismatch,
}

impl FindingKind {
    fn blind(self) -> bool {
        matches!(
            self,
            Self::RpcUnavailable
                | Self::InvalidChainResponse
                | Self::UnreadableAccount
                | Self::UnreadableMeter
        )
    }
}

/// Internal detail for logs and operational consumers; public status publishes
/// counts only. Never attach RPC bodies or URLs (which can contain credentials).
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub account: Option<String>,
    pub job_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObserverReport {
    pub schema_version: u32,
    pub observed_at_ms: u64,
    pub scan_complete: bool,
    pub l1_slot: Option<u64>,
    pub scanned_leases: usize,
    pub findings: Vec<Finding>,
}

impl ObserverReport {
    pub fn status(&self) -> ObserverStatus {
        if !self.scan_complete || self.findings.iter().any(|f| f.kind.blind()) {
            ObserverStatus::Blind
        } else if self.findings.is_empty() {
            ObserverStatus::Healthy
        } else {
            ObserverStatus::Degraded
        }
    }

    fn push(&mut self, kind: FindingKind, account: Option<&str>, job_id: Option<Uuid>) {
        self.findings.push(Finding {
            kind,
            account: account.map(str::to_owned),
            job_id,
        });
    }
}

pub struct ChainObserver {
    config: ObserverConfig,
    http: reqwest::Client,
    latest: RwLock<Option<ObserverReport>>,
    alerts: Arc<RwLock<alerts::AlertTracker>>,
    observe_lock: tokio::sync::Mutex<()>,
}

impl ChainObserver {
    pub fn new(config: ObserverConfig) -> Result<Self, String> {
        config.validate()?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "could not build observer RPC client")?;
        Ok(Self {
            config,
            http,
            latest: RwLock::new(None),
            alerts: Arc::new(RwLock::new(alerts::AlertTracker::memory())),
            observe_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn persistent(config: ObserverConfig, path: std::path::PathBuf) -> Result<Self, String> {
        let binding = bs58::encode(Sha256::digest(
            serde_json::to_vec(&[
                &config.genesis_hash,
                &config.program_id,
                &config.coordinator,
                &config.renter,
                &config.mint,
                &config.validator,
            ])
            .expect("string pins serialize"),
        ))
        .into_string();
        let observer = Self::new(config)?;
        *observer.alerts.write() = alerts::AlertTracker::open(path, &binding)?;
        Ok(observer)
    }

    pub fn report(&self) -> Option<ObserverReport> {
        self.latest.read().clone()
    }

    pub fn public_report(&self, now_ms: u64) -> PublicObservation {
        public_observation(self.latest.read().as_ref(), now_ms, self.config.interval)
    }

    async fn rpc(&self, url: &str, method: &str, params: Value) -> Result<Value, FindingKind> {
        let mut response = self
            .http
            .post(url)
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
            .send()
            .await
            .map_err(|_| FindingKind::RpcUnavailable)?;
        if !response.status().is_success() {
            return Err(FindingKind::RpcUnavailable);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RPC_BYTES as u64)
        {
            return Err(FindingKind::InvalidChainResponse);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| FindingKind::RpcUnavailable)?
        {
            if chunk.len() > MAX_RPC_BYTES.saturating_sub(bytes.len()) {
                return Err(FindingKind::InvalidChainResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| FindingKind::InvalidChainResponse)?;
        if value.get("error").is_some() {
            return Err(FindingKind::RpcUnavailable);
        }
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || value.get("id") != Some(&json!(1))
        {
            return Err(FindingKind::InvalidChainResponse);
        }
        value
            .get("result")
            .cloned()
            .ok_or(FindingKind::InvalidChainResponse)
    }

    /// One complete L1 scan discovers accounts even when admission never made a
    /// local row. Live records missing on chain are judged only after that scan
    /// succeeded; a truncated/unreadable scan cannot turn into missing money.
    pub async fn observe(&self, state: &CoordinatorState, now_ms: u64) -> ObserverReport {
        let _serial = self.observe_lock.lock().await;
        let mut report = ObserverReport {
            schema_version: 1,
            observed_at_ms: now_ms,
            scan_complete: false,
            l1_slot: None,
            scanned_leases: 0,
            findings: Vec::new(),
        };
        match tokio::time::timeout(
            Duration::from_secs(120),
            self.scan(state, now_ms, &mut report),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(kind)) => report.push(kind, None, None),
            Err(_) => report.push(FindingKind::RpcUnavailable, None, None),
        }
        let tracker = self.alerts.clone();
        let recorded = report.clone();
        let interval = self.config.interval;
        let _ =
            tokio::task::spawn_blocking(move || tracker.write().record(&recorded, interval)).await;
        *self.latest.write() = Some(report.clone());
        report
    }

    async fn scan(
        &self,
        state: &CoordinatorState,
        now_ms: u64,
        report: &mut ObserverReport,
    ) -> Result<(), FindingKind> {
        // An empty scan of the wrong network or nonexistent program is not a
        // successful reconciliation. Verify these even when there are no leases.
        let genesis = self
            .rpc(&self.config.rpc_url, "getGenesisHash", json!([]))
            .await?;
        if genesis.as_str() != Some(self.config.genesis_hash.as_str()) {
            return Err(FindingKind::InvalidChainResponse);
        }
        let program = self.rpc(&self.config.rpc_url, "getAccountInfo", json!([
            self.config.program_id, {"encoding":"base64", "commitment":"confirmed", "dataSlice":{"offset":0,"length":0}}
        ])).await?;
        if program
            .pointer("/value/executable")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(FindingKind::InvalidChainResponse);
        }
        let result = self.rpc(&self.config.rpc_url, "getProgramAccounts", json!([
            self.config.program_id, {"encoding":"base64", "commitment":"confirmed", "withContext":true,
                "filters":[
                    {"memcmp":{"offset":0,"bytes":bs58::encode(discriminator("LeaseTerms")).into_string()}},
                    {"memcmp":{"offset":24,"bytes":self.config.renter}},
                    {"memcmp":{"offset":88,"bytes":self.config.coordinator}}
                ]}
        ])).await?;
        let slot = result
            .pointer("/context/slot")
            .and_then(Value::as_u64)
            .ok_or(FindingKind::InvalidChainResponse)?;
        let rows = result
            .get("value")
            .and_then(Value::as_array)
            .ok_or(FindingKind::InvalidChainResponse)?;
        if rows.len() > MAX_ACCOUNTS {
            return Err(FindingKind::InvalidChainResponse);
        }
        if self
            .latest
            .read()
            .as_ref()
            .and_then(|r| r.l1_slot)
            .is_some_and(|previous| slot < previous)
        {
            return Err(FindingKind::InvalidChainResponse);
        }
        let block_time = self
            .rpc(&self.config.rpc_url, "getBlockTime", json!([slot]))
            .await?
            .as_u64()
            .ok_or(FindingKind::InvalidChainResponse)?
            .saturating_mul(1000);
        if now_ms.saturating_sub(block_time) > 300_000 || block_time.saturating_sub(now_ms) > 30_000
        {
            return Err(FindingKind::InvalidChainResponse);
        }
        report.l1_slot = Some(slot);
        let mut seen = HashSet::new();
        let mut rollup_verified = false;
        for row in rows {
            let address = row
                .get("pubkey")
                .and_then(Value::as_str)
                .ok_or(FindingKind::InvalidChainResponse)?;
            let initial = match decode_terms_account(address, &row["account"], &self.config) {
                Ok(terms) => terms,
                Err(_) => {
                    report.push(FindingKind::UnreadableAccount, Some(address), None);
                    continue;
                }
            };
            if !seen.insert(initial.job_id) {
                return Err(FindingKind::InvalidChainResponse);
            }
            // Re-read both from one L1 bank: the account scan and meter read can
            // straddle settlement. Never compare an old paid flag to a new meter.
            let fresh = self.rpc(&self.config.rpc_url, "getMultipleAccounts", json!([
                [address, initial.meter_address], {"encoding":"base64", "commitment":"confirmed", "minContextSlot":slot}
            ])).await?;
            let fresh_slot = fresh
                .pointer("/context/slot")
                .and_then(Value::as_u64)
                .ok_or(FindingKind::InvalidChainResponse)?;
            if fresh_slot < slot {
                return Err(FindingKind::InvalidChainResponse);
            }
            let accounts = fresh
                .get("value")
                .and_then(Value::as_array)
                .filter(|a| a.len() == 2)
                .ok_or(FindingKind::InvalidChainResponse)?;
            let terms = match decode_terms_account(address, &accounts[0], &self.config) {
                Ok(t) if t.job_id == initial.job_id && t.meter_address == initial.meter_address => {
                    t
                }
                _ => {
                    report.push(
                        FindingKind::UnreadableAccount,
                        Some(address),
                        Some(initial.job_id),
                    );
                    continue;
                }
            };
            report.scanned_leases += 1;
            let meter = match accounts[1].get("owner").and_then(Value::as_str) {
                Some(owner) if owner == self.config.program_id => {
                    decode_meter(&accounts[1], address, &terms, &self.config)
                        .map(|m| MeterEvidence::Committed(m.metered_ms))
                }
                Some(DELEGATION_PROGRAM) if terms.delegated => {
                    if terms.settled() {
                        // Settlement of an expired/voided still-delegated lease
                        // uses zero, not ER bytes the L1 program never received.
                        Ok(MeterEvidence::Uncommitted)
                    } else {
                        if !rollup_verified {
                            let identity = self
                                .rpc(&self.config.er_rpc_url, "getIdentity", json!([]))
                                .await?;
                            if identity.get("identity").and_then(Value::as_str)
                                != Some(self.config.validator.as_str())
                            {
                                return Err(FindingKind::InvalidChainResponse);
                            }
                            rollup_verified = true;
                        }
                        let remote = self.rpc(&self.config.er_rpc_url, "getAccountInfo", json!([
                            terms.meter_address, {"encoding":"base64", "commitment":"confirmed"}
                        ])).await?;
                        let account = remote
                            .get("value")
                            .ok_or(FindingKind::InvalidChainResponse)?;
                        if account.get("owner").and_then(Value::as_str)
                            != Some(self.config.program_id.as_str())
                        {
                            Err(())
                        } else {
                            decode_meter(account, address, &terms, &self.config)
                                .map(|_| MeterEvidence::Uncommitted)
                        }
                    }
                }
                _ => Err(()),
            };
            let meter = match meter {
                Ok(m) => Some(m),
                Err(_) => {
                    report.push(
                        FindingKind::UnreadableMeter,
                        Some(address),
                        Some(terms.job_id),
                    );
                    None
                }
            };
            let local = state.jobs().get(terms.job_id).map(|record| LocalLease {
                phase: record.phase,
                concluded_at_ms: record.concluded_at_ms,
                expected_terms: parse_lease_terms(&record.envelope.payload.input)
                    .ok()
                    .flatten()
                    .filter(|_| record.envelope.payload.kind == JobKind::LeaseSession)
                    .map(|t| {
                        (
                            t.rate_micro_usdc_per_sec,
                            t.max_duration_secs,
                            record.payout_address,
                        )
                    }),
                settled_charge: state.escrow().settled_charge_micro_usdc(terms.job_id),
            });
            for kind in compare_lease(&terms, meter, local.as_ref(), now_ms, self.config.grace) {
                report.push(kind, Some(address), Some(terms.job_id));
            }
        }
        report.scan_complete = !report
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::UnreadableAccount);
        // Missing local-to-chain accounts are deliberately not asserted: the
        // current journal has no durable successful-open marker and the lease
        // meter explicitly permits an off-chain fallback. Chain accounts still
        // lead discovery, including accounts absent from this journal.
        Ok(())
    }
}

pub fn spawn_periodic_observer(
    state: CoordinatorState,
    observer: Arc<ChainObserver>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(observer.config.interval);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let report = observer.observe(&state, crate::epoch_ms()).await;
            // At-least-once delivery with stable event IDs; a receiver should
            // deduplicate by event_id / Idempotency-Key after an uncertain ack.
            for _ in 0..4 {
                let next = observer.alerts.read().next();
                let Some((event, delivery)) = next else {
                    break;
                };
                if !delivery.send(&observer.http, &event).await {
                    tracing::warn!(event_id=%event.event_id, "observer alert delivery failed; retained for retry");
                    break;
                }
                tracing::warn!(event_id=%event.event_id, kind=?event.kind, transition=%event.transition,
                    count=event.count, "observer alert transition; read-only, no funds moved");
                let tracker = observer.alerts.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    tracker.write().delivered(event.event_id, crate::epoch_ms())
                })
                .await;
            }
            if report.status() != ObserverStatus::Healthy {
                tracing::debug!(status = ?report.status(), scanned_leases = report.scanned_leases,
                    findings = ?report.findings, "chain-first lease observer observation; no funds moved");
            }
        }
    })
}

fn discriminator(name: &str) -> [u8; 8] {
    Sha256::digest(format!("account:{name}").as_bytes())[..8]
        .try_into()
        .expect("eight bytes")
}

fn key(value: &str) -> Result<[u8; 32], String> {
    bs58::decode(value)
        .into_vec()
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| "observer identity must be a base58 32-byte key".into())
}

fn account_data(account: &Value) -> Result<Vec<u8>, ()> {
    if account.get("executable").and_then(Value::as_bool) != Some(false) {
        return Err(());
    }
    let data = account
        .get("data")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 2)
        .ok_or(())?;
    if data[1].as_str() != Some("base64") {
        return Err(());
    }
    base64::engine::general_purpose::STANDARD
        .decode(data[0].as_str().ok_or(())?)
        .map_err(|_| ())
}

/// Uses the bump stored by the program, so no curve implementation or Solana
/// SDK enters the coordinator. Ownership, seeds and the resulting address are
/// all verified before any decoded fields are trusted.
fn address(seeds: &[&[u8]], bump: u8, program: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    for seed in seeds {
        hash.update(seed);
    }
    hash.update([bump]);
    hash.update(program);
    hash.update(b"ProgramDerivedAddress");
    bs58::encode(hash.finalize()).into_string()
}

#[derive(Debug)]
struct ChainTerms {
    job_id: Uuid,
    operator: String,
    rate: u64,
    duration: u64,
    funded: u64,
    opened_at: u64,
    paid_operator: bool,
    paid_renter: bool,
    voided: bool,
    delegated: bool,
    meter_address: String,
}

impl ChainTerms {
    fn settled(&self) -> bool {
        self.paid_operator && self.paid_renter
    }
    fn charge(&self, elapsed: u64) -> u64 {
        if self.voided {
            return 0;
        }
        (u128::from(self.rate) * u128::from(elapsed))
            .div_ceil(1000)
            .min(u128::from(self.rate) * u128::from(self.duration))
            .min(u128::from(self.funded)) as u64
    }
}

fn number(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("checked layout"))
}

fn decode_terms_account(
    pubkey: &str,
    account: &Value,
    config: &ObserverConfig,
) -> Result<ChainTerms, ()> {
    if account.get("owner").and_then(Value::as_str) != Some(config.program_id.as_str()) {
        return Err(());
    }
    let bytes = account_data(account)?;
    if bytes.len() != TERMS_LEN || bytes[..8] != discriminator("LeaseTerms") {
        return Err(());
    }
    if bytes[216..220].iter().any(|b| *b > 1) {
        return Err(());
    }
    let program = key(&config.program_id).map_err(|_| ())?;
    let p = key(pubkey).map_err(|_| ())?;
    for (offset, expected) in [
        (24, &config.renter),
        (88, &config.coordinator),
        (120, &config.validator),
        (152, &config.mint),
    ] {
        if bytes[offset..offset + 32] != key(expected).map_err(|_| ())? {
            return Err(());
        }
    }
    if address(
        &[b"lease", &bytes[24..56], &bytes[8..24]],
        bytes[220],
        &program,
    ) != pubkey
    {
        return Err(());
    }
    let opened = i64::from_le_bytes(bytes[208..216].try_into().map_err(|_| ())?);
    let rate = number(&bytes, 184);
    let duration = number(&bytes, 192);
    let funded = number(&bytes, 200);
    if opened < 0
        || rate == 0
        || !(1..=86_400).contains(&duration)
        || funded == 0
        || rate.checked_mul(duration).is_none_or(|max| funded > max)
    {
        return Err(());
    }
    Ok(ChainTerms {
        job_id: Uuid::from_slice(&bytes[8..24]).map_err(|_| ())?,
        operator: bs58::encode(&bytes[56..88]).into_string(),
        rate,
        duration,
        funded,
        opened_at: opened as u64,
        paid_operator: bytes[216] == 1,
        paid_renter: bytes[217] == 1,
        voided: bytes[218] == 1,
        delegated: bytes[219] == 1,
        meter_address: address(&[b"meter", &p], bytes[221], &program),
    })
}

struct Meter {
    metered_ms: u64,
}

fn decode_meter(
    account: &Value,
    terms_address: &str,
    terms: &ChainTerms,
    config: &ObserverConfig,
) -> Result<Meter, ()> {
    let bytes = account_data(account)?;
    if bytes.len() != METER_LEN
        || bytes[..8] != discriminator("LeaseMeter")
        || bytes[128] > 1
        || bytes[8..40] != key(terms_address).map_err(|_| ())?
        || bytes[40..72] != key(&config.coordinator).map_err(|_| ())?
        || bytes[72..88] != *terms.job_id.as_bytes()
    {
        return Err(());
    }
    Ok(Meter {
        metered_ms: number(&bytes, 88),
    })
}

#[derive(Clone, Copy)]
enum MeterEvidence {
    Committed(u64),
    Uncommitted,
}

struct LocalLease {
    phase: JobPhase,
    concluded_at_ms: Option<u64>,
    expected_terms: Option<(u64, u64, String)>,
    settled_charge: Option<u64>,
}

fn compare_lease(
    terms: &ChainTerms,
    meter: Option<MeterEvidence>,
    local: Option<&LocalLease>,
    now_ms: u64,
    grace: Duration,
) -> Vec<FindingKind> {
    let mut findings = Vec::new();
    let grace_ms = grace.as_millis() as u64;
    let age_ms = now_ms.saturating_sub(terms.opened_at.saturating_mul(1000));
    if terms.paid_operator != terms.paid_renter && age_ms > grace_ms {
        findings.push(FindingKind::PartialSettlement);
    }
    if !terms.settled() && age_ms > terms.duration.saturating_mul(1000).saturating_add(grace_ms) {
        findings.push(FindingKind::ExpiredUnsettledLease);
    }
    let Some(local) = local else {
        if age_ms > grace_ms {
            findings.push(if terms.settled() {
                FindingKind::OrphanedSettledLease
            } else {
                FindingKind::OrphanedFundedLease
            });
        }
        return findings;
    };
    if local
        .expected_terms
        .as_ref()
        .is_none_or(|(rate, duration, operator)| {
            *rate != terms.rate || *duration != terms.duration || *operator != terms.operator
        })
    {
        findings.push(FindingKind::TermsMismatch);
    }
    let terminal = matches!(
        local.phase,
        JobPhase::Completed | JobPhase::Refunded | JobPhase::Failed | JobPhase::Rejected
    );
    let terminal_aged = local
        .concluded_at_ms
        .map_or(age_ms > grace_ms, |t| now_ms.saturating_sub(t) > grace_ms);
    if terminal && !terms.settled() && terminal_aged {
        findings.push(FindingKind::LocalTerminalChainOpen);
    }
    if !terminal && terms.settled() && age_ms > grace_ms {
        findings.push(FindingKind::LocalLiveChainSettled);
    }
    if terms.settled() && terminal && terminal_aged {
        if local.settled_charge.is_none() {
            findings.push(FindingKind::LocalSettlementMissing);
        }
        let charge = match meter {
            Some(MeterEvidence::Committed(ms)) => Some(terms.charge(ms)),
            Some(MeterEvidence::Uncommitted) => Some(0),
            None => None,
        };
        if charge
            .zip(local.settled_charge)
            .is_some_and(|(chain, local)| chain != local)
        {
            findings.push(FindingKind::SettlementMismatch);
        }
    }
    findings
}

#[derive(Debug, Serialize)]
pub struct PublicObservation {
    pub status: ObserverStatus,
    pub coverage: &'static str,
    pub stale_after_ms: u64,
    pub observed_at_ms: Option<u64>,
    pub l1_slot: Option<u64>,
    pub scan_complete: bool,
    pub scanned_leases: usize,
    pub finding_counts: BTreeMap<String, usize>,
}

fn public_observation(
    report: Option<&ObserverReport>,
    now_ms: u64,
    interval: Duration,
) -> PublicObservation {
    let Some(report) = report else {
        return PublicObservation {
            status: ObserverStatus::NotObserved,
            coverage: "not_observed",
            stale_after_ms: (interval.as_millis() as u64).saturating_mul(3),
            observed_at_ms: None,
            l1_slot: None,
            scan_complete: false,
            scanned_leases: 0,
            finding_counts: BTreeMap::new(),
        };
    };
    let mut counts = BTreeMap::new();
    for finding in &report.findings {
        let name = serde_json::to_value(finding.kind)
            .expect("enum serializes")
            .as_str()
            .expect("enum string")
            .to_owned();
        *counts.entry(name).or_insert(0) += 1;
    }
    PublicObservation {
        coverage: if !report.scan_complete || report.findings.iter().any(|f| f.kind.blind()) {
            "incomplete"
        } else if report.scanned_leases == 0 {
            "no_leases"
        } else {
            "leases_observed"
        },
        stale_after_ms: (interval.as_millis() as u64).saturating_mul(3),
        status: if now_ms.saturating_sub(report.observed_at_ms)
            > (interval.as_millis() as u64).saturating_mul(3)
        {
            ObserverStatus::Stale
        } else {
            report.status()
        },
        observed_at_ms: Some(report.observed_at_ms),
        l1_slot: report.l1_slot,
        scan_complete: report.scan_complete,
        scanned_leases: report.scanned_leases,
        finding_counts: counts,
    }
}

/// JSON status is deliberately a description, not an uptime percentage. Empty
/// capacity or no settlements is not proof of an outage. It exposes no job or
/// buyer identities, including when the public receipt feed is disabled.
pub async fn public_status(
    State(state): State<CoordinatorState>,
    observer: Option<Extension<Arc<ChainObserver>>>,
) -> (HeaderMap, Json<Value>) {
    let now = crate::epoch_ms();
    let config = state.config();
    let capacity = crate::matcher::capacity_view(
        state.registry(),
        state.reputation(),
        state.bonds(),
        now,
        config.operator_liveness_timeout,
        config.min_operator_score_bps,
        config.bond_floor(),
    )
    .await;
    let observation = observer
        .as_ref()
        .map(|Extension(o)| o.public_report(now))
        .unwrap_or_else(|| {
            let mut v = public_observation(None, now, Duration::from_secs(60));
            v.status = ObserverStatus::Disabled;
            v
        });
    let latest = state.jobs().latest_recorded_lease_payout_at_ms();
    let pins = observer.as_ref().map(|Extension(o)| &o.config);
    let alerts = observer
        .as_ref()
        .map(|Extension(o)| o.alerts.read().public_view());
    let mut headers = HeaderMap::new();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    (
        headers,
        Json(
            json!({"schema_version":1, "generated_at_ms":crate::epoch_ms(), "mode":"alert_only",
                "deployment":{"revision":option_env!("COVENANT_BUILD_REVISION").unwrap_or("development"),
                    "coordinator":state.coordinator_pubkey_b58(), "chain":"solana",
                    "genesis_hash":pins.map(|c| &c.genesis_hash), "program":pins.map(|c| &c.program_id),
                    "mint":pins.map(|c| &c.mint), "lease_coordinator":pins.map(|c| &c.coordinator),
                    "funding_renter":pins.map(|c| &c.renter), "validator":pins.map(|c| &c.validator), "observer_configured":pins.is_some()},
                "alerts":alerts,
                "journal_persistence":if state.journal().is_some() {"durable"} else {"memory"},
                "journal_writable":state.journal().map(|j| j.is_healthy()).unwrap_or(true),
                "chain_observer":observation, "capacity":capacity,
                "latest_recorded_lease_payout_at_ms":latest,
                "settlement_note":"Recorded payout time; no recent settlement can also mean no recent demand.",
                "incidents":incident_record()
            }),
        ),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentRecord {
    pub schema_version: u32,
    pub incidents: Vec<Incident>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Incident {
    pub id: String,
    pub started_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
    pub effect: String,
    pub cause: String,
    pub fix: String,
    pub updates: Vec<IncidentUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentUpdate {
    pub at_ms: u64,
    pub message: String,
}

pub fn validate_incidents(record: &IncidentRecord) -> Result<(), String> {
    if record.schema_version != 1 {
        return Err("unknown incident schema version".into());
    }
    let mut ids = HashSet::new();
    for incident in &record.incidents {
        if incident.id.trim().is_empty()
            || !ids.insert(&incident.id)
            || incident.started_at_ms == 0
            || [&incident.effect, &incident.cause, &incident.fix]
                .iter()
                .any(|s| s.trim().is_empty())
            || incident.updates.is_empty()
            || incident
                .resolved_at_ms
                .is_some_and(|t| t < incident.started_at_ms)
        {
            return Err(
                "incident needs a unique id, effect, cause, fix and coherent timeline".into(),
            );
        }
        let mut previous = incident.started_at_ms;
        for update in &incident.updates {
            if update.at_ms < previous
                || update.message.trim().is_empty()
                || incident.resolved_at_ms.is_some_and(|t| update.at_ms > t)
            {
                return Err("incident updates must be ordered inside its timeline".into());
            }
            previous = update.at_ms;
        }
    }
    Ok(())
}

pub fn incident_record() -> IncidentRecord {
    let record: IncidentRecord =
        serde_json::from_str(include_str!("../incidents.json")).expect("validated incident JSON");
    validate_incidents(&record).expect("validated incident chronology");
    record
}

mod alerts;

/// Anonymous metrics, with fixed enum labels and no lease/buyer/RPC identities.
pub fn append_metrics(out: &mut String, observer: Option<&ChainObserver>, now: u64) {
    use std::fmt::Write;
    let mut observation = public_observation(None, now, Duration::from_secs(60));
    observation.status = ObserverStatus::Disabled;
    if let Some(observer) = observer {
        observation = observer.public_report(now);
    }
    for (name, help, value) in [
        (
            "compute_observer_enabled",
            "Read-only chain observer is configured.",
            u64::from(observer.is_some()),
        ),
        (
            "compute_observer_scan_complete",
            "Latest scan completed with readable terms accounts.",
            u64::from(observation.scan_complete),
        ),
        (
            "compute_observer_last_observation_timestamp_seconds",
            "Start time of the latest recorded scan.",
            observation.observed_at_ms.unwrap_or(0) / 1000,
        ),
        (
            "compute_observer_scanned_leases",
            "Lease accounts compared in latest observation.",
            observation.scanned_leases as u64,
        ),
        (
            "compute_observer_visibility_available",
            "Latest scan is fresh and readable; not a solvency claim.",
            u64::from(matches!(
                observation.status,
                ObserverStatus::Healthy | ObserverStatus::Degraded
            )),
        ),
    ] {
        let _ = writeln!(
            out,
            "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}"
        );
    }
    let _ = writeln!(out, "# HELP compute_observer_findings Latest findings by category.\n# TYPE compute_observer_findings gauge");
    for (kind, count) in observation.finding_counts {
        let _ = writeln!(out, "compute_observer_findings{{kind=\"{kind}\"}} {count}");
    }
    if let Some(observer) = observer {
        observer.alerts.read().metrics(out);
    }
}

#[cfg(test)]
mod tests;
