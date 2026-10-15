//! The IMD swarm (identity.md, api.imd.fun) as a Covenant operator.
//!
//! A swarm node serves `batch_job` for buyers who require the model
//! `imd-swarm`. It runs nothing itself: for each job it buys the same
//! work from IMD, one transaction on an EVM chain through IMD's Intake
//! contract (0.5 IMD, the payment and the ask in one call), follows the
//! order on IMD's control plane, and relays the accepted files as the
//! job's output under this node's signed receipt. A buyer pays USDC on
//! Solana and never touches IMD, Ethereum or Permit2; IMD gets a paying
//! customer per job.
//!
//! Money rules, all enforced here because IMD refunds nothing:
//! - nothing is paid before IMD's free `POST /requests/check` passes;
//! - an offer must cover the live cost of 0.5 IMD plus the operator's
//!   margin, read from a price feed and refused at admission otherwise;
//! - every payment is written to a durable order book before it is
//!   broadcast, so a node that dies mid-job resumes the same order on
//!   recovery instead of paying twice;
//! - a job IMD refuses, blocks or cancels fails here, which refunds the
//!   buyer; the lost 0.5 IMD is the operator's cost of selling work it
//!   does not control.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use covenant_compute_protocol::{parse_swarm_input, JobEnvelopePayload, SwarmOrder, SWARM_MODEL};
use covenant_mcp::Content;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::evm::{
    bytes32_string, decode_uint, encode_allowance, encode_approve_max, encode_balance_of,
    encode_hex, encode_intake_request, Address, Eip1559Transaction, EvmError, RpcClient, Signer,
};
use crate::executor::{ExecutionOutcome, ExecutorError, JobExecutor};

/// IMD's Intake v2, the same address on Ethereum, Robinhood Chain and Sepolia.
pub const INTAKE_V2: &str = "0xa43e6f75ee006411f79ac1c84120606c2330de82";
/// 0.5 IMD in atomic units: what every Intake action costs today.
pub const DEFAULT_PRICE_WEI: u128 = 500_000_000_000_000_000;
pub const DEFAULT_API_URL: &str = "https://api.imd.fun";
pub const DEFAULT_EXPLORER_URL: &str = "https://explorer.imd.fun";

/// IMD's token on the chains the Intake is served on.
pub fn default_token_for_chain(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        1 => Some("0xd34a99bc0f67ae1bbd63c660e6d0b0dd03e263b7"),
        4663 => Some("0x5f7bb59365ce557c26dbcaa4ee9d39a4b95b7127"),
        11_155_111 => Some("0x44a1cd38474fb1748400e7deb5f8d786cce3f89a"),
        _ => None,
    }
}

/// The chain name DexScreener uses, for the default price feed.
fn dexscreener_chain(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        1 => Some("ethereum"),
        4663 => Some("robinhood"),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct ImdConfig {
    pub api_url: String,
    pub explorer_url: String,
    pub rpc_url: String,
    pub chain_id: u64,
    pub intake: Address,
    pub token: Address,
    /// Hex private key of the wallet that pays IMD. Read once; never logged.
    pub key_hex: String,
    pub price_wei: u128,
    /// A fixed IMD price in USD, instead of the feed. For rigs and tests.
    pub fixed_price_usd: Option<f64>,
    /// Where the IMD/USD price is read when it is not fixed.
    pub price_source_url: String,
    /// The most the node pays for one action, in micro-USDC at the live
    /// price. Above it the node goes offline rather than serve at a loss.
    pub max_cost_micro_usdc: u64,
    /// What an offer must carry above the live cost.
    pub min_margin_micro_usdc: u64,
    /// The least time a job may have left: IMD answers in minutes to hours.
    pub min_deadline: Duration,
    pub poll_interval: Duration,
    /// How long a payment transaction may take to mine.
    pub tx_timeout: Duration,
    /// The actions this node sells, a subset of what a swarm order allows.
    pub actions: Vec<String>,
    /// Text artifacts up to this size ride back inline; larger ones by URL.
    pub max_inline_bytes: usize,
    /// The least native balance the wallet must hold for gas.
    pub min_gas_wei: u128,
    /// The order book: every payment this node made, by job.
    pub orders_path: PathBuf,
}

impl ImdConfig {
    /// Reads the `COVENANT_COMPUTE_IMD_*` knobs. `home` is the node home,
    /// where the order book lives.
    pub fn from_environment(home: &Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let env = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let chain_id: u64 = env("COVENANT_COMPUTE_IMD_CHAIN_ID")
            .map(|v| v.parse())
            .transpose()
            .context("COVENANT_COMPUTE_IMD_CHAIN_ID must be a chain id")?
            .unwrap_or(4663);
        let rpc_url = env("COVENANT_COMPUTE_IMD_RPC_URL").context(
            "COVENANT_COMPUTE_IMD_RPC_URL must name a JSON-RPC endpoint for the chain the node \
             pays IMD on (Robinhood Chain 4663 by default)",
        )?;
        let key_file = env("COVENANT_COMPUTE_IMD_KEY_FILE").context(
            "COVENANT_COMPUTE_IMD_KEY_FILE must name a file holding the hex private key of the \
             wallet that pays IMD",
        )?;
        let key_hex = std::fs::read_to_string(&key_file)
            .with_context(|| format!("read {key_file}"))?
            .trim()
            .to_string();
        let token = match env("COVENANT_COMPUTE_IMD_TOKEN") {
            Some(t) => t,
            None => default_token_for_chain(chain_id)
                .context("COVENANT_COMPUTE_IMD_TOKEN must name the IMD token on this chain")?
                .to_string(),
        };
        let fixed_price_usd = env("COVENANT_COMPUTE_IMD_PRICE_USD")
            .map(|v| v.parse::<f64>())
            .transpose()
            .context("COVENANT_COMPUTE_IMD_PRICE_USD must be a price in USD")?;
        let price_source_url = env("COVENANT_COMPUTE_IMD_PRICE_SOURCE_URL")
            .unwrap_or_else(|| format!("https://api.dexscreener.com/latest/dex/tokens/{token}"));
        if fixed_price_usd.is_none()
            && dexscreener_chain(chain_id).is_none()
            && env("COVENANT_COMPUTE_IMD_PRICE_SOURCE_URL").is_none()
        {
            anyhow::bail!(
                "no price feed for chain {chain_id}: set COVENANT_COMPUTE_IMD_PRICE_USD or \
                 COVENANT_COMPUTE_IMD_PRICE_SOURCE_URL"
            );
        }
        let parse_u64 = |key: &str, default: u64| -> anyhow::Result<u64> {
            env(key)
                .map(|v| v.parse::<u64>())
                .transpose()
                .with_context(|| format!("{key} must be a whole number"))
                .map(|v| v.unwrap_or(default))
        };
        let actions: Vec<String> = env("COVENANT_COMPUTE_IMD_ACTIONS")
            .unwrap_or_else(|| "job.open,job.continue,oracle.request".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        anyhow::ensure!(
            !actions.is_empty(),
            "COVENANT_COMPUTE_IMD_ACTIONS names no action"
        );
        Ok(ImdConfig {
            api_url: env("COVENANT_COMPUTE_IMD_API_URL")
                .unwrap_or_else(|| DEFAULT_API_URL.into())
                .trim_end_matches('/')
                .to_string(),
            explorer_url: env("COVENANT_COMPUTE_IMD_EXPLORER_URL")
                .unwrap_or_else(|| DEFAULT_EXPLORER_URL.into())
                .trim_end_matches('/')
                .to_string(),
            rpc_url,
            chain_id,
            intake: Address::parse(
                &env("COVENANT_COMPUTE_IMD_INTAKE").unwrap_or_else(|| INTAKE_V2.into()),
            )
            .context("COVENANT_COMPUTE_IMD_INTAKE")?,
            token: Address::parse(&token).context("COVENANT_COMPUTE_IMD_TOKEN")?,
            key_hex,
            price_wei: env("COVENANT_COMPUTE_IMD_PRICE_WEI")
                .map(|v| v.parse::<u128>())
                .transpose()
                .context("COVENANT_COMPUTE_IMD_PRICE_WEI must be atomic units")?
                .unwrap_or(DEFAULT_PRICE_WEI),
            fixed_price_usd,
            price_source_url,
            max_cost_micro_usdc: parse_u64("COVENANT_COMPUTE_IMD_MAX_COST_MICRO_USDC", 4_500_000)?,
            min_margin_micro_usdc: parse_u64(
                "COVENANT_COMPUTE_IMD_MIN_MARGIN_MICRO_USDC",
                500_000,
            )?,
            min_deadline: Duration::from_secs(parse_u64(
                "COVENANT_COMPUTE_IMD_MIN_DEADLINE_SECS",
                1_800,
            )?),
            poll_interval: Duration::from_secs(parse_u64("COVENANT_COMPUTE_IMD_POLL_SECS", 15)?),
            tx_timeout: Duration::from_secs(parse_u64(
                "COVENANT_COMPUTE_IMD_TX_TIMEOUT_SECS",
                180,
            )?),
            actions,
            max_inline_bytes: parse_u64("COVENANT_COMPUTE_IMD_MAX_INLINE_BYTES", 1_048_576)?
                as usize,
            min_gas_wei: env("COVENANT_COMPUTE_IMD_MIN_GAS_WEI")
                .map(|v| v.parse::<u128>())
                .transpose()
                .context("COVENANT_COMPUTE_IMD_MIN_GAS_WEI must be wei")?
                .unwrap_or(100_000_000_000_000),
            orders_path: home.join("imd-orders.jsonl"),
        })
    }
}

// ------------------------------------------------------------ order book

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    /// Signed and written down; the broadcast may or may not have landed.
    Prepared,
    /// The chain accepted the transaction.
    Sent,
    /// IMD read the payment and opened the work.
    Admitted,
    Completed,
    /// IMD refused the body or the payment: nothing to collect.
    Refused,
    /// The work ended without a result, or the payment was lost.
    Failed,
}

/// One payment this node made for one Covenant job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImdOrder {
    pub job_id: Uuid,
    pub buyer: String,
    pub action: String,
    pub body_sha256: String,
    pub state: OrderState,
    #[serde(default)]
    pub nonce: Option<u64>,
    #[serde(default)]
    pub tx_hash: Option<String>,
    #[serde(default)]
    pub raw_tx: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub imd_job_id: Option<String>,
    #[serde(default)]
    pub oracle_request_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Append-only JSONL, last state per job wins on replay. The same idiom
/// as the node's other books, without compaction: a swarm node pays at
/// most a few hundred times a month.
pub struct OrderBook {
    path: Option<PathBuf>,
    orders: Mutex<Vec<ImdOrder>>,
}

impl OrderBook {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut orders: Vec<ImdOrder> = Vec::new();
        if path.exists() {
            for line in std::fs::read_to_string(path)?.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // A torn final line is the crash this book exists for.
                let Ok(order) = serde_json::from_str::<ImdOrder>(line) else {
                    continue;
                };
                match orders.iter_mut().find(|o| o.job_id == order.job_id) {
                    Some(slot) => *slot = order,
                    None => orders.push(order),
                }
            }
        }
        Ok(OrderBook {
            path: Some(path.to_path_buf()),
            orders: Mutex::new(orders),
        })
    }

    pub fn in_memory() -> Self {
        OrderBook {
            path: None,
            orders: Mutex::new(Vec::new()),
        }
    }

    pub fn find(&self, job_id: Uuid) -> Option<ImdOrder> {
        self.orders
            .lock()
            .iter()
            .find(|o| o.job_id == job_id)
            .cloned()
    }

    /// The IMD job this book bought for `buyer`, by IMD job id: a
    /// continuation may only follow a project its own buyer paid for here.
    pub fn owns_imd_job(&self, buyer: &str, imd_job_id: &str) -> bool {
        self.orders
            .lock()
            .iter()
            .any(|o| o.buyer == buyer && o.imd_job_id.as_deref() == Some(imd_job_id))
    }

    pub fn record(&self, order: ImdOrder) -> Result<(), ExecutorError> {
        {
            let mut orders = self.orders.lock();
            match orders.iter_mut().find(|o| o.job_id == order.job_id) {
                Some(slot) => *slot = order.clone(),
                None => orders.push(order.clone()),
            }
        }
        if let Some(path) = &self.path {
            use std::io::Write;
            let mut line = serde_json::to_string(&order)
                .map_err(|e| ExecutorError::Failed(format!("order book: {e}")))?;
            line.push('\n');
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| ExecutorError::Failed(format!("order book: {e}")))?;
            file.write_all(line.as_bytes())
                .map_err(|e| ExecutorError::Failed(format!("order book: {e}")))?;
            file.sync_data()
                .map_err(|e| ExecutorError::Failed(format!("order book: {e}")))?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.orders.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// -------------------------------------------------------------- executor

#[derive(Debug, Clone, Copy)]
struct Spot {
    usd: f64,
    read_at: Instant,
}

const SPOT_TTL: Duration = Duration::from_secs(600);
const HEALTH_TTL: Duration = Duration::from_secs(60);

pub struct ImdExecutor {
    config: ImdConfig,
    http: reqwest::Client,
    rpc: RpcClient,
    signer: Signer,
    book: Arc<OrderBook>,
    spot: Mutex<Option<Spot>>,
    health: Mutex<Option<(Instant, Result<(), String>)>>,
}

impl std::fmt::Debug for ImdExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImdExecutor")
            .field("api_url", &self.config.api_url)
            .field("chain_id", &self.config.chain_id)
            .field("wallet", &self.signer.address())
            .finish()
    }
}

/// What a job's input asks for, once admitted.
struct Order {
    swarm: SwarmOrder,
    body: Vec<u8>,
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn failed(msg: impl Into<String>) -> ExecutorError {
    ExecutorError::Failed(msg.into())
}

impl ImdExecutor {
    pub fn new(config: ImdConfig) -> anyhow::Result<Self> {
        let signer = Signer::from_hex(&config.key_hex)
            .map_err(|e| anyhow::anyhow!("COVENANT_COMPUTE_IMD_KEY_FILE: {e}"))?;
        let book = OrderBook::open(&config.orders_path)
            .map_err(|e| anyhow::anyhow!("open {}: {e}", config.orders_path.display()))?;
        Ok(Self::with_parts(config, signer, Arc::new(book)))
    }

    pub fn with_parts(config: ImdConfig, signer: Signer, book: Arc<OrderBook>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("a client with a timeout builds");
        let rpc = RpcClient::new(config.rpc_url.clone(), Duration::from_secs(30));
        ImdExecutor {
            config,
            http,
            rpc,
            signer,
            book,
            spot: Mutex::new(None),
            health: Mutex::new(None),
        }
    }

    pub fn wallet(&self) -> Address {
        self.signer.address()
    }

    pub fn book(&self) -> &OrderBook {
        &self.book
    }

    pub fn config(&self) -> &ImdConfig {
        &self.config
    }

    // ------------------------------------------------------------ price

    /// The live cost of one action in micro-USDC, from the cached price.
    pub fn cached_cost_micro_usdc(&self) -> Option<u64> {
        let spot = (*self.spot.lock())?;
        if spot.read_at.elapsed() > SPOT_TTL * 3 {
            return None;
        }
        Some(self.cost_at(spot.usd))
    }

    fn cost_at(&self, usd_per_imd: f64) -> u64 {
        let imd = self.config.price_wei as f64 / 1e18;
        (imd * usd_per_imd * 1e6).ceil().max(0.0) as u64
    }

    /// The least an offer must carry: live cost plus margin.
    pub fn least_offer_micro_usdc(&self) -> Option<u64> {
        self.cached_cost_micro_usdc()
            .map(|cost| cost.saturating_add(self.config.min_margin_micro_usdc))
    }

    async fn refresh_spot(&self) -> Result<f64, ExecutorError> {
        if let Some(fixed) = self.config.fixed_price_usd {
            *self.spot.lock() = Some(Spot {
                usd: fixed,
                read_at: Instant::now(),
            });
            return Ok(fixed);
        }
        if let Some(spot) = *self.spot.lock() {
            if spot.read_at.elapsed() < SPOT_TTL {
                return Ok(spot.usd);
            }
        }
        let value: Value = self
            .http
            .get(&self.config.price_source_url)
            .send()
            .await
            .map_err(|e| failed(format!("price feed: {e}")))?
            .error_for_status()
            .map_err(|e| failed(format!("price feed: {e}")))?
            .json()
            .await
            .map_err(|e| failed(format!("price feed: {e}")))?;
        let usd = parse_price_feed(&value, dexscreener_chain(self.config.chain_id))
            .ok_or_else(|| failed("price feed carried no usable IMD price"))?;
        *self.spot.lock() = Some(Spot {
            usd,
            read_at: Instant::now(),
        });
        Ok(usd)
    }

    // --------------------------------------------------------- imd api

    fn api(&self, path: &str) -> String {
        format!("{}{}", self.config.api_url, path)
    }

    async fn get_json(&self, url: &str) -> Result<(u16, Value), ExecutorError> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| failed(format!("GET {url}: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| failed(format!("GET {url}: {e}")))?;
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        Ok((status, value))
    }

    async fn check(&self, order: &SwarmOrder) -> Result<(), ExecutorError> {
        let url = self.api("/requests/check");
        let resp = self
            .http
            .post(&url)
            .json(&json!({ "action": order.action, "input": order.body }))
            .send()
            .await
            .map_err(|e| failed(format!("IMD check: {e}")))?;
        let status = resp.status().as_u16();
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        if status == 422 {
            return Err(failed(format!(
                "IMD would refuse this body: {}",
                compact(value.get("problems").unwrap_or(&value))
            )));
        }
        if !(200..300).contains(&status) {
            return Err(failed(format!(
                "IMD check answered {status}: {}",
                compact(&value)
            )));
        }
        let blockers = value
            .get("blockers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !blockers.is_empty() {
            return Err(failed(format!(
                "IMD would refuse this body: {}",
                compact(&Value::Array(blockers))
            )));
        }
        Ok(())
    }

    // ------------------------------------------------------------ chain

    async fn allowance(&self) -> Result<u128, EvmError> {
        let data = encode_allowance(self.signer.address(), self.config.intake);
        decode_uint(&self.rpc.eth_call(self.config.token, &data).await?)
    }

    async fn token_balance(&self) -> Result<u128, EvmError> {
        let data = encode_balance_of(self.signer.address());
        decode_uint(&self.rpc.eth_call(self.config.token, &data).await?)
    }

    async fn fees(&self) -> Result<(u128, u128), EvmError> {
        let priority = self.rpc.priority_fee().await?;
        match self.rpc.base_fee().await? {
            Some(base) => Ok((priority, base.saturating_mul(2).saturating_add(priority))),
            None => {
                let price = self.rpc.gas_price().await?;
                Ok((price, price))
            }
        }
    }

    async fn sign_call(
        &self,
        to: Address,
        data: Vec<u8>,
    ) -> Result<(u64, crate::evm::SignedTransaction), ExecutorError> {
        let from = self.signer.address();
        let nonce = self
            .rpc
            .pending_nonce(from)
            .await
            .map_err(|e| failed(format!("nonce: {e}")))?;
        let gas = self
            .rpc
            .estimate_gas(from, to, &data)
            .await
            .map_err(|e| failed(format!("gas estimate: {e}")))?;
        let gas_limit = gas.saturating_mul(13).div_ceil(10).min(3_000_000);
        let (priority, max_fee) = self
            .fees()
            .await
            .map_err(|e| failed(format!("fees: {e}")))?;
        let tx = Eip1559Transaction {
            chain_id: self.config.chain_id,
            nonce,
            max_priority_fee_per_gas: priority,
            max_fee_per_gas: max_fee,
            gas_limit,
            to,
            value: 0,
            data,
        };
        let signed = tx
            .sign(&self.signer)
            .map_err(|e| failed(format!("sign: {e}")))?;
        Ok((nonce, signed))
    }

    /// Broadcasts `raw` unless the chain already knows `hash`. "Already
    /// known" answers are fine; a nonce the chain has moved past is not,
    /// unless the hash is mined, which the caller checks next.
    async fn broadcast(&self, raw_hex: &str, hash_hex: &str) -> Result<(), ExecutorError> {
        if self
            .rpc
            .knows_transaction(hash_hex)
            .await
            .map_err(|e| failed(format!("rpc: {e}")))?
        {
            return Ok(());
        }
        match self.rpc.send_raw(raw_hex).await {
            Ok(reported) => {
                if !reported.eq_ignore_ascii_case(hash_hex) {
                    tracing::warn!(
                        reported,
                        computed = hash_hex,
                        "the rpc reported a different hash for the broadcast transaction"
                    );
                }
                Ok(())
            }
            Err(EvmError::Rpc { message, .. })
                if message.to_ascii_lowercase().contains("already known")
                    || message.to_ascii_lowercase().contains("already imported") =>
            {
                Ok(())
            }
            Err(e) => Err(failed(format!("broadcast: {e}"))),
        }
    }

    async fn wait_mined(&self, hash_hex: &str, until: Instant) -> Result<(), ExecutorError> {
        let started = Instant::now();
        loop {
            if let Some(receipt) = self
                .rpc
                .receipt(hash_hex)
                .await
                .map_err(|e| failed(format!("receipt: {e}")))?
            {
                if receipt.success {
                    return Ok(());
                }
                return Err(failed(format!("transaction {hash_hex} reverted")));
            }
            if started.elapsed() > self.config.tx_timeout {
                return Err(failed(format!(
                    "transaction {hash_hex} not mined within {}s",
                    self.config.tx_timeout.as_secs()
                )));
            }
            if Instant::now() >= until {
                return Err(ExecutorError::Timeout(Duration::ZERO));
            }
            tokio::time::sleep(self.config.poll_interval.min(Duration::from_secs(3))).await;
        }
    }

    async fn ensure_allowance(&self, until: Instant) -> Result<(), ExecutorError> {
        let allowance = self
            .allowance()
            .await
            .map_err(|e| failed(format!("allowance: {e}")))?;
        if allowance >= self.config.price_wei {
            return Ok(());
        }
        tracing::info!(
            token = %self.config.token,
            intake = %self.config.intake,
            "approving the intake to pull IMD, once"
        );
        let (_, signed) = self
            .sign_call(self.config.token, encode_approve_max(self.config.intake))
            .await?;
        self.broadcast(&signed.raw_hex(), &signed.hash_hex())
            .await?;
        self.wait_mined(&signed.hash_hex(), until).await
    }

    // --------------------------------------------------------- the job

    fn parse_order(&self, job: &JobEnvelopePayload) -> Result<Order, String> {
        if job.capability_requirement.model_id.as_deref() != Some(SWARM_MODEL) {
            return Err(format!(
                "a swarm node serves only jobs that require the model {SWARM_MODEL}"
            ));
        }
        let swarm = parse_swarm_input(&job.input).map_err(|e| e.to_string())?;
        if !self.config.actions.contains(&swarm.action) {
            return Err(format!(
                "this node sells {}, not {}",
                self.config.actions.join(", "),
                swarm.action
            ));
        }
        if swarm.action == "job.continue" {
            let parent = swarm
                .body
                .get("parentJobId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !self.book.owns_imd_job(&job.buyer.display, parent) {
                return Err(format!(
                    "job.continue may only follow a project this buyer bought through this node; \
                     {parent} is not one"
                ));
            }
        }
        let body = swarm.compact_body().map_err(|e| e.to_string())?;
        Ok(Order { swarm, body })
    }

    fn price_check(&self, offered: u64) -> Result<u64, String> {
        let cost = self
            .cached_cost_micro_usdc()
            .ok_or("the IMD price is not known yet; the node is still probing its feed")?;
        if cost > self.config.max_cost_micro_usdc {
            return Err(format!(
                "one IMD action costs {cost} micro-USDC at the live price, above this node's \
                 ceiling of {}",
                self.config.max_cost_micro_usdc
            ));
        }
        let least = cost.saturating_add(self.config.min_margin_micro_usdc);
        if offered < least {
            return Err(format!(
                "offer {offered} micro-USDC is below the live cost {cost} plus this node's \
                 margin {}",
                self.config.min_margin_micro_usdc
            ));
        }
        Ok(cost)
    }

    /// Pays for `order`, or resumes the payment a previous life recorded,
    /// and returns the mined transaction hash.
    async fn pay(
        &self,
        job: &JobEnvelopePayload,
        order: &Order,
        until: Instant,
    ) -> Result<ImdOrder, ExecutorError> {
        let body_sha256 = sha256_hex(&order.body);
        let mut record = match self.book.find(job.job_id) {
            Some(existing) => {
                if existing.body_sha256 != body_sha256 {
                    return Err(failed(
                        "the order book holds a different body for this job; not paying again",
                    ));
                }
                existing
            }
            None => {
                self.check(&order.swarm).await?;
                self.refresh_spot().await?;
                self.price_check(job.price_micro_usdc).map_err(failed)?;
                self.ensure_allowance(until).await?;
                let action = bytes32_string(
                    order
                        .swarm
                        .action_id()
                        .ok_or_else(|| failed("action is not sold"))?,
                )
                .map_err(|e| failed(e.to_string()))?;
                let data = encode_intake_request(
                    action,
                    &order.body,
                    (Address::ZERO, [0u8; 4]),
                    self.config.token,
                    self.config.price_wei,
                );
                let (nonce, signed) = self.sign_call(self.config.intake, data).await?;
                let now = epoch_ms();
                let record = ImdOrder {
                    job_id: job.job_id,
                    buyer: job.buyer.display.clone(),
                    action: order.swarm.action.clone(),
                    body_sha256,
                    state: OrderState::Prepared,
                    nonce: Some(nonce),
                    tx_hash: Some(signed.hash_hex()),
                    raw_tx: Some(signed.raw_hex()),
                    request_id: None,
                    imd_job_id: None,
                    oracle_request_id: None,
                    note: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                // Written before the broadcast: a crash between the two
                // re-broadcasts the same bytes on recovery, never a second
                // payment.
                self.book.record(record.clone())?;
                record
            }
        };

        match record.state {
            OrderState::Refused | OrderState::Failed => {
                return Err(failed(format!(
                    "this job's IMD order already ended {:?}: {}",
                    record.state,
                    record.note.clone().unwrap_or_default()
                )));
            }
            OrderState::Prepared => {
                let (raw, hash) = match (&record.raw_tx, &record.tx_hash) {
                    (Some(raw), Some(hash)) => (raw.clone(), hash.clone()),
                    _ => return Err(failed("a prepared order carries no transaction")),
                };
                if let Err(e) = self.broadcast(&raw, &hash).await {
                    // A nonce the chain moved past, with our hash unknown,
                    // means the payment never happened. Say so and stop;
                    // a fresh payment is an operator's decision.
                    record.state = OrderState::Failed;
                    record.note = Some(format!("payment lost: {e}"));
                    record.updated_at_ms = epoch_ms();
                    self.book.record(record.clone())?;
                    return Err(failed(format!("IMD payment failed: {e}")));
                }
                self.wait_mined(&hash, until).await?;
                record.state = OrderState::Sent;
                record.updated_at_ms = epoch_ms();
                self.book.record(record.clone())?;
            }
            OrderState::Sent | OrderState::Admitted | OrderState::Completed => {}
        }
        Ok(record)
    }

    /// Follows the payment on IMD's plane until the work is admitted.
    async fn await_admission(
        &self,
        mut record: ImdOrder,
        until: Instant,
    ) -> Result<ImdOrder, ExecutorError> {
        if matches!(record.state, OrderState::Admitted | OrderState::Completed) {
            return Ok(record);
        }
        let hash = record
            .tx_hash
            .clone()
            .ok_or_else(|| failed("order carries no transaction hash"))?;
        let url = self.api(&format!("/intake/{}/{}", self.config.chain_id, hash));
        loop {
            let (status, value) = self.get_json(&url).await?;
            if status == 200 {
                let entries = value
                    .get("entries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let entry = entries
                    .iter()
                    .find(|e| e.get("kind").and_then(Value::as_str) == Some("request"))
                    .or(entries.first())
                    .cloned()
                    .unwrap_or(Value::Null);
                let state = entry.get("state").and_then(Value::as_str).unwrap_or("");
                record.request_id = entry
                    .get("requestId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(record.request_id.clone());
                match state {
                    "admitted" | "completing" | "completed" => {
                        let result = entry
                            .pointer("/admission/result")
                            .cloned()
                            .unwrap_or(Value::Null);
                        record.imd_job_id = result
                            .get("jobId")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        record.oracle_request_id = result
                            .get("requestId")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| {
                                result
                                    .get("attestationUrl")
                                    .and_then(Value::as_str)
                                    .and_then(oracle_id_from_url)
                            });
                        if record.imd_job_id.is_none() && record.oracle_request_id.is_none() {
                            return Err(failed(format!(
                                "IMD admitted the order but named no job: {}",
                                compact(&entry)
                            )));
                        }
                        record.state = OrderState::Admitted;
                        record.updated_at_ms = epoch_ms();
                        self.book.record(record.clone())?;
                        return Ok(record);
                    }
                    "refused" | "failed" => {
                        let reason = entry.get("reason").and_then(Value::as_str).unwrap_or("");
                        let detail = entry.get("detail").and_then(Value::as_str).unwrap_or("");
                        record.state = if state == "refused" {
                            OrderState::Refused
                        } else {
                            OrderState::Failed
                        };
                        record.note = Some(format!("{reason} {detail}").trim().to_string());
                        record.updated_at_ms = epoch_ms();
                        self.book.record(record.clone())?;
                        return Err(failed(format!("IMD {state} the order: {reason} {detail}")));
                    }
                    _ => {}
                }
            } else if status != 404 {
                return Err(failed(format!("IMD intake read answered {status}")));
            }
            self.sleep_until(until).await?;
        }
    }

    async fn sleep_until(&self, until: Instant) -> Result<(), ExecutorError> {
        let now = Instant::now();
        if now + self.config.poll_interval >= until {
            return Err(ExecutorError::Timeout(Duration::ZERO));
        }
        tokio::time::sleep(self.config.poll_interval).await;
        Ok(())
    }

    async fn await_job(
        &self,
        record: &ImdOrder,
        imd_job_id: &str,
        until: Instant,
    ) -> Result<Value, ExecutorError> {
        let job_url = self.api(&format!("/jobs/{imd_job_id}"));
        let state = loop {
            let (status, value) = self.get_json(&job_url).await?;
            if status != 200 {
                return Err(failed(format!("IMD job read answered {status}")));
            }
            let state = value
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            match state.as_str() {
                "completed" => break state,
                "blocked" | "cancelled" | "failed" | "deleted" | "superseded" => {
                    let reason = value
                        .get("blockedReason")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let mut ended = record.clone();
                    ended.state = OrderState::Failed;
                    ended.note = Some(format!("job {state} {reason}").trim().to_string());
                    ended.updated_at_ms = epoch_ms();
                    self.book.record(ended)?;
                    return Err(failed(format!(
                        "IMD job {imd_job_id} ended {state}: {reason}"
                    )));
                }
                _ => {}
            }
            self.sleep_until(until).await?;
        };

        let (status, result) = self
            .get_json(&self.api(&format!("/jobs/{imd_job_id}/result")))
            .await?;
        if status != 200 {
            return Err(failed(format!("IMD result read answered {status}")));
        }
        let mut files = Vec::new();
        let mut inlined = 0usize;
        for file in result
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let mut entry = file.clone();
            let bytes = file.get("bytes").and_then(Value::as_u64).unwrap_or(0) as usize;
            let media = file
                .get("mediaType")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let textual = media.starts_with("text/")
                || media == "application/json"
                || media.ends_with("+json")
                || media == "application/x-ndjson";
            let url = file.get("url").and_then(Value::as_str).unwrap_or("");
            if textual
                && bytes <= self.config.max_inline_bytes
                && inlined + bytes <= self.config.max_inline_bytes
                && !url.is_empty()
            {
                let body = self.fetch_bytes(url, self.config.max_inline_bytes).await?;
                if let Some(expected) = file.get("hash").and_then(Value::as_str) {
                    let got = sha256_hex(&body);
                    if !got.eq_ignore_ascii_case(expected) {
                        return Err(failed(format!(
                            "artifact {url} hashed to {got}, IMD says {expected}"
                        )));
                    }
                }
                inlined += body.len();
                entry["content"] = Value::String(String::from_utf8_lossy(&body).into_owned());
            }
            files.push(entry);
        }
        let mut report = Value::Null;
        if record.action != "oracle.request" {
            let (status, text) = self
                .fetch_text(&self.api(&format!("/jobs/{imd_job_id}/report.md")))
                .await?;
            if status == 200 && !text.trim().is_empty() {
                report = Value::String(text);
            }
        }
        Ok(json!({
            "state": state,
            "result": result,
            "files": files,
            "report": report,
        }))
    }

    async fn await_oracle(&self, request_id: &str, until: Instant) -> Result<Value, ExecutorError> {
        let status_url = self.api(&format!("/oracle/requests/{request_id}"));
        let attestation_url = self.api(&format!("/oracle/requests/{request_id}/attestation"));
        loop {
            let (status, attestation) = self.get_json(&attestation_url).await?;
            if status == 200 {
                return Ok(json!({ "state": "attested", "attestation": attestation }));
            }
            let (status, request) = self.get_json(&status_url).await?;
            if status == 200 {
                let state = request.get("status").and_then(Value::as_str).unwrap_or("");
                if matches!(
                    state,
                    "disagreed" | "blocked" | "mismatch" | "refused" | "failed"
                ) {
                    return Err(failed(format!(
                        "IMD oracle request {request_id} ended {state}: {}",
                        compact(request.get("failure").unwrap_or(&Value::Null))
                    )));
                }
            }
            self.sleep_until(until).await?;
        }
    }

    async fn fetch_bytes(&self, url: &str, cap: usize) -> Result<Vec<u8>, ExecutorError> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| failed(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(failed(format!("GET {url}: HTTP {}", resp.status())));
        }
        let bytes = crate::executor::read_body_capped(resp, cap, Duration::from_secs(60)).await?;
        Ok(bytes)
    }

    async fn fetch_text(&self, url: &str) -> Result<(u16, String), ExecutorError> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| failed(format!("GET {url}: {e}")))?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            return Ok((status, String::new()));
        }
        let bytes = crate::executor::read_body_capped(
            resp,
            self.config.max_inline_bytes,
            Duration::from_secs(60),
        )
        .await?;
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn probe(&self) -> Result<(), String> {
        let (status, value) = self
            .get_json(&self.api("/health"))
            .await
            .map_err(|e| e.to_string())?;
        if status != 200 || value.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(format!("IMD control plane answered {status}"));
        }
        let chain = self.rpc.chain_id().await.map_err(|e| e.to_string())?;
        if chain != self.config.chain_id {
            return Err(format!(
                "rpc serves chain {chain}, the node is configured for {}",
                self.config.chain_id
            ));
        }
        let gas = self
            .rpc
            .native_balance(self.signer.address())
            .await
            .map_err(|e| e.to_string())?;
        if gas < self.config.min_gas_wei {
            return Err(format!(
                "wallet {} holds {gas} wei for gas, below the {} wei floor",
                self.signer.address(),
                self.config.min_gas_wei
            ));
        }
        let imd = self.token_balance().await.map_err(|e| e.to_string())?;
        if imd < self.config.price_wei {
            return Err(format!(
                "wallet {} holds {imd} atomic IMD, below one action's price {}",
                self.signer.address(),
                self.config.price_wei
            ));
        }
        let spot = self.refresh_spot().await.map_err(|e| e.to_string())?;
        let cost = self.cost_at(spot);
        if cost > self.config.max_cost_micro_usdc {
            return Err(format!(
                "one action costs {cost} micro-USDC at ${spot:.4} per IMD, above the ceiling {}",
                self.config.max_cost_micro_usdc
            ));
        }
        Ok(())
    }
}

/// `/oracle/requests/<id>/attestation`, absolute or relative, to its id.
fn oracle_id_from_url(url: &str) -> Option<String> {
    let marker = "/oracle/requests/";
    let start = url.find(marker)? + marker.len();
    let rest = &url[start..];
    let id = rest.split('/').next()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Reads an IMD/USD price out of a DexScreener token answer: the deepest
/// pool on `chain` (any chain when none is named).
pub fn parse_price_feed(value: &Value, chain: Option<&str>) -> Option<f64> {
    let pairs = value.get("pairs").and_then(Value::as_array)?;
    let mut best: Option<(f64, f64)> = None;
    for pair in pairs {
        if let Some(chain) = chain {
            if pair.get("chainId").and_then(Value::as_str) != Some(chain) {
                continue;
            }
        }
        let price = pair
            .get("priceUsd")
            .and_then(Value::as_str)
            .and_then(|p| p.parse::<f64>().ok())?;
        let liquidity = pair
            .pointer("/liquidity/usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if price.is_finite() && price > 0.0 && best.is_none_or(|(l, _)| liquidity > l) {
            best = Some((liquidity, price));
        }
    }
    best.map(|(_, price)| price)
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 600 {
        format!("{}…", &text[..600])
    } else {
        text
    }
}

#[async_trait]
impl JobExecutor for ImdExecutor {
    async fn execute(
        &self,
        job: &JobEnvelopePayload,
        deadline: Duration,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let started = Instant::now();
        let until = started + deadline.saturating_sub(Duration::from_secs(5));
        let order = self.parse_order(job).map_err(failed)?;
        let record = self.pay(job, &order, until).await?;
        let record = self.await_admission(record, until).await?;
        let outcome = if let Some(imd_job_id) = record.imd_job_id.clone() {
            self.await_job(&record, &imd_job_id, until).await?
        } else if let Some(request_id) = record.oracle_request_id.clone() {
            self.await_oracle(&request_id, until).await?
        } else {
            return Err(failed(
                "the order names neither a job nor an oracle request",
            ));
        };
        let mut done = record.clone();
        done.state = OrderState::Completed;
        done.updated_at_ms = epoch_ms();
        self.book.record(done)?;

        let explorer = record
            .imd_job_id
            .as_ref()
            .map(|id| format!("{}/jobs/{id}", self.config.explorer_url));
        let summary = json!({
            "swarm": {
                "provider": "imd",
                "action": record.action,
                "paidWith": {
                    "chainId": self.config.chain_id,
                    "txHash": record.tx_hash,
                    "asset": self.config.token.to_hex(),
                    "amountWei": self.config.price_wei.to_string(),
                    "intake": self.config.intake.to_hex(),
                },
                "requestId": record.request_id,
                "jobId": record.imd_job_id,
                "oracleRequestId": record.oracle_request_id,
                "explorer": explorer,
            },
            "outcome": outcome,
        });
        let text = match (&record.imd_job_id, &record.oracle_request_id) {
            (Some(id), _) => format!(
                "IMD job {id} completed; files and the result are in the JSON block; \
                 {}/jobs/{id}",
                self.config.explorer_url
            ),
            (None, Some(id)) => format!("IMD oracle request {id} attested; see the JSON block"),
            _ => "IMD order completed".to_string(),
        };
        Ok(ExecutionOutcome {
            output: vec![Content::json(summary), Content::text(text)],
            wall_ms: started.elapsed().as_millis() as u64,
            tokens_in: None,
            tokens_out: None,
            finish_reason: None,
        })
    }

    fn admit(&self, job: &JobEnvelopePayload, remaining: Duration) -> Result<(), String> {
        self.parse_order(job)?;
        if remaining < self.config.min_deadline {
            return Err(format!(
                "{}s left on the job, below this node's {}s floor: IMD answers in minutes to \
                 hours",
                remaining.as_secs(),
                self.config.min_deadline.as_secs()
            ));
        }
        self.price_check(job.price_micro_usdc)?;
        Ok(())
    }

    async fn health(&self) -> Result<(), ExecutorError> {
        if let Some((at, verdict)) = self.health.lock().clone() {
            if at.elapsed() < HEALTH_TTL {
                return verdict.map_err(ExecutorError::Failed);
            }
        }
        let verdict = self.probe().await;
        *self.health.lock() = Some((Instant::now(), verdict.clone()));
        verdict.map_err(ExecutorError::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path as AxPath, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
    use covenant_compute_protocol::{swarm_input, CapabilityRequirement, JobKind};
    use covenant_types::AgentId;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";

    /// What the mocked chain and plane saw, for the tests to assert on.
    #[derive(Default)]
    struct Seen {
        rpc_methods: Mutex<Vec<String>>,
        intake_reads: AtomicUsize,
        job_reads: AtomicUsize,
        allowance_zero: bool,
        job_ends: &'static str,
        intake_ends: &'static str,
        check_blockers: Vec<&'static str>,
    }

    async fn rpc_handler(State(seen): State<Arc<Seen>>, Json(req): Json<Value>) -> Json<Value> {
        let method = req["method"].as_str().unwrap_or("").to_string();
        seen.rpc_methods.lock().push(method.clone());
        let result = match method.as_str() {
            "eth_chainId" => json!("0x1237"),
            "eth_getTransactionCount" => json!("0x5"),
            "eth_getBalance" => json!("0x2386f26fc10000"),
            "eth_call" => {
                let data = req["params"][0]["data"].as_str().unwrap_or("");
                if data.starts_with("0xdd62ed3e") {
                    if seen.allowance_zero {
                        json!(format!("0x{}", "0".repeat(64)))
                    } else {
                        json!(format!("0x{}", "f".repeat(64)))
                    }
                } else {
                    // balanceOf: 2 IMD
                    json!("0x0000000000000000000000000000000000000000000000001bc16d674ec80000")
                }
            }
            "eth_estimateGas" => json!("0x30000"),
            "eth_getBlockByNumber" => json!({ "baseFeePerGas": "0x1440000" }),
            "eth_maxPriorityFeePerGas" => {
                return Json(json!({
                    "jsonrpc": "2.0", "id": 1,
                    "error": { "code": -32601, "message": "method not found" }
                }));
            }
            "eth_getTransactionByHash" => Value::Null,
            "eth_sendRawTransaction" => json!("0x00"),
            "eth_getTransactionReceipt" => json!({ "status": "0x1", "blockNumber": "0x10" }),
            _ => Value::Null,
        };
        Json(json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
    }

    fn report_body() -> &'static str {
        "# Report\n\nDeterministic testing compared.\n"
    }

    fn plane_router(seen: Arc<Seen>, base: String) -> Router {
        let report_hash = sha256_hex(report_body().as_bytes());
        Router::new()
            .route("/health", get(|| async { Json(json!({ "status": "ok" })) }))
            .route(
                "/requests/check",
                post({
                    let seen = seen.clone();
                    move |Json(body): Json<Value>| {
                        let seen = seen.clone();
                        async move {
                            assert_eq!(body["action"], "job.open");
                            Json(json!({
                                "action": "job.open",
                                "blockers": seen.check_blockers,
                                "suggestions": []
                            }))
                        }
                    }
                }),
            )
            .route(
                "/intake/:chain/:tx",
                get({
                    let seen = seen.clone();
                    move |AxPath((chain, tx)): AxPath<(String, String)>| {
                        let seen = seen.clone();
                        async move {
                            assert_eq!(chain, "4663");
                            let n = seen.intake_reads.fetch_add(1, Ordering::SeqCst);
                            if n == 0 {
                                return (
                                    axum::http::StatusCode::NOT_FOUND,
                                    Json(json!({ "error": "unknown_transaction" })),
                                );
                            }
                            let state = seen.intake_ends;
                            let mut entry = json!({
                                "kind": "request",
                                "state": state,
                                "requestId": "0xreq",
                                "transactionHash": tx,
                                "reason": "body_refused",
                                "detail": "no such skill",
                            });
                            if state == "admitted" {
                                entry["admission"] =
                                    json!({ "result": { "kind": "job", "jobId": "JOB1" } });
                            }
                            (
                                axum::http::StatusCode::OK,
                                Json(json!({ "entries": [entry] })),
                            )
                        }
                    }
                }),
            )
            .route(
                "/jobs/:id",
                get({
                    let seen = seen.clone();
                    move |AxPath(id): AxPath<String>| {
                        let seen = seen.clone();
                        async move {
                            assert_eq!(id, "JOB1");
                            let n = seen.job_reads.fetch_add(1, Ordering::SeqCst);
                            let state = if n == 0 { "running" } else { seen.job_ends };
                            Json(json!({ "id": id, "state": state, "blockedReason": "planner gave up" }))
                        }
                    }
                }),
            )
            .route(
                "/jobs/:id/result",
                get({
                    let hash = report_hash.clone();
                    let base = base.clone();
                    move |AxPath(id): AxPath<String>| {
                        let hash = hash.clone();
                        let base = base.clone();
                        async move {
                            Json(json!({
                                "jobId": id, "state": "completed", "complete": true,
                                "files": [{
                                    "name": "report", "path": "artifacts/report.md",
                                    "mediaType": "text/markdown", "hash": hash,
                                    "bytes": report_body().len(),
                                    "url": format!("{base}/artifacts/{hash}")
                                }]
                            }))
                        }
                    }
                }),
            )
            .route(
                "/jobs/:id/report.md",
                get(|| async { (axum::http::StatusCode::NOT_FOUND, "no audit") }),
            )
            .route(
                "/artifacts/:hash",
                get(|AxPath(hash): AxPath<String>| async move {
                    assert_eq!(hash, sha256_hex(report_body().as_bytes()));
                    report_body()
                }),
            )
            .route(
                "/oracle/requests/:id/attestation",
                get(|| async {
                    Json(json!({ "primaryType": "OracleAttestation", "signature": "0xsig" }))
                }),
            )
    }

    async fn serve(make: impl FnOnce(String) -> Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let router = make(base.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        base
    }

    async fn rig(seen: Arc<Seen>, book: Arc<OrderBook>) -> ImdExecutor {
        let rpc_seen = seen.clone();
        let rpc_url = serve(move |_| {
            Router::new()
                .route("/", post(rpc_handler))
                .with_state(rpc_seen)
        })
        .await;
        let plane_seen = seen.clone();
        // The result's artifact URL points back at the mock plane.
        let plane_url = serve(move |base| plane_router(plane_seen, base)).await;
        let config = ImdConfig {
            api_url: plane_url.clone(),
            explorer_url: "https://explorer.example".into(),
            rpc_url: format!("{rpc_url}/"),
            chain_id: 4663,
            intake: Address::parse(INTAKE_V2).unwrap(),
            token: Address::parse(default_token_for_chain(4663).unwrap()).unwrap(),
            key_hex: KEY.into(),
            price_wei: DEFAULT_PRICE_WEI,
            fixed_price_usd: Some(7.0),
            price_source_url: String::new(),
            max_cost_micro_usdc: 4_500_000,
            min_margin_micro_usdc: 500_000,
            min_deadline: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
            tx_timeout: Duration::from_secs(5),
            actions: vec![
                "job.open".into(),
                "job.continue".into(),
                "oracle.request".into(),
            ],
            max_inline_bytes: 1 << 20,
            min_gas_wei: 1,
            orders_path: PathBuf::from("/dev/null"),
        };
        let signer = Signer::from_hex(KEY).unwrap();
        ImdExecutor::with_parts(config, signer, book)
    }

    fn research_job(price: u64) -> JobEnvelopePayload {
        let order = SwarmOrder::new(
            "job.open",
            json!({
                "objective": "Write a sourced report comparing deterministic EVM test approaches.",
                "skill": "research-report",
                "outputs": [{ "name": "report", "path": "artifacts/report.md", "mediaType": "text/markdown" }],
                "github": false
            }),
        );
        JobEnvelopePayload {
            job_id: Uuid::new_v4(),
            buyer: AgentId::new("buyer@swarm", [7u8; 32]),
            kind: JobKind::BatchJob,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: Some(SWARM_MODEL.into()),
                kind: JobKind::BatchJob,
                max_duration_secs: 7_200,
                min_reputation_bps: None,
            },
            input: swarm_input(&order),
            price_micro_usdc: price,
            deadline_ms: 7_200_000,
            idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "swarm-test"),
            issued_at_ms: 0,
            referral_code: None,
            stream: false,
        }
    }

    fn methods(seen: &Seen) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for m in seen.rpc_methods.lock().iter() {
            *counts.entry(m.clone()).or_insert(0) += 1;
        }
        counts
    }

    #[tokio::test]
    async fn a_research_job_is_paid_once_followed_and_relayed() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "admitted",
            ..Default::default()
        });
        let book = Arc::new(OrderBook::in_memory());
        let exec = rig(seen.clone(), book.clone()).await;
        let job = research_job(5_000_000);
        exec.health().await.expect("the rig is healthy");

        let outcome = exec
            .execute(&job, Duration::from_secs(30))
            .await
            .expect("the swarm job completes");
        let Content::Json { value } = &outcome.output[0] else {
            panic!("a JSON block leads the output");
        };
        assert_eq!(value["swarm"]["jobId"], "JOB1");
        assert_eq!(value["swarm"]["paidWith"]["chainId"], 4663);
        assert_eq!(value["outcome"]["state"], "completed");
        assert_eq!(value["outcome"]["files"][0]["content"], report_body());
        assert!(matches!(&outcome.output[1], Content::Text { text } if text.contains("JOB1")));

        let counts = methods(&seen);
        assert_eq!(counts["eth_sendRawTransaction"], 1, "paid exactly once");
        assert!(!counts.contains_key("eth_sendRawTransaction_approve"));
        // The allowance was already there, so no approve transaction.
        assert_eq!(counts["eth_estimateGas"], 1);
        let order = book.find(job.job_id).unwrap();
        assert_eq!(order.state, OrderState::Completed);
        assert_eq!(order.imd_job_id.as_deref(), Some("JOB1"));
        assert_eq!(order.buyer, "buyer@swarm");
    }

    #[tokio::test]
    async fn a_missing_allowance_is_approved_first() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "admitted",
            allowance_zero: true,
            ..Default::default()
        });
        let book = Arc::new(OrderBook::in_memory());
        let exec = rig(seen.clone(), book).await;
        exec.execute(&research_job(5_000_000), Duration::from_secs(30))
            .await
            .expect("completes after approving");
        let counts = methods(&seen);
        assert_eq!(counts["eth_sendRawTransaction"], 2, "approve, then request");
    }

    #[tokio::test]
    async fn a_recovered_job_resumes_its_order_instead_of_paying_again() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "admitted",
            ..Default::default()
        });
        let book = Arc::new(OrderBook::in_memory());
        let exec = rig(seen.clone(), book.clone()).await;
        let job = research_job(5_000_000);
        let order = exec.parse_order(&job).unwrap();
        // A previous life paid and recorded the admitted order, then died.
        book.record(ImdOrder {
            job_id: job.job_id,
            buyer: job.buyer.display.clone(),
            action: "job.open".into(),
            body_sha256: sha256_hex(&order.body),
            state: OrderState::Admitted,
            nonce: Some(5),
            tx_hash: Some("0xabc".into()),
            raw_tx: Some("0x02".into()),
            request_id: Some("0xreq".into()),
            imd_job_id: Some("JOB1".into()),
            oracle_request_id: None,
            note: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        })
        .unwrap();
        exec.execute(&job, Duration::from_secs(30))
            .await
            .expect("resumes to completion");
        let counts = methods(&seen);
        assert!(
            !counts.contains_key("eth_sendRawTransaction"),
            "nothing was broadcast: {counts:?}"
        );
        assert_eq!(
            seen.intake_reads.load(Ordering::SeqCst),
            0,
            "admission was known"
        );
    }

    #[tokio::test]
    async fn a_prepared_but_unbroadcast_payment_is_sent_once_on_recovery() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "admitted",
            ..Default::default()
        });
        let book = Arc::new(OrderBook::in_memory());
        let exec = rig(seen.clone(), book.clone()).await;
        let job = research_job(5_000_000);
        let order = exec.parse_order(&job).unwrap();
        book.record(ImdOrder {
            job_id: job.job_id,
            buyer: job.buyer.display.clone(),
            action: "job.open".into(),
            body_sha256: sha256_hex(&order.body),
            state: OrderState::Prepared,
            nonce: Some(5),
            tx_hash: Some("0xabc".into()),
            raw_tx: Some("0x02ff".into()),
            request_id: None,
            imd_job_id: None,
            oracle_request_id: None,
            note: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        })
        .unwrap();
        exec.execute(&job, Duration::from_secs(30)).await.unwrap();
        let counts = methods(&seen);
        assert_eq!(
            counts["eth_sendRawTransaction"], 1,
            "the stored bytes went out once"
        );
        assert!(
            !counts.contains_key("eth_estimateGas"),
            "nothing was re-signed"
        );
    }

    #[tokio::test]
    async fn a_refused_order_fails_the_job_and_is_remembered() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "refused",
            ..Default::default()
        });
        let book = Arc::new(OrderBook::in_memory());
        let exec = rig(seen, book.clone()).await;
        let job = research_job(5_000_000);
        let err = exec
            .execute(&job, Duration::from_secs(30))
            .await
            .expect_err("a refusal fails the job");
        assert!(err.to_string().contains("body_refused"), "{err}");
        assert_eq!(book.find(job.job_id).unwrap().state, OrderState::Refused);
        // Re-executing never pays again.
        let err = exec
            .execute(&job, Duration::from_secs(30))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already ended"), "{err}");
    }

    #[tokio::test]
    async fn a_blocked_job_fails_without_a_result() {
        let seen = Arc::new(Seen {
            job_ends: "blocked",
            intake_ends: "admitted",
            ..Default::default()
        });
        let book = Arc::new(OrderBook::in_memory());
        let exec = rig(seen, book.clone()).await;
        let job = research_job(5_000_000);
        let err = exec
            .execute(&job, Duration::from_secs(30))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("blocked"), "{err}");
        assert!(err.to_string().contains("planner gave up"), "{err}");
        assert_eq!(book.find(job.job_id).unwrap().state, OrderState::Failed);
    }

    #[tokio::test]
    async fn the_free_check_runs_before_any_payment() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "admitted",
            check_blockers: vec!["skill_unknown"],
            ..Default::default()
        });
        let exec = rig(seen.clone(), Arc::new(OrderBook::in_memory())).await;
        let err = exec
            .execute(&research_job(5_000_000), Duration::from_secs(30))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("skill_unknown"), "{err}");
        assert!(
            !methods(&seen).contains_key("eth_sendRawTransaction"),
            "nothing was paid"
        );
    }

    #[tokio::test]
    async fn admission_refuses_what_the_node_will_not_sell_or_serve() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "admitted",
            ..Default::default()
        });
        let exec = rig(seen, Arc::new(OrderBook::in_memory())).await;
        let job = research_job(5_000_000);
        // No price known yet: refused until the feed is read.
        let err = exec.admit(&job, Duration::from_secs(3_600)).unwrap_err();
        assert!(err.contains("price is not known"), "{err}");
        exec.health().await.unwrap();
        exec.admit(&job, Duration::from_secs(3_600))
            .expect("a sound offer admits");

        // At $7 one action costs 3,500,000; plus the 500,000 margin.
        let err = exec
            .admit(&research_job(3_999_999), Duration::from_secs(3_600))
            .unwrap_err();
        assert!(err.contains("below the live cost"), "{err}");
        exec.admit(&research_job(4_000_000), Duration::from_secs(3_600))
            .unwrap();

        let err = exec.admit(&job, Duration::from_millis(10)).unwrap_err();
        assert!(err.contains("floor"), "{err}");

        let mut plain = job.clone();
        plain.capability_requirement.model_id = None;
        let err = exec.admit(&plain, Duration::from_secs(3_600)).unwrap_err();
        assert!(err.contains("imd-swarm"), "{err}");

        let mut shell = job.clone();
        shell.input = vec![Content::text("rm -rf /")];
        let err = exec.admit(&shell, Duration::from_secs(3_600)).unwrap_err();
        assert!(err.contains("no swarm order"), "{err}");

        let mut launch = job.clone();
        launch.input = swarm_input(&SwarmOrder::new("launch.open", json!({"objective": "x"})));
        let err = exec.admit(&launch, Duration::from_secs(3_600)).unwrap_err();
        assert!(err.contains("not sold"), "{err}");

        let mut cont = job.clone();
        cont.input = swarm_input(&SwarmOrder::new(
            "job.continue",
            json!({"parentJobId": "SOMEONE-ELSES", "objective": "more", "skill": "build-website"}),
        ));
        let err = exec.admit(&cont, Duration::from_secs(3_600)).unwrap_err();
        assert!(err.contains("this buyer"), "{err}");
    }

    #[tokio::test]
    async fn an_oracle_order_returns_the_attestation() {
        let seen = Arc::new(Seen {
            job_ends: "completed",
            intake_ends: "admitted",
            ..Default::default()
        });
        let book = Arc::new(OrderBook::in_memory());
        let exec = rig(seen, book.clone()).await;
        let mut job = research_job(5_000_000);
        job.input = swarm_input(&SwarmOrder::new(
            "oracle.request",
            json!({
                "v": 1, "question": "Did the pool trade above 1 ETH?", "chainId": 1,
                "window": {"hours": 24}, "answerType": "bool", "evidence": "panel",
                "panelSize": 5, "quorum": 4, "validForSeconds": 86400
            }),
        ));
        let order = exec.parse_order(&job).unwrap();
        book.record(ImdOrder {
            job_id: job.job_id,
            buyer: job.buyer.display.clone(),
            action: "oracle.request".into(),
            body_sha256: sha256_hex(&order.body),
            state: OrderState::Admitted,
            nonce: Some(5),
            tx_hash: Some("0xabc".into()),
            raw_tx: Some("0x02".into()),
            request_id: Some("0xreq".into()),
            imd_job_id: None,
            oracle_request_id: Some("ORACLE1".into()),
            note: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        })
        .unwrap();
        let outcome = exec.execute(&job, Duration::from_secs(30)).await.unwrap();
        let Content::Json { value } = &outcome.output[0] else {
            panic!("json first");
        };
        assert_eq!(value["outcome"]["state"], "attested");
        assert_eq!(value["outcome"]["attestation"]["signature"], "0xsig");
        assert_eq!(value["swarm"]["oracleRequestId"], "ORACLE1");
    }

    /// Pays the real Intake bytecode on a forked chain. Needs an anvil
    /// fork (`anvil --fork-url <rpc> --chain-id 4663 --hardfork shanghai`)
    /// with a funded wallet: COVENANT_COMPUTE_IMD_FORK_RPC names the fork,
    /// COVENANT_COMPUTE_IMD_FORK_KEY_FILE the wallet's key file. Proves the
    /// signing, the ABI words and the gas against the contract as deployed;
    /// the control plane never sees a fork transaction, so the order stops
    /// at the mined payment. Run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "needs an anvil fork of Robinhood Chain and a funded wallet"]
    async fn fork_rehearsal_pays_the_real_intake() {
        let rpc_url = std::env::var("COVENANT_COMPUTE_IMD_FORK_RPC")
            .unwrap_or_else(|_| "http://127.0.0.1:8599".into());
        let key_file = std::env::var("COVENANT_COMPUTE_IMD_FORK_KEY_FILE")
            .expect("COVENANT_COMPUTE_IMD_FORK_KEY_FILE names the wallet key file");
        let key_hex = std::fs::read_to_string(key_file)
            .unwrap()
            .trim()
            .to_string();
        let config = ImdConfig {
            api_url: "http://127.0.0.1:9".into(),
            explorer_url: DEFAULT_EXPLORER_URL.into(),
            rpc_url,
            chain_id: 4663,
            intake: Address::parse(INTAKE_V2).unwrap(),
            token: Address::parse(default_token_for_chain(4663).unwrap()).unwrap(),
            key_hex: key_hex.clone(),
            price_wei: DEFAULT_PRICE_WEI,
            fixed_price_usd: Some(7.0),
            price_source_url: String::new(),
            max_cost_micro_usdc: 4_500_000,
            min_margin_micro_usdc: 500_000,
            min_deadline: Duration::from_secs(1),
            poll_interval: Duration::from_millis(200),
            tx_timeout: Duration::from_secs(60),
            actions: vec!["job.open".into()],
            max_inline_bytes: 1 << 20,
            min_gas_wei: 1,
            orders_path: PathBuf::from("/dev/null"),
        };
        let exec = ImdExecutor::with_parts(
            config,
            Signer::from_hex(&key_hex).unwrap(),
            Arc::new(OrderBook::in_memory()),
        );
        let wallet = exec.wallet();
        let before = exec
            .token_balance()
            .await
            .expect("balance reads on the fork");
        assert!(before >= DEFAULT_PRICE_WEI, "fund {wallet} with IMD first");
        let until = Instant::now() + Duration::from_secs(120);
        exec.ensure_allowance(until)
            .await
            .expect("the approval mines");
        assert!(exec.allowance().await.unwrap() >= DEFAULT_PRICE_WEI);

        let order = SwarmOrder::new(
            "job.open",
            json!({
                "objective": "Rehearsal only: a one-line note on deterministic EVM testing.",
                "skill": "research-report",
                "outputs": [{"name": "report", "path": "artifacts/report.md", "mediaType": "text/markdown"}],
                "github": false
            }),
        );
        let body = order.compact_body().unwrap();
        let data = encode_intake_request(
            bytes32_string(order.action_id().unwrap()).unwrap(),
            &body,
            (Address::ZERO, [0u8; 4]),
            exec.config.token,
            DEFAULT_PRICE_WEI,
        );
        let (nonce, signed) = exec
            .sign_call(exec.config.intake, data)
            .await
            .expect("the request estimates and signs against the real bytecode");
        exec.broadcast(&signed.raw_hex(), &signed.hash_hex())
            .await
            .expect("broadcast");
        exec.wait_mined(&signed.hash_hex(), until)
            .await
            .expect("the request mines with status 1");
        let after = exec.token_balance().await.unwrap();
        assert_eq!(
            before - after,
            DEFAULT_PRICE_WEI,
            "exactly 0.5 IMD left the wallet"
        );
        println!(
            "FORK REHEARSAL OK wallet={wallet} nonce={nonce} tx={} body_bytes={}",
            signed.hash_hex(),
            body.len()
        );
    }

    /// One real job through the executor on the live chain and IMD's live
    /// plane: check, pay 0.5 IMD, follow, relay. Configured exactly as a
    /// node is (`COVENANT_COMPUTE_IMD_*`), plus COVENANT_COMPUTE_IMD_LIVE_HOME
    /// for the order book, COVENANT_COMPUTE_IMD_LIVE_JOB_ID (reuse it to
    /// resume the same order rather than pay again),
    /// COVENANT_COMPUTE_IMD_LIVE_ORDER (a JSON file of `{action, input}`) and
    /// COVENANT_COMPUTE_IMD_LIVE_OUT (where the relayed output is written).
    /// Spends real IMD. Run with `cargo test -- --ignored live_job`.
    #[tokio::test]
    #[ignore = "pays 0.5 IMD on the live chain"]
    async fn live_job_through_the_real_imd() {
        let var = |key: &str| std::env::var(key).unwrap_or_else(|_| panic!("{key} is required"));
        let home = PathBuf::from(var("COVENANT_COMPUTE_IMD_LIVE_HOME"));
        let job_id = Uuid::parse_str(&var("COVENANT_COMPUTE_IMD_LIVE_JOB_ID")).unwrap();
        let order: Value = serde_json::from_str(
            &std::fs::read_to_string(var("COVENANT_COMPUTE_IMD_LIVE_ORDER")).unwrap(),
        )
        .unwrap();
        let out = PathBuf::from(var("COVENANT_COMPUTE_IMD_LIVE_OUT"));

        let config = ImdConfig::from_environment(&home).expect("the knobs parse");
        let exec = ImdExecutor::new(config).expect("the executor opens");
        exec.health().await.expect("the node would report online");

        let order = SwarmOrder::new(order["action"].as_str().unwrap(), order["input"].clone());
        let mut job = research_job(5_000_000);
        job.job_id = job_id;
        job.input = swarm_input(&order);
        let deadline = Duration::from_secs(3 * 3_600);
        exec.admit(&job, deadline).expect("the node admits the job");
        println!(
            "LIVE wallet={} job={job_id} least_offer={:?}",
            exec.wallet(),
            exec.least_offer_micro_usdc()
        );

        let outcome = exec
            .execute(&job, deadline)
            .await
            .expect("the job completes");
        let Content::Json { value } = &outcome.output[0] else {
            panic!("a JSON block leads the output");
        };
        std::fs::write(&out, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        println!(
            "LIVE OK wall_ms={} tx={} job={} -> {}",
            outcome.wall_ms,
            value["swarm"]["paidWith"]["txHash"],
            value["swarm"]["jobId"],
            out.display()
        );
    }

    #[test]
    fn the_order_book_replays_the_last_state_per_job() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("imd-orders.jsonl");
        let book = OrderBook::open(&path).unwrap();
        assert!(book.is_empty());
        let job_id = Uuid::new_v4();
        let mut order = ImdOrder {
            job_id,
            buyer: "b".into(),
            action: "job.open".into(),
            body_sha256: "h".into(),
            state: OrderState::Prepared,
            nonce: Some(1),
            tx_hash: Some("0x1".into()),
            raw_tx: Some("0x02".into()),
            request_id: None,
            imd_job_id: None,
            oracle_request_id: None,
            note: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        book.record(order.clone()).unwrap();
        order.state = OrderState::Admitted;
        order.imd_job_id = Some("J".into());
        book.record(order.clone()).unwrap();
        drop(book);
        // A torn final line is dropped, not fatal.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"job_id\":\"torn").unwrap();
        }
        let book = OrderBook::open(&path).unwrap();
        assert_eq!(book.len(), 1);
        let found = book.find(job_id).unwrap();
        assert_eq!(found.state, OrderState::Admitted);
        assert!(book.owns_imd_job("b", "J"));
        assert!(!book.owns_imd_job("someone", "J"));
    }

    #[test]
    fn the_price_feed_picks_the_deepest_pool_on_the_chain() {
        let feed = json!({ "pairs": [
            { "chainId": "ethereum", "priceUsd": "7.05", "liquidity": { "usd": 2000000.0 } },
            { "chainId": "robinhood", "priceUsd": "6.92", "liquidity": { "usd": 350000.0 } },
            { "chainId": "robinhood", "priceUsd": "7.75", "liquidity": { "usd": 139.0 } },
        ]});
        assert_eq!(parse_price_feed(&feed, Some("robinhood")), Some(6.92));
        assert_eq!(parse_price_feed(&feed, Some("ethereum")), Some(7.05));
        assert_eq!(parse_price_feed(&feed, None), Some(7.05));
        assert_eq!(parse_price_feed(&feed, Some("base")), None);
        assert_eq!(parse_price_feed(&json!({}), None), None);
        assert_eq!(
            oracle_id_from_url("https://api.imd.fun/oracle/requests/abc-1/attestation"),
            Some("abc-1".into())
        );
        assert_eq!(oracle_id_from_url("/jobs/x"), None);
    }

    #[test]
    fn the_cost_follows_the_price_and_the_configured_amount() {
        let config = ImdConfig {
            api_url: String::new(),
            explorer_url: String::new(),
            rpc_url: String::new(),
            chain_id: 4663,
            intake: Address::ZERO,
            token: Address::ZERO,
            key_hex: KEY.into(),
            price_wei: DEFAULT_PRICE_WEI,
            fixed_price_usd: None,
            price_source_url: String::new(),
            max_cost_micro_usdc: 4_500_000,
            min_margin_micro_usdc: 500_000,
            min_deadline: Duration::from_secs(1),
            poll_interval: Duration::from_secs(1),
            tx_timeout: Duration::from_secs(1),
            actions: vec!["job.open".into()],
            max_inline_bytes: 1,
            min_gas_wei: 1,
            orders_path: PathBuf::from("/dev/null"),
        };
        let exec = ImdExecutor::with_parts(
            config,
            Signer::from_hex(KEY).unwrap(),
            Arc::new(OrderBook::in_memory()),
        );
        assert_eq!(exec.cost_at(7.0), 3_500_000);
        assert_eq!(exec.cost_at(7.05), 3_525_000);
        assert_eq!(exec.cached_cost_micro_usdc(), None);
        assert_eq!(exec.least_offer_micro_usdc(), None);
    }
}
