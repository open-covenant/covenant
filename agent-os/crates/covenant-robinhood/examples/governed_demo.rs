//! Governed trading demo: run a sequence of orders through the Covenant policy
//! gate and print the signed receipt for each decision.
//!
//! Dry-run against a mock by default (nothing is sent, no credentials needed):
//!     cargo run -p covenant-robinhood --example governed_demo
//!
//! Against a real account, set the credentials (reads + dry-run receipts):
//!     COVENANT_ROBINHOOD_API_KEY=... COVENANT_ROBINHOOD_KEYPAIR=/path/key.b64 \
//!         cargo run -p covenant-robinhood --example governed_demo
//! Add COVENANT_ROBINHOOD_LIVE=1 to actually place the within-policy order.

use covenant_robinhood::governed::GovernedTrader;
use covenant_robinhood::policy::{Approvals, Caps, Rate, Risk, TradingPolicy, Universe};
use covenant_robinhood::{
    HttpTransport, Mode, OrderRequest, OrderType, RobinhoodClient, RobinhoodSigner, Side, Transport,
};
use ed25519_dalek::SigningKey;
use serde_json::json;

fn policy(mode: Mode) -> TradingPolicy {
    TradingPolicy {
        version: 1,
        venue: "robinhood-crypto".into(),
        mode,
        caps: Caps { per_order_usd: Some(500.0), daily_notional_usd: Some(2_000.0) },
        risk: Risk { daily_loss_stop_usd: Some(300.0) },
        universe: Universe {
            allow: Some(vec!["BTC-USD".into(), "ETH-USD".into()]),
            deny: None,
            sides: Some(vec![Side::Buy]),
        },
        order_types: vec![OrderType::Market],
        rate: Rate { max_orders_per_min: Some(10), cooldown_secs: None },
        approvals: Approvals { require_human_over_usd: Some(400.0) },
    }
}

async fn run<T: Transport + 'static>(trader: GovernedTrader<T>) {
    let cases = [
        ("within policy   (~$60)", OrderRequest::market("BTC-USD", Side::Buy, 0.001)),
        ("over per-order   (~$1200)", OrderRequest::market("BTC-USD", Side::Buy, 0.02)),
        ("not in universe  (DOGE)", OrderRequest::market("DOGE-USD", Side::Buy, 1_000.0)),
        ("needs approval   (~$450)", OrderRequest::market("BTC-USD", Side::Buy, 0.0075)),
    ];
    for (label, order) in cases {
        match trader.submit(order).await {
            Ok(s) => println!(
                "{label:<26} -> {:<16?} verified={} anchor={:?}  {}",
                s.receipt.decision,
                s.verify(),
                s.anchor,
                s.receipt.reason.clone().unwrap_or_default(),
            ),
            Err(e) => println!("{label:<26} -> error: {e}"),
        }
    }
}

#[tokio::main]
async fn main() {
    let attestor = SigningKey::from_bytes(&[42u8; 32]);
    let creds = std::env::var("COVENANT_ROBINHOOD_API_KEY")
        .ok()
        .zip(std::env::var("COVENANT_ROBINHOOD_KEYPAIR").ok());

    match creds {
        Some((api_key, keypair_path)) => {
            let key_b64 = std::fs::read_to_string(&keypair_path).expect("read keypair file");
            let signer = RobinhoodSigner::from_base64_key(api_key, &key_b64).expect("valid credential");
            let mode = if std::env::var("COVENANT_ROBINHOOD_LIVE").as_deref() == Ok("1") {
                Mode::Live
            } else {
                Mode::DryRun
            };
            println!("live account, {mode:?}\n");
            let client = RobinhoodClient::new(signer, HttpTransport::new());
            run(GovernedTrader::new(client, policy(mode), attestor, "demo-agent")).await;
        }
        None => {
            println!("no credentials set: dry-run against the mock transport\n");
            let mock = crate_mock();
            let signer = RobinhoodSigner::new("demo", SigningKey::from_bytes(&[1u8; 32]));
            let client = RobinhoodClient::new(signer, mock);
            run(GovernedTrader::new(client, policy(Mode::DryRun), attestor, "demo-agent")).await;
        }
    }
}

fn crate_mock() -> covenant_robinhood::MockTransport {
    covenant_robinhood::MockTransport::new()
        .json("GET", "symbol=BTC-USD", json!({"results":[{"symbol":"BTC-USD","price":"60000"}]}))
        .json("GET", "symbol=DOGE-USD", json!({"results":[{"symbol":"DOGE-USD","price":"0.12"}]}))
        .json("POST", "/orders/", json!({"id":"mock_order_1"}))
}
