//! Daemon glue for the Hyre x402 provider profile.
//!
//! `covenant-hyre` owns the catalog and lower-level client. Daemon-owned paid
//! execution is deliberately parked: the previous budget pre-check and
//! post-payment debit were not one durable reservation and carried no
//! transaction-bound authorization or idempotency record.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_audit::AuditLog;
use covenant_budget::BudgetLedger;
use covenant_hyre::{HyreCatalog, HyreConfig, PaidExecutor, PaidRequest, PaidResponse};
use covenant_settlement::Settlement;
use covenant_types::AgentId;

use crate::x402::{X402Config, LEGACY_OUTBOUND_PARKED};

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
    _settlement: Arc<dyn Settlement>,
    _audit: Arc<dyn AuditLog>,
    _budget: Arc<dyn BudgetLedger>,
    x402: Arc<X402Config>,
    _issuer: AgentId,
    _payer: AgentId,
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
            _settlement: settlement,
            _audit: audit,
            _budget: budget,
            x402,
            _issuer: issuer,
            _payer: payer,
        }
    }
}

#[async_trait]
impl PaidExecutor for DaemonHyreExecutor {
    async fn execute(&self, _req: PaidRequest) -> Result<PaidResponse, String> {
        if !self.x402.enabled {
            return Err("x402 outbound surface is disabled".into());
        }
        Err(LEGACY_OUTBOUND_PARKED.into())
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

    /// An old `enabled` opt-in must not revive signer or network activity.
    #[tokio::test]
    async fn executor_is_parked_even_when_legacy_config_is_enabled() {
        let settlement = Arc::new(InMemorySettlement::new());
        let audit = Arc::new(InMemoryAuditLog::new());
        let budget = Arc::new(InMemoryLedger::new());
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let exec = DaemonHyreExecutor::new(
            settlement.clone(),
            audit.clone(),
            budget.clone(),
            Arc::new(enabled_x402()),
            agent(9),
            payer,
        );

        let err = exec.execute(req()).await.expect_err("parked");
        assert_eq!(err, LEGACY_OUTBOUND_PARKED);
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
        assert_eq!(budget.tokens_remaining(&agent(1)).await.unwrap(), 1000);
    }
}
