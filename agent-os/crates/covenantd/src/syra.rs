//! Daemon glue for the Syra x402 market-intelligence profile.
//!
//! Mirrors [`crate::hyre`]: `covenant-syra` owns the tool surface and the
//! x402 v2 pay loop; this binds one caller (the payer) to the daemon's
//! accounting and signs via the `covenant-x402-signer` sidecar. Every
//! Syra tool is a paid call, so each routes through budget/settlement/
//! audit. The funding key never enters the daemon.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_audit::AuditLog;
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_settlement::Settlement;
use covenant_syra::{PaidRequest, PaidResponse, SyraConfig, SyraExecutor};
use covenant_types::AgentId;
use covenant_x402::Capability;
use serde_json::Value;
use tracing::warn;

use crate::x402::{record_paid_call, PaidCall, SettlementContext, X402Config};

/// One budget credit per Syra call. The authoritative on-chain amount is
/// recorded separately from the live 402 settlement.
const CALL_CREDITS: u64 = 1;

/// Syra config, built at daemon startup and shared behind an `Arc`.
pub struct SyraState {
    pub config: SyraConfig,
}

impl SyraState {
    pub fn new(config: SyraConfig) -> Self {
        Self { config }
    }
}

/// A [`SyraExecutor`] bound to one payer and the daemon's accounting.
/// Constructed per tool call so the budget debit and settlement receipt
/// land against the agent that invoked the tool.
pub struct DaemonSyraExecutor {
    settlement: Arc<dyn Settlement>,
    audit: Arc<dyn AuditLog>,
    budget: Arc<dyn BudgetLedger>,
    x402: Arc<X402Config>,
    issuer: AgentId,
    payer: AgentId,
    http: reqwest::Client,
}

impl DaemonSyraExecutor {
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
            // Bounded client: Syra's data backend can stall (its routes
            // flap 503), and the paid retry would otherwise hang the
            // daemon path with no deadline. 30s is well above a healthy
            // round-trip and fails closed on a stalled upstream.
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }
}

#[async_trait]
impl SyraExecutor for DaemonSyraExecutor {
    async fn execute(&self, req: PaidRequest) -> Result<PaidResponse, String> {
        if !self.x402.enabled {
            return Err("x402 outbound surface is disabled".into());
        }
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| format!("invalid HTTP method: {:?}", req.method))?;

        // Read-only pre-check so the daemon never spends USDC for an agent
        // that can't afford the call. The authoritative debit happens in
        // record_paid_call after a successful settlement.
        match self.budget.would_exceed(&self.payer, CALL_CREDITS).await {
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

        let out = covenant_syra::execute_paid(&self.http, &signer, &req)
            .await
            .map_err(|e| e.to_string())?;

        let receipt_id = match &out.paid_amount {
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
                    credits: CALL_CREDITS,
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
                        warn!(error = %e, endpoint = %req.url, "syra paid call succeeded but accounting failed");
                        return Err(e.to_string());
                    }
                }
            }
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
            provider: "syra".into(),
            slug: "signal".into(),
            url: "https://api.syraa.fun/signal?token=solana".into(),
            method: "GET".into(),
            body: None,
            network: covenant_syra::config::SOLANA_NETWORK.into(),
            asset: covenant_syra::config::USDC_MINT.into(),
            per_call_cap: 100_000,
        }
    }

    fn exec(x402: X402Config, payer: AgentId, budget: Arc<dyn BudgetLedger>) -> DaemonSyraExecutor {
        DaemonSyraExecutor::new(
            Arc::new(InMemorySettlement::new()),
            Arc::new(InMemoryAuditLog::new()),
            budget,
            Arc::new(x402),
            agent(9),
            payer,
        )
    }

    #[tokio::test]
    async fn execute_refuses_when_x402_disabled() {
        let e = exec(
            X402Config::default(),
            agent(1),
            Arc::new(InMemoryLedger::new()),
        );
        let err = e.execute(req()).await.expect_err("disabled");
        assert!(err.contains("disabled"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_refuses_payer_without_budget() {
        let x402 = X402Config {
            enabled: true,
            signer_binary: "/nonexistent-signer".into(),
            signer_env: vec![],
        };
        let e = exec(x402, agent(1), Arc::new(InMemoryLedger::new()));
        let err = e.execute(req()).await.expect_err("no capacity");
        assert!(err.contains("capacity"), "got: {err}");
    }
}
