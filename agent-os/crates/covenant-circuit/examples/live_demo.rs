//! Live demo: an agent settling a real Circuit call on Solana. It spends, so it is guarded
//! by `--confirm`; without it, the demo only hits the free status endpoint.
//!
//! Circuit's Data API quotes CIRC by default and lists alternates under `acceptedTokens`.
//! Set `CIRCUIT_PAY_TOKEN` to the $CVNT mint to pay Circuit in Covenant's own token —
//! Circuit auto-settles it to CIRC on their side. (The inference gateway quotes CIRC only
//! today, so an alt-token run settles a paid Data API call.)
//!
//!   CIRCUIT_KEYPAIR=~/.config/solana/covenant-cvnt-consolidated.json \
//!   CIRCUIT_PAY_TOKEN=2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump \
//!   cargo run -p covenant-circuit --features solana --example live_demo -- --confirm
//!
//! Env:
//!   CIRCUIT_KEYPAIR    (required) funder keypair, holding the pay token + SOL for gas
//!   CIRCUIT_PAY_TOKEN  (optional) mint to settle in; unset = CIRC (the 402 default)
//!   CIRCUIT_RPC        (optional) Solana RPC URL, default mainnet-beta
//!   CIRCUIT_TREASURY   (optional) pin the 402 recipient to this pubkey
//!   CIRCUIT_PER_CALL   (optional) per-call spend cap, raw base units
//!   CIRCUIT_BUDGET     (optional) cumulative budget, raw base units

use std::sync::Arc;
use std::time::Duration;

use covenant_circuit::{
    ChatMessage, ChatParams, CircPayer, CircuitCapability, DataClient, Inference, SolanaCircPayer,
    SpendLedger, X402,
};

const SOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[tokio::main]
async fn main() {
    let confirm = std::env::args().any(|a| a == "--confirm");
    let keypair =
        std::env::var("CIRCUIT_KEYPAIR").expect("set CIRCUIT_KEYPAIR to a funder keypair path");
    let rpc = std::env::var("CIRCUIT_RPC").ok();
    let pay_token = std::env::var("CIRCUIT_PAY_TOKEN").ok();

    let payer = Arc::new(
        SolanaCircPayer::from_keypair_file(&keypair, rpc.as_deref()).expect("load funder keypair"),
    );

    // The grant: pin Circuit's hosts, and optionally the treasury and spend caps.
    let mut cap = CircuitCapability::new()
        .allow_host("inference.circuitllm.xyz")
        .allow_host("api.circuitllm.xyz");
    if let Ok(t) = std::env::var("CIRCUIT_TREASURY") {
        cap = cap.allow_recipient(t);
    }
    if let Ok(v) = std::env::var("CIRCUIT_PER_CALL") {
        cap = cap.per_call(v.parse().expect("CIRCUIT_PER_CALL is a u64"));
    }
    if let Ok(v) = std::env::var("CIRCUIT_BUDGET") {
        cap = cap.budget(v.parse().expect("CIRCUIT_BUDGET is a u64"));
    }

    let ledger = Arc::new(SpendLedger::new());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("http client");

    let mut engine = X402::new(http, payer.clone(), cap, ledger.clone());
    if let Some(mint) = pay_token.clone() {
        engine = engine.with_pay_mint(mint);
    }
    let inference = Inference::from_x402(engine.clone());
    let data = DataClient::from_x402(engine);

    println!("funder {}", payer.address().unwrap_or("?"));
    println!(
        "paying in {}",
        pay_token.as_deref().unwrap_or("CIRC (default)")
    );

    // Free endpoint first: proves connectivity with no spend.
    match data.status().await {
        Ok(s) => println!("circuit status ok: {s}"),
        Err(e) => {
            eprintln!("status failed: {e}");
            std::process::exit(1);
        }
    }

    if !confirm {
        println!("\ndry run — pass --confirm to settle a paid Data API call.");
        return;
    }

    // The agent senses (paid data) — the Data API lists $CVNT under acceptedTokens, so this
    // settles in whatever CIRCUIT_PAY_TOKEN selected.
    match data
        .get_paid("/api/token-security", &[("mint", SOL_MINT.to_string())])
        .await
    {
        Ok(p) => println!(
            "\ndata.token_security(SOL) settled\n  paid {} raw {} | tx {}",
            p.quote.as_ref().map(|q| q.amount_raw).unwrap_or(0),
            p.quote.as_ref().map(|q| q.token.as_str()).unwrap_or("-"),
            p.payment_tx.as_deref().unwrap_or("-")
        ),
        Err(e) => {
            eprintln!("data failed: {e}");
            std::process::exit(1);
        }
    }

    // The inference gateway quotes CIRC only (no acceptedTokens), so attempt it only when
    // settling in CIRC; otherwise say so plainly rather than fail a guaranteed-CIRC call.
    if pay_token.is_some() {
        println!(
            "\ninference skipped — the Circuit inference gateway quotes CIRC only right now; \
             $CVNT is accepted on the Data API."
        );
    } else {
        match inference
            .chat(ChatParams::new(vec![ChatMessage::user(
                "In one sentence, what is an ephemeral rollup?",
            )]))
            .await
        {
            Ok(r) => println!(
                "\ninference -> {}\n  paid {} raw {} | tx {}",
                r.content,
                r.paid_raw.unwrap_or(0),
                r.token.as_deref().unwrap_or("-"),
                r.payment_tx.as_deref().unwrap_or("-")
            ),
            Err(e) => {
                eprintln!("inference failed: {e}");
                std::process::exit(1);
            }
        }
    }

    println!("\ntotal settled: {} raw base units", ledger.spent_raw());
}
