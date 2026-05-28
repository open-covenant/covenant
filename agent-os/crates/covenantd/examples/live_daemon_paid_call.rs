//! Real daemon path: `DaemonHyreExecutor` against the live Hyre API
//! with the real sidecar binary, real Solana settlement, and in-memory
//! settlement/audit/budget ledgers (the daemon's accounting subsystems
//! in the same shape they have in production).
//!
//! Where `live_paid_call` in `covenant-hyre` only exercises the inner
//! 402-then-pay loop, this also exercises the daemon's budget
//! pre-check, the `record_paid_call` receipt write, the audit event,
//! and the issuer-vs-payer split — i.e. everything that lands on top
//! of the signed transfer.
//!
//! Dry-run by default. Pass `--confirm` to actually pay.
//!
//!     COVENANT_X402_FUNDING_KEYPAIR=~/.config/solana/some.json \
//!     COVENANT_X402_RPC_URL=https://api.mainnet-beta.solana.com \
//!     COVENANT_X402_SIGNER_BIN=/path/to/covenant-x402-signer \
//!     cargo run -p covenantd --example live_daemon_paid_call -- --confirm

use std::sync::Arc;

use covenant_audit::{AuditLog, InMemoryAuditLog};
use covenant_budget::{BudgetLedger, InMemoryLedger};
use covenant_hyre::config::{HyreConfig, PAY_TO, USDC_MINT};
use covenant_hyre::{PaidExecutor, PaidRequest};
use covenant_settlement::{InMemorySettlement, Settlement};
use covenant_types::AgentId;
use covenantd::hyre::DaemonHyreExecutor;
use covenantd::x402::X402Config;

const CREDITS_PER_CALL: u64 = 10_000;
const PER_CALL_CAP: u128 = 50_000;
const PRICE_MICRO_USDC: u128 = 10_000;

#[tokio::main]
async fn main() {
    let confirm = std::env::args().any(|a| a == "--confirm");
    let cfg = HyreConfig::default();

    let signer_bin = std::env::var("COVENANT_X402_SIGNER_BIN")
        .expect("COVENANT_X402_SIGNER_BIN must point to the built covenant-x402-signer binary");
    let kp = std::env::var("COVENANT_X402_FUNDING_KEYPAIR")
        .expect("COVENANT_X402_FUNDING_KEYPAIR must be set");
    let rpc = std::env::var("COVENANT_X402_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());

    let settlement: Arc<dyn Settlement> = Arc::new(InMemorySettlement::new());
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let budget: Arc<dyn BudgetLedger> = Arc::new(InMemoryLedger::new());

    let issuer = AgentId::new("operator@local", [0x11; 32]);
    let payer = AgentId::new("payer@local", [0x22; 32]);
    budget.set_capacity(&payer, 100_000).await.expect("seed budget");
    println!("payer budget seeded: 100000 credits");

    let x402 = Arc::new(X402Config {
        enabled: true,
        signer_binary: signer_bin.into(),
        signer_env: vec![
            ("COVENANT_X402_FUNDING_KEYPAIR".into(), kp),
            ("COVENANT_X402_RPC_URL".into(), rpc),
        ],
    });

    let executor = DaemonHyreExecutor::new(
        settlement.clone(),
        audit.clone(),
        budget.clone(),
        x402,
        issuer.clone(),
        payer.clone(),
    );

    let req = PaidRequest {
        provider: "hyre".into(),
        slug: "defi/tvl".into(),
        url: format!("{}/defi/tvl", cfg.base_url),
        method: "GET".into(),
        body: None,
        network: cfg.network.clone(),
        asset: cfg.asset.clone(),
        per_call_cap: PER_CALL_CAP,
        credits: CREDITS_PER_CALL,
        price_micro_usdc: PRICE_MICRO_USDC,
    };

    println!("target:        {}", req.url);
    println!("pay_to (pin):  {PAY_TO}");
    println!("asset (pin):   {USDC_MINT}");
    println!("network:       {}", req.network);

    if !confirm {
        println!("\nDRY RUN — pass --confirm to actually pay (spends $0.01 USDC).");
        return;
    }

    println!("\n--- executor.execute() ---");
    let result = executor.execute(req).await;

    let response = match result {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: executor returned error: {e}");
            std::process::exit(1);
        }
    };

    println!("status:        {}", response.status);
    println!("receipt_id:    {:?}", response.receipt_id);
    let body_preview = response.body.to_string();
    println!("body (head):   {}", &body_preview[..body_preview.len().min(300)]);

    let settled = settlement.recent(10).await.expect("settlement recent");
    let audit_events = audit.recent(10).await.expect("audit recent");
    let payer_remaining = budget
        .tokens_remaining(&payer)
        .await
        .map(|c| c.to_string())
        .unwrap_or_else(|e| format!("err: {e}"));

    println!("\n--- daemon accounting after the call ---");
    println!("settlement receipts: {} written", settled.len());
    for r in &settled {
        println!(
            "  - id={} resource={:?} credits_consumed={} payer={:?}",
            r.id, r.resource, r.credits_consumed, r.payer
        );
    }
    println!("audit events:        {} written", audit_events.len());
    for e in &audit_events {
        println!("  - kind={:?} issuer={:?}", e.kind, e.issuer);
    }
    println!("payer remaining:     {payer_remaining} credits (was 100000)");

    assert!(response.status >= 200 && response.status < 300, "non-2xx status");
    assert!(response.receipt_id.is_some(), "no receipt id");
    assert!(!settled.is_empty(), "no settlement receipt written");
    assert!(!audit_events.is_empty(), "no audit event written");
    println!("\nOK — daemon-path end-to-end verified.");
}
