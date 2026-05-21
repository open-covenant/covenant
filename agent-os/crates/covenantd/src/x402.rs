//! Daemon-side accounting for outbound x402 payments.
//!
//! The `covenant-x402` crate runs the 402-then-pay loop but holds no
//! budget, writes no receipts, and records no audit events — by
//! design. This module is where those Covenant concerns attach. A
//! completed paid call produces three linked records, in this order:
//!
//! 1. a [`covenant_budget::BudgetDebit`] against the payer,
//! 2. a [`SettlementReceipt`] (resource [`ResourceKind::Tool`]),
//! 3. an [`AuditKind::ExternalPaymentSettled`] event.
//!
//! All three share the receipt id so the budget log, settlement log,
//! and audit log join cleanly — the same invariant the daemon's
//! per-dispatch path maintains.
//!
//! ## Scope
//!
//! This is the accounting layer only. It signs nothing: the
//! [`covenant_x402::Signer`] is supplied by the caller. The real
//! Solana funding-key signer, its `solana` feature wiring, and a
//! live dispatch endpoint land in a separate, separately-reviewed
//! change — anything that moves real USDC is kept out of this slice.

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::{Method, Response};
use serde_json::Value;
use tracing::{debug, warn};
use uuid::Uuid;

use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_settlement::Settlement;
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use covenant_x402::{Capability, Client, Signer, X402Error};

/// Opt-in switch for the daemon's outbound x402 surface. `enabled`
/// defaults to `false` so a daemon with no operator opt-in never
/// makes paid calls.
#[derive(Debug, Clone, Default)]
pub struct X402Config {
    pub enabled: bool,
}

/// Subsystem handles the accounting needs, borrowed for the duration
/// of one call. `issuer` is the actor recorded as the audit event's
/// issuer (the daemon/operator identity), distinct from the `payer`
/// whose budget is debited.
pub struct SettlementContext<'a> {
    pub settlement: &'a dyn Settlement,
    pub audit: &'a dyn AuditLog,
    pub budget: &'a dyn BudgetLedger,
    pub issuer: &'a AgentId,
}

/// A resolved paid call ready to execute. The caller resolves the
/// endpoint and pricing (e.g. from a [`covenant_x402::Catalog`]) and
/// fills this in; `amount`/`network`/`asset` are recorded on the
/// receipt and audit event.
pub struct PaidCall<'a> {
    pub provider: &'a str,
    pub endpoint: &'a str,
    pub method: Method,
    pub capability: Capability,
    pub body: Option<&'a Value>,
    /// Atomic amount advertised for this call.
    pub amount: String,
    /// CAIP-2 network the payment settles on.
    pub network: String,
    /// Asset (mint/contract) paid in.
    pub asset: String,
    /// USD-pegged credits to debit from the payer's budget.
    pub credits: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum X402DaemonError {
    #[error("x402 outbound surface is disabled")]
    Disabled,
    #[error("payer has no budget capacity; refusing to spend")]
    NoCapacity,
    #[error("payer budget would be exceeded by this call")]
    BudgetExceeded,
    #[error("payment: {0}")]
    Payment(#[from] X402Error),
    #[error("budget: {0}")]
    Budget(BudgetError),
    #[error("settlement: {0}")]
    Settlement(String),
    #[error("audit: {0}")]
    Audit(String),
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Records the budget debit, settlement receipt, and audit event for
/// a completed paid call. Returns the shared receipt id.
///
/// Order is debit → receipt → audit. A debit failure aborts before
/// any receipt is written, so the logs never carry a half-recorded
/// call.
pub async fn record_paid_call(
    ctx: &SettlementContext<'_>,
    payer: &AgentId,
    call: &PaidCall<'_>,
) -> Result<Uuid, X402DaemonError> {
    let receipt_id = Uuid::new_v4();
    let now = epoch_ms();

    ctx.budget
        .try_debit(payer, call.credits, receipt_id)
        .await
        .map_err(X402DaemonError::Budget)?;

    ctx.settlement
        .record(SettlementReceipt {
            id: receipt_id,
            payer: payer.clone(),
            resource: ResourceKind::Tool,
            memory_record_id: None,
            credits_consumed: call.credits,
            settled_at: now,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: None,
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        })
        .await
        .map_err(|e| X402DaemonError::Settlement(e.to_string()))?;

    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now,
            issuer: ctx.issuer.clone(),
            kind: AuditKind::ExternalPaymentSettled {
                provider: call.provider.to_string(),
                endpoint: call.endpoint.to_string(),
                network: call.network.clone(),
                asset: call.asset.clone(),
                amount: call.amount.clone(),
                receipt_id,
            },
        })
        .await
        .map_err(|e| X402DaemonError::Audit(e.to_string()))?;

    debug!(
        provider = call.provider,
        endpoint = call.endpoint,
        credits = call.credits,
        %receipt_id,
        "recorded x402 paid call"
    );
    Ok(receipt_id)
}

/// Pre-checks budget, runs the 402-then-pay loop, and records the
/// accounting on a successful paid response.
///
/// The budget is checked read-only *before* paying so the daemon does
/// not spend USDC for an agent that cannot afford the credits. The
/// authoritative debit happens after a successful response. A free
/// (non-402) success is returned without any debit or receipt.
pub async fn pay_and_record(
    ctx: &SettlementContext<'_>,
    config: &X402Config,
    client: &Client,
    signer: &dyn Signer,
    payer: &AgentId,
    call: &PaidCall<'_>,
) -> Result<Response, X402DaemonError> {
    if !config.enabled {
        return Err(X402DaemonError::Disabled);
    }

    match ctx.budget.would_exceed(payer, call.credits).await {
        Ok(false) => {}
        Ok(true) => return Err(X402DaemonError::BudgetExceeded),
        Err(BudgetError::NoCapacity(_)) => return Err(X402DaemonError::NoCapacity),
        Err(e) => return Err(X402DaemonError::Budget(e)),
    }

    let resp = client
        .request_paid(
            call.method.clone(),
            call.endpoint,
            call.body,
            &call.capability,
            signer,
        )
        .await?;

    // A free 2xx (no 402 was issued) needs no settlement. We can't
    // distinguish "paid 2xx" from "free 2xx" from the response alone,
    // so the daemon treats any success here as a settled call when the
    // endpoint is in a paid catalog. Endpoints known to be free should
    // not be routed through this path.
    if resp.status().is_success() {
        if let Err(e) = record_paid_call(ctx, payer, call).await {
            // The payment already happened; failing to record it is a
            // serious accounting gap, so surface it loudly rather than
            // swallowing.
            warn!(error = %e, endpoint = call.endpoint, "x402 paid call succeeded but accounting failed");
            return Err(e);
        }
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::InMemoryAuditLog;
    use covenant_budget::InMemoryLedger;
    use covenant_settlement::InMemorySettlement;

    fn agent(tag: u8) -> AgentId {
        AgentId::new("payer@local", [tag; 32])
    }

    fn sample_call<'a>() -> PaidCall<'a> {
        PaidCall {
            provider: "xona",
            endpoint: "https://api.xona-agent.com/img",
            method: Method::POST,
            capability: Capability {
                provider: "xona".into(),
                network: "solana:mainnet".into(),
                asset: "usdc-sol".into(),
                per_call_cap: 100_000,
            },
            body: None,
            amount: "80000".into(),
            network: "solana:mainnet".into(),
            asset: "usdc-sol".into(),
            credits: 8,
        }
    }

    #[tokio::test]
    async fn record_links_debit_receipt_and_audit() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let call = sample_call();
        let receipt_id = record_paid_call(&ctx, &payer, &call).await.expect("record");

        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].id, receipt_id);
        assert_eq!(receipts[0].credits_consumed, 8);
        assert_eq!(receipts[0].resource, ResourceKind::Tool);

        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            AuditKind::ExternalPaymentSettled { receipt_id: rid, amount, provider, .. } => {
                assert_eq!(*rid, receipt_id);
                assert_eq!(amount, "80000");
                assert_eq!(provider, "xona");
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }

        // 1000 capacity − 8 debited = 992 remaining.
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
    }

    #[tokio::test]
    async fn record_aborts_when_budget_exhausted() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(2);
        budget.set_capacity(&payer, 5).await.unwrap(); // less than 8 credits

        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let err = record_paid_call(&ctx, &payer, &sample_call())
            .await
            .expect_err("should abort");
        assert!(matches!(err, X402DaemonError::Budget(_)));

        // No partial records.
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pay_and_record_refuses_when_disabled() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(3);
        let ctx = SettlementContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let err = pay_and_record(
            &ctx,
            &X402Config::default(),
            &Client::new(reqwest::Client::new()),
            &covenant_x402::MockSigner,
            &payer,
            &sample_call(),
        )
        .await
        .expect_err("disabled");
        assert!(matches!(err, X402DaemonError::Disabled));
    }
}
