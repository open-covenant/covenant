//! Runs the x402 ER facilitator over a single paid endpoint.
//!
//!   cargo run -p covenant-x402-facilitator
//!
//! Env: PROGRAM (settlement program, default cov9UDyp…), ER (validator RPC, default
//! devnet-eu), PRICE (credits/call, default 1), PORT (default 8402).

use std::env;

use covenant_x402_facilitator::{router, ErPaymentVerifier, PaidEndpoint};
use serde_json::json;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let program = env::var("PROGRAM").unwrap_or("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y".into());
    let er = env::var("ER").unwrap_or("https://devnet-eu.magicblock.app".into());
    let price: u64 = env::var("PRICE").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let port: u16 = env::var("PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8402);

    let content = json!({ "quote": "BTC looks coiled. Position for a breakout." });
    let state = PaidEndpoint::new(ErPaymentVerifier::new(program.clone(), er.clone()), price, content);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!(%program, %er, price, %addr, "x402 ER facilitator listening on /paid");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, router(state)).await.expect("serve");
}
