//! Compatibility regression for the parked daemon-owned Hyre payment path.
//!
//! The lower-level `covenant-hyre` client remains available for explicit
//! development, but `DaemonHyreExecutor` must fail before signer or network
//! activity until transaction-bound authorization and a durable prepayment
//! reservation exist.

use std::sync::Arc;

use covenant_audit::{AuditLog, InMemoryAuditLog};
use covenant_budget::{BudgetLedger, InMemoryLedger};
use covenant_hyre::{PaidExecutor, PaidRequest};
use covenant_settlement::{InMemorySettlement, Settlement};
use covenant_types::AgentId;
use covenantd::hyre::DaemonHyreExecutor;
use covenantd::x402::{X402Config, LEGACY_OUTBOUND_PARKED};

#[tokio::main]
async fn main() {
    let settlement: Arc<dyn Settlement> = Arc::new(InMemorySettlement::new());
    let audit: Arc<dyn AuditLog> = Arc::new(InMemoryAuditLog::new());
    let budget: Arc<dyn BudgetLedger> = Arc::new(InMemoryLedger::new());
    let issuer = AgentId::new("operator@local", [0x11; 32]);
    let payer = AgentId::new("payer@local", [0x22; 32]);
    budget
        .set_capacity(&payer, 100_000)
        .await
        .expect("seed budget");

    let executor = DaemonHyreExecutor::new(
        settlement.clone(),
        audit.clone(),
        budget.clone(),
        Arc::new(X402Config {
            enabled: true,
            ..Default::default()
        }),
        issuer,
        payer.clone(),
    );
    let request = PaidRequest {
        provider: "hyre".into(),
        slug: "defi/tvl".into(),
        url: "https://mpp.hyreagent.fun/defi/tvl".into(),
        method: "GET".into(),
        body: None,
        network: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".into(),
        asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
        per_call_cap: 50_000,
        credits: 10_000,
        price_micro_usdc: 10_000,
    };

    let error = executor
        .execute(request)
        .await
        .expect_err("daemon-owned Hyre payment must remain parked");
    assert_eq!(error, LEGACY_OUTBOUND_PARKED);
    assert!(settlement
        .recent(10)
        .await
        .expect("settlement read")
        .is_empty());
    assert!(audit.recent(10).await.expect("audit read").is_empty());
    assert_eq!(
        budget.tokens_remaining(&payer).await.expect("budget read"),
        100_000
    );

    println!("{LEGACY_OUTBOUND_PARKED}");
}
