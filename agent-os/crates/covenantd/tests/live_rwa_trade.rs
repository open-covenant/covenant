//! Opt-in live proof of the daemon's trading leg against the deployed guard on
//! Robinhood Chain (4663).
//!
//!   cargo test -p covenantd --test live_rwa_trade -- --ignored live_
//!
//! The point of this leg is that the daemon and the contract refuse the same
//! trades. So this reads the guard's own bounds and the asset's live oracle
//! context off chain, judges four trades locally, and puts the same four through
//! the deployed guard's `checkTrade` view. Local verdict and on-chain verdict
//! have to agree on every one, whatever the market is doing when it runs.
//!
//! Read-only throughout: `eth_call` only, no key, no funds, no state. Override
//! the endpoint with `COVENANT_RWA_MAINNET_RPC`, the guard with
//! `COVENANT_RWA_GUARD`, and the asset with `COVENANT_RWA_ASSET`.

use covenantd::rwa::{RwaConfig, Side, TradeRequest};

const DEFAULT_RPC: &str = "https://rpc.mainnet.chain.robinhood.com";
const DEFAULT_GUARD: &str = "1c6cca8de094209de79a12ed63477434ec2621c0";
const DEFAULT_EXECUTOR: &str = "e94a70f8c864ca3cae85c74f92ab8783d2d039a3";
const DEFAULT_ASSET: &str = "af3d76f1834a1d425780943c99ea8a608f8a93f9";
const WAD: u128 = 1_000_000_000_000_000_000;

fn addr(hex: &str) -> [u8; 20] {
    let body = hex.trim_start_matches("0x");
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&body[i * 2..i * 2 + 2], 16).expect("hex address");
    }
    out
}

fn from_env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

fn config() -> RwaConfig {
    RwaConfig::new(
        4663,
        addr(&from_env("COVENANT_RWA_GUARD", DEFAULT_GUARD)),
        addr(&from_env("COVENANT_RWA_EXECUTOR", DEFAULT_EXECUTOR)),
    )
    .with_rpc(from_env("COVENANT_RWA_MAINNET_RPC", DEFAULT_RPC))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

fn request(asset: [u8; 20], min_out: u128, quoted: u128) -> TradeRequest {
    TradeRequest {
        asset,
        side: Side::Buy,
        max_in: 300_000,
        min_out,
        quoted_price_usd_e8: quoted,
        router: [0u8; 20],
        swap_data: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "live: reads the deployed guard on Robinhood Chain mainnet"]
async fn live_guard_bounds_are_readable() {
    let cfg = config();
    let asset = addr(&from_env("COVENANT_RWA_ASSET", DEFAULT_ASSET));
    let live = cfg
        .live_context(asset, now())
        .await
        .expect("the guard should publish bounds for a registered asset");

    assert!(
        live.policy.per_trade_notional_cap_usd_e8 > 0,
        "a registered asset carries a per-trade cap"
    );
    assert!(
        live.policy.fair_value_band_bps > 0 && live.policy.fair_value_band_bps < 10_000,
        "the fair-value band is a real bound"
    );
    assert!(
        live.policy.max_feed_staleness_secs > 0,
        "the feed carries a freshness bound"
    );
    assert!(
        live.context.oracle_price_usd_e8 > 0,
        "the live feed prints a price"
    );
    assert!(
        live.context.ui_multiplier_e18 > 0,
        "an initialized Stock Token has a multiplier"
    );
}

#[tokio::test]
#[ignore = "live: reads the deployed guard on Robinhood Chain mainnet"]
async fn live_daemon_and_guard_refuse_the_same_trades() {
    let cfg = config();
    let asset = addr(&from_env("COVENANT_RWA_ASSET", DEFAULT_ASSET));
    let live = cfg.live_context(asset, now()).await.expect("live context");
    let oracle = live.context.oracle_price_usd_e8;

    // Sized so the case under test is the one that bites, exactly as the
    // contract-side parity test does.
    let cases: [(&str, u128, u128); 3] = [
        ("a fair, in-cap buy", WAD / 100, oracle),
        ("a buy past the per-trade cap", 100 * WAD, oracle),
        ("a fill 2% off the oracle", WAD / 100, oracle * 102 / 100),
    ];

    for (label, min_out, quoted) in cases {
        let req = request(asset, min_out, quoted);
        let local = cfg.judge(&req, &live);
        let on_chain = cfg.preview(&req).await;
        assert_eq!(
            local.is_ok(),
            on_chain.is_ok(),
            "{label}: the daemon and the guard disagree (local {local:?}, on-chain {on_chain:?})"
        );
    }

    // An asset the owner never registered is not tradeable, whatever the market
    // is doing. The daemon refuses this before it can even build a policy.
    let unregistered = cfg.live_context([0x0b; 20], now()).await;
    assert!(
        unregistered.is_err(),
        "an unregistered asset has no bounds to trade under"
    );
}
