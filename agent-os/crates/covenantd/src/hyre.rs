//! Daemon glue for the Hyre x402 provider profile.
//!
//! `covenant-hyre` owns the catalog and the MCP tool surface but knows
//! nothing about the daemon's funding key, budget, or settlement logs —
//! it reaches them through a [`covenant_hyre::PaidExecutor`]. This
//! module is that implementation: [`DaemonHyreExecutor`] binds one
//! caller (the payer) to the daemon's accounting subsystems and routes
//! a resolved Hyre call through the same [`crate::x402::pay_and_record`]
//! path every other outbound x402 payment uses. A Hyre receipt is a
//! plain `ResourceKind::Tool` receipt — it rolls into the same Merkle
//! batch and optional Synapse mirror with no Hyre-specific surface.
//!
//! The funding key never enters the daemon: signing is delegated to the
//! `covenant-x402-signer` sidecar via [`crate::x402::SubprocessSigner`].

use std::sync::Arc;

use async_trait::async_trait;
use covenant_audit::AuditLog;
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_hyre::{HyreCatalog, HyreConfig, PaidExecutor, PaidRequest, PaidResponse};
use covenant_settlement::Settlement;
use covenant_types::AgentId;
use covenant_x402::Capability;
use serde_json::Value;
use tracing::warn;

use crate::x402::{record_paid_call, PaidCall, SettlementContext, X402Config};

/// Materialised Hyre catalog plus its config, built once at daemon
/// startup and shared behind an `Arc`. Rebuilt out of band when the
/// daemon refreshes the manifest.
pub struct HyreState {
    pub catalog: HyreCatalog,
    pub config: HyreConfig,
}

impl HyreState {
    pub fn new(catalog: HyreCatalog, config: HyreConfig) -> Self {
        Self { catalog, config }
    }
}

/// A [`PaidExecutor`] bound to one payer and the daemon's accounting
/// subsystems. Constructed per tool call so the budget debit and
/// settlement receipt land against the agent that invoked the tool.
pub struct DaemonHyreExecutor {
    settlement: Arc<dyn Settlement>,
    audit: Arc<dyn AuditLog>,
    budget: Arc<dyn BudgetLedger>,
    x402: Arc<X402Config>,
    issuer: AgentId,
    payer: AgentId,
}

impl DaemonHyreExecutor {
    pub fn new(
        settlement: Arc<dyn Settlement>,
        audit: Arc<dyn AuditLog>,
        budget: Arc<dyn BudgetLedger>,
        x402: Arc<X402Config>,
        issuer: AgentId,
        payer: AgentId,
    ) -> Self {
        Self {
            settlement,
            audit,
            budget,
            x402,
            issuer,
            payer,
        }
    }
}

#[async_trait]
impl PaidExecutor for DaemonHyreExecutor {
    async fn execute(&self, req: PaidRequest) -> Result<PaidResponse, String> {
        if !self.x402.enabled {
            return Err("x402 outbound surface is disabled".into());
        }
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| format!("invalid HTTP method: {:?}", req.method))?;

        // Read-only pre-check so the daemon never spends USDC for an
        // agent that can't afford the credits. The authoritative debit
        // happens inside record_paid_call after a successful response.
        match self.budget.would_exceed(&self.payer, req.credits).await {
            Ok(false) => {}
            Ok(true) => return Err("payer budget would be exceeded by this call".into()),
            Err(BudgetError::NoCapacity(_)) => {
                return Err("payer has no budget capacity; refusing to spend".into())
            }
            Err(e) => return Err(format!("budget: {e}")),
        }

        let mut signer = crate::x402::SubprocessSigner::new(&self.x402.signer_binary);
        for (k, v) in &self.x402.signer_env {
            signer = signer.env(k.clone(), v.clone());
        }

        let http = reqwest::Client::new();
        let out = covenant_hyre::execute_paid(&http, &signer, &req)
            .await
            .map_err(|e| e.to_string())?;

        let receipt_id = match &out.paid_amount {
            // A 402 was answered and paid: record the live, authoritative
            // amount against the caller.
            Some(amount) if (200..300).contains(&out.status) => {
                let capability = Capability {
                    provider: req.provider.clone(),
                    network: req.network.clone(),
                    asset: req.asset.clone(),
                    per_call_cap: req.per_call_cap,
                };
                let call = PaidCall {
                    provider: &req.provider,
                    endpoint: &req.url,
                    method,
                    capability,
                    body: req.body.as_ref(),
                    amount: amount.clone(),
                    network: req.network.clone(),
                    asset: req.asset.clone(),
                    credits: req.credits,
                };
                let ctx = SettlementContext {
                    settlement: self.settlement.as_ref(),
                    audit: self.audit.as_ref(),
                    budget: self.budget.as_ref(),
                    issuer: &self.issuer,
                };
                match record_paid_call(&ctx, &self.payer, &call).await {
                    Ok(id) => Some(id),
                    Err(e) => {
                        // Payment already settled on-chain; failing to record
                        // it is an accounting gap, so surface it loudly.
                        warn!(error = %e, endpoint = %req.url, "hyre paid call succeeded but accounting failed");
                        return Err(e.to_string());
                    }
                }
            }
            // Free 2xx, or a paid attempt that didn't return success: no debit.
            _ => None,
        };

        let body = serde_json::from_str(&out.body).unwrap_or(Value::String(out.body));
        Ok(PaidResponse {
            status: out.status,
            body,
            receipt_id: receipt_id.map(|id| id.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::InMemoryAuditLog;
    use covenant_budget::InMemoryLedger;
    use covenant_settlement::InMemorySettlement;

    fn agent(tag: u8) -> AgentId {
        AgentId::new("agent@local", [tag; 32])
    }

    fn req() -> PaidRequest {
        PaidRequest {
            provider: "hyre".into(),
            slug: "defi/tvl".into(),
            url: "https://mpp.hyreagent.fun/defi/tvl".into(),
            method: "GET".into(),
            body: None,
            network: covenant_hyre::config::SOLANA_NETWORK.into(),
            asset: covenant_hyre::config::USDC_MINT.into(),
            per_call_cap: 10_000,
            credits: 1,
            price_micro_usdc: 10_000,
        }
    }

    fn enabled_x402() -> X402Config {
        X402Config {
            enabled: true,
            signer_binary: "/nonexistent-signer".into(),
            signer_env: vec![],
        }
    }

    /// With the funding sidecar disabled the executor must refuse
    /// before any network or signer activity and write no accounting.
    #[tokio::test]
    async fn executor_refuses_when_x402_disabled() {
        let settlement = Arc::new(InMemorySettlement::new());
        let audit = Arc::new(InMemoryAuditLog::new());
        let budget = Arc::new(InMemoryLedger::new());
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let exec = DaemonHyreExecutor::new(
            settlement.clone(),
            audit.clone(),
            budget.clone(),
            Arc::new(X402Config::default()), // enabled = false
            agent(9),
            payer,
        );

        let err = exec.execute(req()).await.expect_err("disabled");
        assert!(err.contains("disabled"), "got: {err}");
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    /// A payer with no budget bucket is refused before any network or
    /// signer activity — the read-only pre-check fires first.
    #[tokio::test]
    async fn executor_refuses_payer_without_budget() {
        let exec = DaemonHyreExecutor::new(
            Arc::new(InMemorySettlement::new()),
            Arc::new(InMemoryAuditLog::new()),
            Arc::new(InMemoryLedger::new()),
            Arc::new(enabled_x402()),
            agent(9),
            agent(1), // never given capacity
        );
        let err = exec.execute(req()).await.expect_err("no capacity");
        assert!(err.contains("capacity"), "got: {err}");
    }

    #[tokio::test]
    async fn executor_rejects_bad_method_after_budget_check() {
        let budget = Arc::new(InMemoryLedger::new());
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();
        let exec = DaemonHyreExecutor::new(
            Arc::new(InMemorySettlement::new()),
            Arc::new(InMemoryAuditLog::new()),
            budget,
            Arc::new(enabled_x402()),
            agent(9),
            payer,
        );
        let mut bad = req();
        bad.method = "BAD METHOD".into(); // space is not a valid method token
        let err = exec.execute(bad).await.expect_err("bad method");
        assert!(err.contains("invalid HTTP method"), "got: {err}");
    }
}
