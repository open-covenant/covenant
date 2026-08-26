//! Opt-in live proof of the deployed RWA trade guard on Robinhood Chain (4663).
//!
//!   cargo test -p covenant-rwa-firewall --test live_rh_rwa_guard -- --ignored live_
//!
//! The crate is the off-chain decision half and `RwaTradeGuard` is the on-chain
//! one. They are only worth anything if they refuse the same trades, so this
//! reads the *live* asset context off 4663 — the real AAPL Stock Token's
//! `uiMultiplier()` and `oraclePaused()`, the real Chainlink answer and its age —
//! and puts the same four trades through both:
//!
//!   1. a fair, in-cap buy;
//!   2. a buy far past the per-trade notional cap;
//!   3. a fill quoted well outside the oracle band;
//!   4. a token the guard never registered.
//!
//! Whatever the market is doing when this runs, the two halves must land on the
//! same verdict, named the same way. Outside market hours the feed goes stale and
//! both refuse on staleness before anything else — that agreement is the point,
//! not an escape hatch.
//!
//! Every leg is an `eth_call`: no key, no funds, no state change. Override the
//! endpoint with `COVENANT_RH_MAINNET_RPC` and the guard with
//! `COVENANT_RH_RWA_GUARD`.

use covenant_rwa_firewall::{AssetContext, RwaDenial, RwaPolicy, RwaTrade, Side};

const DEFAULT_RPC: &str = "https://rpc.mainnet.chain.robinhood.com";
const DEFAULT_GUARD: &str = "0x1c6cca8de094209de79a12ed63477434ec2621c0";
const AAPL: &str = "af3d76f1834a1d425780943c99ea8a608f8a93f9";
const AAPL_FEED: &str = "0x6b22a786baa607d76728168703a39ea9c99f2cd0";
// Registered on the guard for nothing: WETH has no Stock Token oracle here.
const UNREGISTERED: &str = "0bd7d308f8e1639fab988df18a8011f41eacad73";

const WAD: u128 = 1_000_000_000_000_000_000;

const SEL_CHECK_TRADE: &str = "79f3f75d";
const SEL_ASSET_CONFIG: &str = "d6dbaf58";
const SEL_UI_MULTIPLIER: &str = "a60bf13d";
const SEL_ORACLE_PAUSED: &str = "7706ba52";
const SEL_LATEST_ROUND: &str = "feaf968c";

const ERR_ASSET_NOT_ENABLED: &str = "f6f24b83";
const ERR_STALE_PRICE_FEED: &str = "ad11e522";
const ERR_PRICE_OUTSIDE_BAND: &str = "7bf748a1";
const ERR_NOTIONAL_OVER_CAP: &str = "cfcc2fe0";
const ERR_ORACLE_PAUSED: &str = "e28b7053";
const ERR_MULTIPLIER_UNSET: &str = "cdf047f3";
const ERR_PRICE_UNAVAILABLE: &str = "cb08be81";

fn rpc_url() -> String {
    std::env::var("COVENANT_RH_MAINNET_RPC").unwrap_or_else(|_| DEFAULT_RPC.into())
}

fn guard() -> String {
    std::env::var("COVENANT_RH_RWA_GUARD").unwrap_or_else(|_| DEFAULT_GUARD.into())
}

/// One `eth_call`. Returns the return data on success, the revert data on a
/// revert — the guard says what it refused in that revert, so both matter.
async fn eth_call(to: &str, data: &str) -> Result<Vec<u8>, Vec<u8>> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "eth_call",
        "params": [{ "to": to, "data": format!("0x{data}") }, "latest"],
    });
    let resp: serde_json::Value = reqwest::Client::builder()
        // the endpoint refuses a bare programmatic user agent
        .user_agent("Mozilla/5.0")
        .build()
        .expect("http client")
        .post(rpc_url())
        .json(&body)
        .send()
        .await
        .expect("rpc send")
        .json()
        .await
        .expect("rpc json");

    if let Some(result) = resp.get("result").and_then(|v| v.as_str()) {
        return Ok(unhex(result));
    }
    let revert = resp
        .get("error")
        .and_then(|e| e.get("data"))
        .and_then(|d| d.as_str())
        .unwrap_or_else(|| panic!("neither result nor revert data: {resp}"));
    Err(unhex(revert))
}

fn unhex(s: &str) -> Vec<u8> {
    let body = s.strip_prefix("0x").unwrap_or(s);
    (0..body.len() / 2)
        .map(|i| u8::from_str_radix(&body[i * 2..i * 2 + 2], 16).expect("hex"))
        .collect()
}

fn word(bytes: &[u8], index: usize) -> u128 {
    bytes[index * 32 + 16..index * 32 + 32]
        .iter()
        .fold(0u128, |acc, b| (acc << 8) | u128::from(*b))
}

/// The 20-byte address in a 32-byte word (a u128 read would clip the top four).
fn address(bytes: &[u8], index: usize) -> String {
    let word = &bytes[index * 32 + 12..index * 32 + 32];
    format!(
        "0x{}",
        word.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn selector(bytes: &[u8]) -> String {
    bytes.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn pad(addr: &str) -> String {
    format!("{:0>64}", addr.trim_start_matches("0x"))
}

fn uint(v: u128) -> String {
    format!("{v:064x}")
}

/// What the guard is configured to allow for the asset, read off the chain.
struct Config {
    feed: String,
    cap_usd_e8: u128,
    band_bps: u32,
    max_staleness_secs: u64,
}

async fn live_config() -> Config {
    let out = eth_call(&guard(), &format!("{SEL_ASSET_CONFIG}{}", pad(AAPL)))
        .await
        .expect("assetConfig is a view");
    Config {
        feed: address(&out, 0),
        cap_usd_e8: word(&out, 1),
        band_bps: word(&out, 2) as u32,
        max_staleness_secs: word(&out, 3) as u64,
    }
}

/// The same context the guard reads inside `checkTrade`, read independently.
async fn live_context(now: u64) -> AssetContext {
    let multiplier = word(
        &eth_call(&format!("0x{AAPL}"), SEL_UI_MULTIPLIER)
            .await
            .expect("uiMultiplier"),
        0,
    );
    let paused = word(
        &eth_call(&format!("0x{AAPL}"), SEL_ORACLE_PAUSED)
            .await
            .expect("oraclePaused"),
        0,
    ) != 0;
    let round = eth_call(AAPL_FEED, SEL_LATEST_ROUND)
        .await
        .expect("latestRoundData");
    AssetContext {
        oracle_price_usd_e8: word(&round, 1),
        oracle_updated_at: word(&round, 3) as u64,
        ui_multiplier_e18: multiplier,
        oracle_paused: paused,
        now,
        // The guard has no calendar; staleness is the only freshness signal it
        // can enforce, so the parity run holds the off-chain gate open and lets
        // staleness speak for both halves.
        market_open: true,
    }
}

async fn on_chain_verdict(
    asset: &str,
    raw_amount: u128,
    quoted_price_usd_e8: u128,
) -> Result<u128, String> {
    let data = format!(
        "{SEL_CHECK_TRADE}{}{}{}",
        pad(asset),
        uint(raw_amount),
        uint(quoted_price_usd_e8)
    );
    match eth_call(&guard(), &data).await {
        Ok(out) => Ok(word(&out, 0)),
        Err(revert) => Err(selector(&revert)),
    }
}

/// The refusal the guard would raise for a denial this crate returned.
fn expected_selector(denial: &RwaDenial) -> &'static str {
    match denial {
        RwaDenial::PriceUnavailable | RwaDenial::AmountTooLarge => ERR_PRICE_UNAVAILABLE,
        RwaDenial::MultiplierUnset => ERR_MULTIPLIER_UNSET,
        RwaDenial::OraclePaused => ERR_ORACLE_PAUSED,
        RwaDenial::StalePriceFeed { .. } => ERR_STALE_PRICE_FEED,
        RwaDenial::PriceOutsideBand { .. } => ERR_PRICE_OUTSIDE_BAND,
        RwaDenial::NotionalOverCap { .. } => ERR_NOTIONAL_OVER_CAP,
        // The off-chain half owns the calendar; the guard has no equivalent.
        RwaDenial::MarketClosed => unreachable!("the parity run holds the market-hours gate open"),
        // Stateful, so it belongs to commitTrade rather than the checkTrade view
        // this parity run drives.
        RwaDenial::WindowCapExceeded { .. } => unreachable!("checkTrade does not touch the window"),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

#[tokio::test]
#[ignore = "live: calls the deployed guard on Robinhood Chain mainnet"]
async fn live_guard_registers_aapl_against_its_real_chainlink_feed() {
    let cfg = live_config().await;
    assert_eq!(
        cfg.feed, AAPL_FEED,
        "AAPL is pinned to the Robinhood AAPL/USD feed"
    );
    assert!(
        cfg.cap_usd_e8 > 0,
        "a registered asset carries a per-trade cap"
    );
    assert!(
        cfg.band_bps > 0 && cfg.band_bps < 10_000,
        "the fair-value band is a real bound"
    );
    assert!(cfg.max_staleness_secs > 0, "the feed has a freshness bound");
}

#[tokio::test]
#[ignore = "live: calls the deployed guard on Robinhood Chain mainnet"]
async fn live_guard_and_policy_agree_on_the_live_asset() {
    let cfg = live_config().await;
    let now = now();
    let ctx = live_context(now).await;
    let policy = RwaPolicy {
        per_trade_notional_cap_usd_e8: cfg.cap_usd_e8,
        fair_value_band_bps: cfg.band_bps,
        max_feed_staleness_secs: cfg.max_staleness_secs,
        require_market_hours: false,
    };
    let asset = {
        let mut a = [0u8; 20];
        a.copy_from_slice(&unhex(AAPL));
        a
    };
    let oracle = ctx.oracle_price_usd_e8;
    assert!(oracle > 0, "the live feed prints a price");

    // Sized so the case under test is the one that bites: a hundredth of a share
    // is far inside any sane cap, a hundred shares is far outside this one.
    let cases: [(&str, u128, u128); 3] = [
        ("a fair, in-cap buy", WAD / 100, oracle),
        ("a buy past the per-trade cap", 100 * WAD, oracle),
        ("a fill 2% off the oracle", WAD / 100, oracle * 102 / 100),
    ];

    for (label, raw_amount, quoted) in cases {
        let trade = RwaTrade {
            asset,
            side: Side::Buy,
            raw_amount,
            quoted_price_usd_e8: quoted,
        };
        let off_chain = policy.evaluate(&trade, &ctx);
        let on_chain = on_chain_verdict(&format!("0x{AAPL}"), raw_amount, quoted).await;

        match (&off_chain, &on_chain) {
            (Ok(verdict), Ok(notional)) => {
                assert_eq!(verdict.notional_usd_e8, *notional, "{label}: same notional");
            }
            (Err(denial), Err(selector)) => {
                assert_eq!(
                    expected_selector(denial),
                    selector,
                    "{label}: same refusal ({denial})"
                );
            }
            _ => panic!(
                "{label}: the halves disagree — off-chain {off_chain:?}, on-chain {on_chain:?}"
            ),
        }
    }

    // An asset the owner never registered is not tradeable through the guard,
    // whatever the market is doing — this is the look-alike-token gate.
    let unregistered = on_chain_verdict(&format!("0x{UNREGISTERED}"), WAD / 100, oracle).await;
    assert_eq!(unregistered.unwrap_err(), ERR_ASSET_NOT_ENABLED);
}
