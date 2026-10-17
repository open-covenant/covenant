//! The daemon's tokenized-equity trading leg: the bounded-trade capability an
//! agent reaches over IPC.
//!
//! [`crate::spend_grant`] bounds what an agent *pays*. This bounds what it
//! *trades*, which is a different question: an order can sit inside every spend
//! limit and still be filled at a price nobody would accept, against a feed that
//! last printed on Friday, for the hundredth time today.
//!
//! Two halves hold the same line. `RwaTradeGuard` is the on-chain one, and a
//! trade that breaks a bound reverts there before it reaches the venue. This
//! module is the daemon's half: it reads the same live context the guard reads,
//! runs the same policy through [`covenant_rwa_firewall`], and refuses locally
//! with a named reason rather than paying gas to be told no. When the two
//! disagree the chain wins, because it is the one holding the money.
//!
//! Fail-closed throughout. A surface that is enabled but missing an address, an
//! RPC, or a key resolves to `None` and the capability is simply absent, rather
//! than booting half-wired and refusing at some later, less obvious point.

use std::path::Path;

use covenant_rwa_firewall::{AssetContext, RwaDenial, RwaPolicy, RwaTrade, Side as PolicySide};
use covenant_x402::evm_tx::{EvmRpc, EvmTxError, EvmTxSigner, TxReceipt, TxRequest};

/// `buy(address,uint256,uint256,uint256,address,bytes)`
const SEL_BUY: [u8; 4] = [0x44, 0xbf, 0x13, 0x26];
/// `sell(address,uint256,uint256,uint256,address,bytes)`
const SEL_SELL: [u8; 4] = [0x4f, 0xc4, 0xb2, 0xc8];
/// `checkTrade(address,uint256,uint256)`
const SEL_CHECK_TRADE: [u8; 4] = [0x79, 0xf3, 0xf7, 0x5d];
/// `assetConfig(address)`
const SEL_ASSET_CONFIG: [u8; 4] = [0xd6, 0xdb, 0xaf, 0x58];
/// `uiMultiplier()`
const SEL_UI_MULTIPLIER: [u8; 4] = [0xa6, 0x0b, 0xf1, 0x3d];
/// `oraclePaused()`
const SEL_ORACLE_PAUSED: [u8; 4] = [0x77, 0x06, 0xba, 0x52];
/// `latestRoundData()`
const SEL_LATEST_ROUND: [u8; 4] = [0xfe, 0xaf, 0x96, 0x8c];

#[derive(Debug, thiserror::Error)]
pub enum RwaError {
    #[error("the trading surface has no in-process submitter")]
    NoSubmitter,
    #[error("the trading surface has no rpc endpoint")]
    NoRpc,
    #[error("submitter key rejected: {0}")]
    SubmitterKey(String),
    #[error("the asset {0} is not registered on the guard")]
    AssetNotRegistered(String),
    #[error("policy refused the trade: {0}")]
    Refused(#[from] RwaDenial),
    #[error("rpc: {0}")]
    Rpc(#[from] EvmTxError),
    #[error("{0}")]
    Decode(String),
}

/// Which way the trade runs. `Buy` spends the settlement asset for the Stock
/// Token; `Sell` is the same trade in reverse under the same bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    fn selector(self) -> [u8; 4] {
        match self {
            Side::Buy => SEL_BUY,
            Side::Sell => SEL_SELL,
        }
    }

    fn as_policy(self) -> PolicySide {
        match self {
            Side::Buy => PolicySide::Buy,
            Side::Sell => PolicySide::Sell,
        }
    }
}

/// A trade as the caller proposes it, before the daemon judges it.
#[derive(Debug, Clone)]
pub struct TradeRequest {
    pub asset: [u8; 20],
    pub side: Side,
    /// What the executor may pull: settlement-asset units on a buy, Stock Token
    /// units on a sell.
    pub max_in: u128,
    /// The floor the fill has to clear, in the other asset's units. This is also
    /// the size the guard judges, so a caller cannot bound the trade loosely and
    /// commit to something larger.
    pub min_out: u128,
    /// The worst-case price per whole token the caller commits to, USD*1e8.
    pub quoted_price_usd_e8: u128,
    pub router: [u8; 20],
    /// Venue calldata built off-chain for the allowlisted router.
    pub swap_data: Vec<u8>,
}

impl TradeRequest {
    /// The size the guard prices: the amount committed on the side that is
    /// bounded. A buy is judged on the tokens it will receive, a sell on the
    /// tokens it gives up.
    fn judged_amount(&self) -> u128 {
        match self.side {
            Side::Buy => self.min_out,
            Side::Sell => self.max_in,
        }
    }
}

/// The live asset context both halves read, and the bounds the guard is
/// configured with, fetched together so the daemon judges exactly what the
/// contract will.
#[derive(Debug, Clone, Copy)]
pub struct LiveContext {
    pub policy: RwaPolicy,
    pub context: AssetContext,
}

/// Signs and pays gas for the trades the daemon decides to let through. The
/// guarantees ride the guard, not this key: a compromised submitter can waste
/// gas but cannot land a trade the guard refuses.
pub struct RwaSubmitter {
    signer: EvmTxSigner,
}

impl RwaSubmitter {
    pub fn new(secret: &[u8; 32]) -> Result<Self, RwaError> {
        let signer = EvmTxSigner::from_secret_bytes(secret)
            .map_err(|e| RwaError::SubmitterKey(e.to_string()))?;
        Ok(Self { signer })
    }

    pub fn address(&self) -> [u8; 20] {
        self.signer.address()
    }
}

/// Boot config for the trading leg: the deployed guard and executor on one
/// chain, and optionally the key that pays to land a trade.
pub struct RwaConfig {
    chain_id: u64,
    guard: [u8; 20],
    executor: [u8; 20],
    /// Reads. Required for anything that judges a trade, since both halves have
    /// to see the same live context; a config without one can still build
    /// calldata, which is what the offline tests exercise.
    rpc: Option<EvmRpc>,
    /// Writes. A preview needs no key, so an operator who only wants the verdict
    /// surface can run this leg without a funded address anywhere near it.
    submitter: Option<RwaSubmitter>,
}

impl RwaConfig {
    pub fn new(chain_id: u64, guard: [u8; 20], executor: [u8; 20]) -> Self {
        Self {
            chain_id,
            guard,
            executor,
            rpc: None,
            submitter: None,
        }
    }

    /// Attach the endpoint the leg reads the chain through.
    pub fn with_rpc(mut self, rpc_url: impl Into<String>) -> Self {
        self.rpc = Some(EvmRpc::new(rpc_url, self.chain_id));
        self
    }

    /// Attach the key that pays to land a trade. Rejected without an RPC, since
    /// a submitter that cannot read cannot judge, and this leg never signs a
    /// trade it has not judged.
    pub fn with_submitter(mut self, submitter: RwaSubmitter) -> Result<Self, RwaError> {
        if self.rpc.is_none() {
            return Err(RwaError::NoRpc);
        }
        self.submitter = Some(submitter);
        Ok(self)
    }

    /// Read the surface off the environment. Off by default; enabled but
    /// incomplete resolves to `None` rather than booting half-wired.
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("COVENANT_RWA_ENABLED")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        let guard = match decode_address(&require_env("COVENANT_RWA_GUARD")?) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "rwa guard address invalid; surface off");
                return None;
            }
        };
        let executor = match decode_address(&require_env("COVENANT_RWA_EXECUTOR")?) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "rwa executor address invalid; surface off");
                return None;
            }
        };
        let chain_id = match require_env("COVENANT_RWA_CHAIN")?.trim().parse::<u64>() {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("rwa chain id is not a u64; surface off");
                return None;
            }
        };
        let Some(rpc_url) = std::env::var("COVENANT_RWA_RPC")
            .ok()
            .filter(|v| !v.trim().is_empty())
        else {
            tracing::warn!("rwa rpc unset; surface off (the trading leg has to read the chain)");
            return None;
        };
        let config = Self::new(chain_id, guard, executor).with_rpc(rpc_url);
        // No key is the preview-only surface, which is a legitimate way to run
        // this: an agent can ask what the guard would say without the daemon
        // holding anything that can spend.
        let Some(secret) = load_secret("COVENANT_RWA_SUBMITTER_KEY") else {
            tracing::info!("rwa submitter key unset; trading leg is preview-only");
            return Some(config);
        };
        match RwaSubmitter::new(&secret).and_then(|s| config.with_submitter(s)) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "rwa submitter rejected; surface off");
                None
            }
        }
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn guard(&self) -> [u8; 20] {
        self.guard
    }

    pub fn executor(&self) -> [u8; 20] {
        self.executor
    }

    pub fn has_submitter(&self) -> bool {
        self.submitter.is_some()
    }

    pub fn submitter_address(&self) -> Option<[u8; 20]> {
        self.submitter.as_ref().map(RwaSubmitter::address)
    }

    /// Read the guard's bounds for an asset and the live oracle context behind
    /// it, so the daemon judges the trade against exactly what the contract
    /// will. `now` is supplied rather than read so the caller can pin it.
    pub async fn live_context(&self, asset: [u8; 20], now: u64) -> Result<LiveContext, RwaError> {
        let rpc = self.rpc.as_ref().ok_or(RwaError::NoRpc)?;

        let cfg = rpc
            .eth_call(&self.guard, &with_address(SEL_ASSET_CONFIG, &asset))
            .await?;
        if cfg.len() < 128 {
            return Err(RwaError::Decode(format!(
                "assetConfig returned {} bytes, want 128",
                cfg.len()
            )));
        }
        let feed = address_at(&cfg, 0);
        if feed == [0u8; 20] {
            return Err(RwaError::AssetNotRegistered(hex_addr(&asset)));
        }
        let policy = RwaPolicy {
            per_trade_notional_cap_usd_e8: word_at(&cfg, 1),
            fair_value_band_bps: word_at(&cfg, 2) as u32,
            max_feed_staleness_secs: word_at(&cfg, 3) as u64,
            // The calendar lives off-chain and the guard has no equivalent, so
            // the daemon holds it open and lets staleness speak for both halves.
            require_market_hours: false,
        };

        let multiplier = word_at(&rpc.eth_call(&asset, &SEL_UI_MULTIPLIER).await?, 0);
        let paused = word_at(&rpc.eth_call(&asset, &SEL_ORACLE_PAUSED).await?, 0) != 0;
        let round = rpc.eth_call(&feed, &SEL_LATEST_ROUND).await?;
        if round.len() < 160 {
            return Err(RwaError::Decode(format!(
                "latestRoundData returned {} bytes, want 160",
                round.len()
            )));
        }

        Ok(LiveContext {
            policy,
            context: AssetContext {
                oracle_price_usd_e8: word_at(&round, 1),
                oracle_updated_at: word_at(&round, 3) as u64,
                ui_multiplier_e18: multiplier,
                oracle_paused: paused,
                now,
                market_open: true,
            },
        })
    }

    /// Judge a trade the way the guard will, without touching the chain's state.
    /// A refusal here costs nothing; the same refusal on chain costs gas.
    pub fn judge(&self, req: &TradeRequest, live: &LiveContext) -> Result<(), RwaError> {
        let trade = RwaTrade {
            asset: req.asset,
            side: req.side.as_policy(),
            raw_amount: req.judged_amount(),
            quoted_price_usd_e8: req.quoted_price_usd_e8,
        };
        live.policy.evaluate(&trade, &live.context)?;
        Ok(())
    }

    /// The executor call for a trade: `buy` or `sell`, ABI-encoded head and
    /// tail with the venue calldata in the tail.
    pub fn trade_calldata(&self, req: &TradeRequest) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 32 * 7 + req.swap_data.len());
        out.extend_from_slice(&req.side.selector());
        out.extend_from_slice(&word_from_address(&req.asset));
        out.extend_from_slice(&word_from_u128(req.max_in));
        out.extend_from_slice(&word_from_u128(req.min_out));
        out.extend_from_slice(&word_from_u128(req.quoted_price_usd_e8));
        out.extend_from_slice(&word_from_address(&req.router));
        // Offset to the bytes tail: six head words follow the selector.
        out.extend_from_slice(&word_from_u128(6 * 32));
        out.extend_from_slice(&word_from_u128(req.swap_data.len() as u128));
        out.extend_from_slice(&req.swap_data);
        let pad = (32 - req.swap_data.len() % 32) % 32;
        out.resize(out.len() + pad, 0u8);
        out
    }

    /// The read-only form of the guard's own check, for a caller that wants the
    /// verdict without committing to anything.
    pub async fn preview(&self, req: &TradeRequest) -> Result<(), RwaError> {
        let rpc = self.rpc.as_ref().ok_or(RwaError::NoRpc)?;
        let mut data = Vec::with_capacity(4 + 96);
        data.extend_from_slice(&SEL_CHECK_TRADE);
        data.extend_from_slice(&word_from_address(&req.asset));
        data.extend_from_slice(&word_from_u128(req.judged_amount()));
        data.extend_from_slice(&word_from_u128(req.quoted_price_usd_e8));
        rpc.eth_call(&self.guard, &data).await?;
        Ok(())
    }

    /// Judge the trade locally, then land it. The local pass is not the
    /// guarantee: the executor calls the guard again inside the transaction, so
    /// a trade that slips past this one still reverts on chain.
    pub async fn trade(&self, req: &TradeRequest, now: u64) -> Result<TxReceipt, RwaError> {
        let submitter = self.submitter.as_ref().ok_or(RwaError::NoSubmitter)?;
        let rpc = self.rpc.as_ref().ok_or(RwaError::NoRpc)?;
        let live = self.live_context(req.asset, now).await?;
        self.judge(req, &live)?;
        let calldata = self.trade_calldata(req);
        Ok(rpc
            .submit(&submitter.signer, &TxRequest::call(self.executor, calldata))
            .await?)
    }
}

fn require_env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => {
            tracing::warn!(key, "rwa surface enabled but {key} is unset; surface off");
            None
        }
    }
}

fn load_secret(key: &str) -> Option<[u8; 32]> {
    let raw = std::env::var(key).ok().filter(|v| !v.trim().is_empty())?;
    let trimmed = raw.trim();
    let hex = if trimmed.starts_with("0x") || trimmed.len() == 64 {
        trimmed.to_string()
    } else {
        std::fs::read_to_string(Path::new(trimmed))
            .ok()?
            .trim()
            .to_string()
    };
    let body = hex.strip_prefix("0x").unwrap_or(&hex);
    if body.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&body[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn decode_address(s: &str) -> Result<[u8; 20], String> {
    let body = s.trim().strip_prefix("0x").unwrap_or(s.trim());
    if body.len() != 40 {
        return Err(format!("want 20 hex bytes, got {:?}", s));
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&body[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("not hex: {s:?}"))?;
    }
    Ok(out)
}

fn hex_addr(a: &[u8; 20]) -> String {
    format!(
        "0x{}",
        a.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn with_address(selector: [u8; 4], addr: &[u8; 20]) -> Vec<u8> {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(&selector);
    out.extend_from_slice(&word_from_address(addr));
    out
}

fn word_from_address(addr: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(addr);
    w
}

fn word_from_u128(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

/// The low 16 bytes of word `index`, which is every quantity these contracts
/// return; the high half is zero for any value that fits a u128.
fn word_at(bytes: &[u8], index: usize) -> u128 {
    let start = index * 32 + 16;
    if bytes.len() < start + 16 {
        return 0;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&bytes[start..start + 16]);
    u128::from_be_bytes(buf)
}

fn address_at(bytes: &[u8], index: usize) -> [u8; 20] {
    let start = index * 32 + 12;
    let mut out = [0u8; 20];
    if bytes.len() >= start + 20 {
        out.copy_from_slice(&bytes[start..start + 20]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSET: [u8; 20] = [0xaf; 20];
    const ROUTER: [u8; 20] = [0x88; 20];
    const GUARD: [u8; 20] = [0x1c; 20];
    const EXECUTOR: [u8; 20] = [0xe9; 20];
    const WAD: u128 = 1_000_000_000_000_000_000;

    fn config() -> RwaConfig {
        RwaConfig::new(4663, GUARD, EXECUTOR)
    }

    fn request(side: Side) -> TradeRequest {
        TradeRequest {
            asset: ASSET,
            side,
            max_in: 300_000,
            min_out: WAD / 1000,
            quoted_price_usd_e8: 310_00000000,
            router: ROUTER,
            swap_data: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    fn live(price: u128, age: u64) -> LiveContext {
        let now = 1_800_000_000;
        LiveContext {
            policy: RwaPolicy {
                per_trade_notional_cap_usd_e8: 250 * 100_000_000,
                fair_value_band_bps: 50,
                max_feed_staleness_secs: 3600,
                require_market_hours: false,
            },
            context: AssetContext {
                oracle_price_usd_e8: price,
                oracle_updated_at: now - age,
                ui_multiplier_e18: WAD,
                oracle_paused: false,
                now,
                market_open: true,
            },
        }
    }

    // The encoding is what the executor decodes, so pin the whole shape: the
    // selector, six head words, then the length-prefixed padded tail.
    #[test]
    fn buy_calldata_matches_the_abi_layout() {
        let data = config().trade_calldata(&request(Side::Buy));
        assert_eq!(&data[..4], &SEL_BUY);
        assert_eq!(
            data.len(),
            4 + 32 * 7 + 32,
            "six head words, offset, length, padded tail"
        );
        assert_eq!(&data[4 + 12..4 + 32], &ASSET, "asset in the first word");
        assert_eq!(word_at(&data[4..], 1), 300_000, "max_in");
        assert_eq!(word_at(&data[4..], 2), WAD / 1000, "min_out");
        assert_eq!(word_at(&data[4..], 3), 310_00000000, "quoted price");
        assert_eq!(&data[4 + 4 * 32 + 12..4 + 5 * 32], &ROUTER, "router");
        assert_eq!(word_at(&data[4..], 5), 6 * 32, "offset to the bytes tail");
        assert_eq!(word_at(&data[4..], 6), 4, "swap data length");
        assert_eq!(&data[4 + 7 * 32..4 + 7 * 32 + 4], &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn sell_differs_from_buy_only_in_the_selector() {
        let buy = config().trade_calldata(&request(Side::Buy));
        let sell = config().trade_calldata(&request(Side::Sell));
        assert_eq!(&sell[..4], &SEL_SELL);
        assert_ne!(&buy[..4], &sell[..4]);
        assert_eq!(&buy[4..], &sell[4..], "same arguments either way");
    }

    #[test]
    fn empty_swap_data_needs_no_padding() {
        let mut req = request(Side::Buy);
        req.swap_data.clear();
        let data = config().trade_calldata(&req);
        assert_eq!(data.len(), 4 + 32 * 7, "head plus a zero-length tail");
    }

    // A buy is bounded by what it receives, a sell by what it gives up. Getting
    // this backwards would let a caller commit to more than the guard judged.
    #[test]
    fn the_judged_size_is_the_committed_side() {
        assert_eq!(request(Side::Buy).judged_amount(), WAD / 1000);
        assert_eq!(request(Side::Sell).judged_amount(), 300_000);
    }

    #[test]
    fn a_fair_trade_passes_the_local_judge() {
        assert!(config()
            .judge(&request(Side::Buy), &live(310_00000000, 60))
            .is_ok());
    }

    #[test]
    fn an_over_cap_trade_is_refused_before_gas() {
        let mut req = request(Side::Buy);
        req.min_out = 100 * WAD; // ~$31,000 against a $250 cap
        let err = config().judge(&req, &live(310_00000000, 60)).unwrap_err();
        assert!(
            matches!(err, RwaError::Refused(RwaDenial::NotionalOverCap { .. })),
            "got {err}"
        );
    }

    #[test]
    fn an_off_band_quote_is_refused_before_gas() {
        let mut req = request(Side::Buy);
        req.quoted_price_usd_e8 = 316_00000000; // ~2% off
        let err = config().judge(&req, &live(310_00000000, 60)).unwrap_err();
        assert!(
            matches!(err, RwaError::Refused(RwaDenial::PriceOutsideBand { .. })),
            "got {err}"
        );
    }

    #[test]
    fn a_stale_feed_is_refused_before_gas() {
        let err = config()
            .judge(&request(Side::Buy), &live(310_00000000, 7200))
            .unwrap_err();
        assert!(
            matches!(err, RwaError::Refused(RwaDenial::StalePriceFeed { .. })),
            "got {err}"
        );
    }

    #[test]
    fn a_config_without_a_submitter_broadcasts_nothing() {
        let cfg = config();
        assert!(!cfg.has_submitter());
        assert!(cfg.submitter_address().is_none());
    }

    // A key without an endpoint cannot judge, and this leg never signs a trade
    // it has not judged.
    #[test]
    fn a_submitter_without_an_rpc_is_rejected() {
        let submitter = RwaSubmitter::new(&[7u8; 32]).unwrap();
        match config().with_submitter(submitter) {
            Err(RwaError::NoRpc) => {}
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("a submitter without an rpc was accepted"),
        }
    }

    #[test]
    fn a_preview_only_config_holds_no_key() {
        let cfg = config().with_rpc("http://localhost:0");
        assert!(!cfg.has_submitter());
        assert!(cfg.submitter_address().is_none());
    }

    #[test]
    fn the_surface_is_off_unless_enabled() {
        // Absent COVENANT_RWA_ENABLED the surface never reads anything else.
        assert!(RwaConfig::from_env().is_none());
    }
}
