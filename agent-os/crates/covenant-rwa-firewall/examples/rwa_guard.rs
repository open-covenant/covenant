//! The RWA firewall against a live Robinhood Chain Stock Token.
//!
//! Reads the real AAPL Stock Token and its Chainlink feed on chain 4663, builds
//! the asset context from what the chain says right now, and runs a set of
//! proposed trades through [`covenant_rwa_firewall::RwaPolicy`]. The price, the
//! feed age, the multiplier, and the oracle-paused flag are all live; the trades
//! are the thing being judged.
//!
//!   cargo run -p covenant-rwa-firewall --example rwa_guard
//!
//! Per Robinhood's docs the canonical addresses come from the live asset
//! registry, not a constant. These were resolved from Blockscout and the
//! Chainlink reference-data-directory on 2026-08-20; re-confirm before trusting
//! them for anything beyond this demo.

use std::time::{SystemTime, UNIX_EPOCH};

use covenant_rwa_firewall::{AssetContext, RwaPolicy, RwaTrade, Side};
use serde_json::json;

const RPC: &str = "https://rpc.mainnet.chain.robinhood.com";
const AAPL_TOKEN: &str = "0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9";
const AAPL_FEED: &str = "0x6B22A786bAa607d76728168703a39Ea9C99f2cD0";
const WAD: u128 = 1_000_000_000_000_000_000;

#[tokio::main]
async fn main() {
    let http = reqwest::Client::builder()
        .user_agent("Mozilla/5.0")
        .build()
        .expect("http client");

    let multiplier = read_uint(&http, AAPL_TOKEN, "0xa60bf13d").await; // uiMultiplier()
    let paused = read_uint(&http, AAPL_TOKEN, "0x7706ba52").await != 0; // oraclePaused()
    let (answer_e8, updated_at) = read_latest_round(&http, AAPL_FEED).await;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(updated_at);
    // The Robinhood stock feeds update on market hours; a fresh feed is the best
    // signal the market is open, so proxy it rather than invent a calendar.
    let market_open = age < 900;

    println!("live AAPL on Robinhood Chain 4663");
    println!("  price        ${:.2}", answer_e8 as f64 / 1e8);
    println!("  feed age     {age}s  (market_open ~= {market_open})");
    println!(
        "  multiplier   {:.6}  (uiMultiplier / 1e18)",
        multiplier as f64 / 1e18
    );
    println!("  oraclePaused {paused}");
    println!();

    let ctx = AssetContext {
        oracle_price_usd_e8: answer_e8,
        oracle_updated_at: updated_at,
        ui_multiplier_e18: multiplier,
        now,
        market_open,
    };

    // A production-shaped policy: $10k per trade, fills within 0.5% of the oracle,
    // a one-hour freshness limit, in-hours only.
    let strict = RwaPolicy {
        per_trade_notional_cap_usd_e8: 10_000 * 100_000_000,
        fair_value_band_bps: 50,
        max_feed_staleness_secs: 3_600,
        require_market_hours: true,
    };

    let fair_five = RwaTrade {
        asset: to_addr(AAPL_TOKEN),
        side: Side::Buy,
        raw_amount: 5 * WAD,
        quoted_price_usd_e8: answer_e8,
    };
    report(
        "strict policy, buy 5 AAPL at the oracle",
        &strict,
        &fair_five,
        &ctx,
    );

    // Isolate the price and size checks with a policy that tolerates the live
    // off-hours staleness, so those refusals show against the real price.
    let lenient = RwaPolicy {
        max_feed_staleness_secs: u64::MAX,
        require_market_hours: false,
        ..strict
    };

    report(
        "buy 5 AAPL at the oracle (fresh-feed policy)",
        &lenient,
        &fair_five,
        &ctx,
    );
    report(
        "buy 1 AAPL at 1% over the oracle",
        &lenient,
        &RwaTrade {
            quoted_price_usd_e8: answer_e8 * 101 / 100,
            raw_amount: WAD,
            ..fair_five.clone()
        },
        &ctx,
    );
    report(
        "buy 100 AAPL (~$31k, over the $10k cap)",
        &lenient,
        &RwaTrade {
            raw_amount: 100 * WAD,
            ..fair_five.clone()
        },
        &ctx,
    );
}

fn report(label: &str, policy: &RwaPolicy, trade: &RwaTrade, ctx: &AssetContext) {
    match policy.evaluate(trade, ctx) {
        Ok(v) => println!(
            "  ALLOW   {label}\n            notional ${:.2}, {:.4} shares, {}bps off oracle",
            v.notional_usd_e8 as f64 / 1e8,
            v.shares_e18 as f64 / 1e18,
            v.diff_bps
        ),
        Err(e) => println!("  REFUSE  {label}\n            {e}"),
    }
}

fn to_addr(s: &str) -> [u8; 20] {
    let bytes = hex_bytes(s);
    let mut a = [0u8; 20];
    a.copy_from_slice(&bytes[..20]);
    a
}

async fn read_uint(http: &reqwest::Client, to: &str, data: &str) -> u128 {
    let bytes = eth_call(http, to, data).await;
    let tail = &bytes[bytes.len().saturating_sub(16)..];
    let mut n = 0u128;
    for b in tail {
        n = (n << 8) | u128::from(*b);
    }
    n
}

async fn read_latest_round(http: &reqwest::Client, feed: &str) -> (u128, u64) {
    // latestRoundData() -> (uint80, int256 answer, uint256, uint256 updatedAt, uint80)
    let bytes = eth_call(http, feed, "0xfeaf968c").await;
    let word = |i: usize| -> &[u8] { &bytes[i * 32..i * 32 + 32] };
    let answer = word(1);
    let updated = word(3);
    let to_u128 = |w: &[u8]| w.iter().fold(0u128, |acc, b| (acc << 8) | u128::from(*b));
    (to_u128(&answer[16..]), to_u128(&updated[16..]) as u64)
}

async fn eth_call(http: &reqwest::Client, to: &str, data: &str) -> Vec<u8> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "eth_call",
        "params": [{ "to": to, "data": data }, "latest"],
    });
    let resp: serde_json::Value = http
        .post(RPC)
        .json(&body)
        .send()
        .await
        .expect("rpc send")
        .json()
        .await
        .expect("rpc json");
    let result = resp
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no result: {resp}"));
    hex_bytes(result)
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let body = s.strip_prefix("0x").unwrap_or(s);
    (0..body.len() / 2)
        .map(|i| u8::from_str_radix(&body[i * 2..i * 2 + 2], 16).expect("hex"))
        .collect()
}
