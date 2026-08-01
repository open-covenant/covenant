//! Daemon glue for the Robinhood governed-trading profile.
//!
//! The trading key never enters the daemon: request signing is delegated to the
//! `covenant-robinhood-signer` sidecar (the same isolation the metaplex and
//! x402 signers use). The daemon holds the receipt attestor key and the
//! configured policy. `GovernedTrader` enforces the policy before any order
//! reaches Robinhood, every decision becomes a signed receipt, and the receipt
//! root is anchored on-chain through the metaplex signer.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_metaplex::{AttestationPayload, MetaplexSigner, SignerRequest};
use covenant_robinhood::{
    ApprovalGate, ApprovalOutcome, Error as RhError, GovernedTrader, HttpTransport, OrderRequest,
    RequestSigner, Result as RhResult, RobinhoodClient, Side, SignRequest, SignedHeaders,
    SignedReceipt, TradeReceipt, TradingPolicy,
};
use ed25519_dalek::SigningKey;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;

pub struct TelegramConfig {
    pub token: String,
    pub chat_id: String,
}

/// Robinhood profile config, read from the daemon environment. `from_env`
/// returns `None` unless the full write surface is configured, so the daemon
/// constructs the profile only when it can actually trade.
pub struct RobinhoodConfig {
    pub signer_binary: PathBuf,
    pub api_key: String,
    pub keypair_path: String,
    pub base_url: Option<String>,
    pub policy: TradingPolicy,
    pub attestor: SigningKey,
    pub telegram: Option<TelegramConfig>,
}

impl RobinhoodConfig {
    pub fn from_env() -> Option<Self> {
        let signer_binary = std::env::var("COVENANT_ROBINHOOD_SIGNER_BIN").ok()?;
        let api_key = std::env::var("COVENANT_ROBINHOOD_API_KEY").ok()?;
        let keypair_path = std::env::var("COVENANT_ROBINHOOD_KEYPAIR").ok()?;
        let attestor = load_attestor(&std::env::var("COVENANT_ROBINHOOD_ATTESTOR_KEYPAIR").ok()?)?;
        let policy_json =
            std::fs::read_to_string(std::env::var("COVENANT_ROBINHOOD_POLICY").ok()?).ok()?;
        let policy: TradingPolicy = serde_json::from_str(&policy_json).ok()?;
        let telegram = match (
            std::env::var("COVENANT_ROBINHOOD_TELEGRAM_TOKEN"),
            std::env::var("COVENANT_ROBINHOOD_TELEGRAM_CHAT_ID"),
        ) {
            (Ok(token), Ok(chat_id)) if !token.is_empty() && !chat_id.is_empty() => {
                Some(TelegramConfig { token, chat_id })
            }
            _ => None,
        };
        Some(Self {
            signer_binary: PathBuf::from(signer_binary),
            api_key,
            keypair_path,
            base_url: std::env::var("COVENANT_ROBINHOOD_BASE_URL").ok(),
            policy,
            attestor,
            telegram,
        })
    }
}

fn load_attestor(path: &str) -> Option<SigningKey> {
    let raw = std::fs::read_to_string(path).ok()?;
    let bytes = STANDARD.decode(raw.trim()).ok()?;
    let seed: [u8; 32] = bytes.get(..32)?.try_into().ok()?;
    Some(SigningKey::from_bytes(&seed))
}

pub struct RobinhoodState {
    trader: GovernedTrader<HttpTransport>,
    metaplex: Option<Arc<dyn MetaplexSigner>>,
    receipts: Mutex<Vec<SignedReceipt>>,
}

impl RobinhoodState {
    pub fn new(config: RobinhoodConfig, metaplex: Option<Arc<dyn MetaplexSigner>>) -> Self {
        let signer = SidecarSigner {
            program: config.signer_binary,
            env: vec![
                ("COVENANT_ROBINHOOD_API_KEY".to_string(), config.api_key),
                (
                    "COVENANT_ROBINHOOD_KEYPAIR".to_string(),
                    config.keypair_path,
                ),
            ],
        };
        let mut client = RobinhoodClient::new(signer, HttpTransport::new());
        if let Some(base) = config.base_url {
            client = client.with_base_url(base);
        }
        let mut trader =
            GovernedTrader::new(client, config.policy, config.attestor, "robinhood-agent");
        if let Some(tg) = config.telegram {
            trader = trader.with_gate(Box::new(TelegramApprovalGate::new(tg)));
        }
        Self {
            trader,
            metaplex,
            receipts: Mutex::new(Vec::new()),
        }
    }

    pub fn trader(&self) -> &GovernedTrader<HttpTransport> {
        &self.trader
    }

    /// Anchor a receipt's root hash on-chain via the metaplex signer, returning
    /// the transaction signature. `None` when metaplex writes aren't wired.
    pub async fn anchor(&self, receipt: &SignedReceipt) -> Option<String> {
        let signer = self.metaplex.as_ref()?;
        let payload = AttestationPayload::new(
            receipt.root_hash_hex.clone(),
            "covenant",
            "robinhood",
            "trade",
            receipt.receipt.decided_at,
        );
        let request = SignerRequest::AttestAuditRoot {
            payload: Box::new(payload),
            asset: None,
            collection: None,
        };
        match signer.sign(request).await {
            Ok(resp) => Some(resp.signature),
            Err(e) => {
                tracing::warn!("robinhood on-chain anchor failed: {e}");
                None
            }
        }
    }

    pub async fn record(&self, receipt: SignedReceipt) {
        let mut log = self.receipts.lock().await;
        log.push(receipt);
        let len = log.len();
        if len > 1000 {
            log.drain(0..len - 1000);
        }
    }

    pub async fn recent(&self, limit: usize) -> Vec<SignedReceipt> {
        let log = self.receipts.lock().await;
        let start = log.len().saturating_sub(limit);
        log[start..].to_vec()
    }
}

pub fn string_list(args: &serde_json::Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_order(args: &serde_json::Value) -> Result<OrderRequest, String> {
    let symbol = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .ok_or("missing symbol")?
        .to_string();
    let side = match args.get("side").and_then(|v| v.as_str()) {
        Some("buy") => Side::Buy,
        Some("sell") => Side::Sell,
        _ => return Err("side must be buy or sell".into()),
    };
    let quantity = args
        .get("quantity")
        .and_then(|v| v.as_f64())
        .ok_or("missing quantity")?;
    match args
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("market")
    {
        "market" => Ok(OrderRequest::market(symbol, side, quantity)),
        "limit" => {
            let limit_price = args
                .get("limit_price")
                .and_then(|v| v.as_f64())
                .ok_or("limit order requires limit_price")?;
            Ok(OrderRequest::limit(symbol, side, quantity, limit_price))
        }
        other => Err(format!("unsupported order type: {other}")),
    }
}

/// A [`RequestSigner`] that delegates to the `covenant-robinhood-signer`
/// sidecar. The daemon spawns it per signed request with a cleared environment;
/// the trading key never enters the daemon's address space.
struct SidecarSigner {
    program: PathBuf,
    env: Vec<(String, String)>,
}

#[async_trait::async_trait]
impl RequestSigner for SidecarSigner {
    async fn signed_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
        timestamp: u64,
    ) -> RhResult<SignedHeaders> {
        let req = SignRequest {
            method: method.to_string(),
            path: path.to_string(),
            body: body.to_string(),
            timestamp: Some(timestamp),
        };
        let payload = serde_json::to_vec(&req)
            .map_err(|e| RhError::Transport(format!("encode SignRequest: {e}")))?;

        let mut child = Command::new(&self.program)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RhError::Transport(format!("spawn signer {:?}: {e}", self.program)))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| RhError::Transport("signer stdin unavailable".into()))?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|e| RhError::Transport(format!("write to signer: {e}")))?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| RhError::Transport(format!("await signer: {e}")))?;
        if !output.status.success() {
            return Err(RhError::Transport(format!(
                "signer exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| RhError::Decode(format!("signer stdout not utf-8: {e}")))?;
        serde_json::from_str::<SignedHeaders>(stdout.trim())
            .map_err(|e| RhError::Decode(format!("decode SignedHeaders: {e}")))
    }
}

/// Notifies a Telegram chat when an order trips the human-approval threshold and
/// holds it pending. Interactive approve-and-execute (a resolve round-trip) is a
/// tracked follow-up; until then a flagged order is never placed unattended.
struct TelegramApprovalGate {
    token: String,
    chat_id: String,
    http: reqwest::Client,
}

impl TelegramApprovalGate {
    fn new(cfg: TelegramConfig) -> Self {
        Self {
            token: cfg.token,
            chat_id: cfg.chat_id,
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl ApprovalGate for TelegramApprovalGate {
    async fn request(&self, receipt: &TradeReceipt) -> ApprovalOutcome {
        let notional = receipt
            .reference_price
            .map(|p| receipt.order.notional(p))
            .unwrap_or(0.0);
        let text = format!(
            "Covenant · order awaiting approval\n{} {:?} {:.6}  (~${:.2})\n{}",
            receipt.order.symbol,
            receipt.order.side,
            receipt.order.quantity(),
            notional,
            receipt.reason.clone().unwrap_or_default(),
        );
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        if let Err(e) = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "chat_id": self.chat_id, "text": text }))
            .send()
            .await
        {
            tracing::warn!("robinhood telegram approval notice failed: {e}");
        }
        ApprovalOutcome::Pending
    }
}
