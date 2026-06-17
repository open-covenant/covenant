//! Daemon glue for the WURK agent-to-human x402 provider profile.
//!
//! Mirrors [`crate::hyre`]: `covenant-wurk` owns the tool surface and the
//! x402 v2 pay loop; this binds one caller (the payer) to the daemon's
//! accounting and signs via the `covenant-x402-signer` sidecar. Only the
//! paid `hire_humans` create goes through settlement/audit/budget; the
//! `job_status` view and `choose_winners` selection are free reads.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_audit::AuditLog;
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_settlement::Settlement;
use covenant_types::AgentId;
use covenant_wurk::{PaidRequest, PaidResponse, WurkConfig, WurkExecutor};
use covenant_x402::Capability;
use serde_json::Value;
use tracing::warn;

use crate::x402::{record_paid_call, PaidCall, SettlementContext, X402Config};

/// One budget credit per hire call. The authoritative on-chain amount is
/// recorded separately from the live 402 settlement.
const HIRE_CREDITS: u64 = 1;

/// WURK config, built at daemon startup and shared behind an `Arc`.
pub struct WurkState {
    pub config: WurkConfig,
}

impl WurkState {
    pub fn new(config: WurkConfig) -> Self {
        Self { config }
    }
}

/// A [`WurkExecutor`] bound to one payer and the daemon's accounting.
/// Constructed per tool call so the budget debit and settlement receipt
/// land against the agent that invoked the tool.
pub struct DaemonWurkExecutor {
    settlement: Arc<dyn Settlement>,
    audit: Arc<dyn AuditLog>,
    budget: Arc<dyn BudgetLedger>,
    x402: Arc<X402Config>,
    issuer: AgentId,
    payer: AgentId,
    base_url: String,
    http: reqwest::Client,
}

impl DaemonWurkExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        settlement: Arc<dyn Settlement>,
        audit: Arc<dyn AuditLog>,
        budget: Arc<dyn BudgetLedger>,
        x402: Arc<X402Config>,
        issuer: AgentId,
        payer: AgentId,
        base_url: String,
    ) -> Self {
        Self {
            settlement,
            audit,
            budget,
            x402,
            issuer,
            payer,
            base_url,
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl WurkExecutor for DaemonWurkExecutor {
    async fn create(&self, req: PaidRequest) -> Result<PaidResponse, String> {
        if !self.x402.enabled {
            return Err("x402 outbound surface is disabled".into());
        }
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| format!("invalid HTTP method: {:?}", req.method))?;

        // Read-only pre-check so the daemon never spends USDC for an agent
        // that can't afford the call. The authoritative debit happens in
        // record_paid_call after a successful settlement.
        match self.budget.would_exceed(&self.payer, HIRE_CREDITS).await {
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

        let out = covenant_wurk::execute_paid(&self.http, &signer, &req)
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
                    credits: HIRE_CREDITS,
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
                        warn!(error = %e, endpoint = %req.url, "wurk paid call succeeded but accounting failed");
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

    async fn view(&self, secret: &str, page: u32) -> Result<Value, String> {
        let url = format!(
            "{}/solana/agenttohumanadvanced?action=view&secret={}&page={}",
            self.base_url.trim_end_matches('/'),
            secret,
            page
        );
        let resp = self.http.get(&url).send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("view returned {}: {}", status.as_u16(), body));
        }
        serde_json::from_str(&body).map_err(|e| format!("decode view: {e}"))
    }

    async fn choose_winners(
        &self,
        secret: &str,
        submission_ids: Vec<String>,
    ) -> Result<Value, String> {
        let url = format!(
            "{}/api/agenttohumanadvanced/choose-winners",
            self.base_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "secret": secret, "submissionIds": submission_ids }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "choose-winners returned {}: {}",
                status.as_u16(),
                body
            ));
        }
        serde_json::from_str(&body).map_err(|e| format!("decode choose-winners: {e}"))
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
            provider: "wurk".into(),
            slug: "agenttohuman".into(),
            url: "https://wurkapi.fun/solana/agenttohuman?description=x".into(),
            method: "GET".into(),
            body: None,
            network: covenant_wurk::config::SOLANA_NETWORK.into(),
            asset: covenant_wurk::config::USDC_MINT.into(),
            per_call_cap: 10_000,
        }
    }

    fn exec(x402: X402Config, payer: AgentId, budget: Arc<dyn BudgetLedger>) -> DaemonWurkExecutor {
        DaemonWurkExecutor::new(
            Arc::new(InMemorySettlement::new()),
            Arc::new(InMemoryAuditLog::new()),
            budget,
            Arc::new(x402),
            agent(9),
            payer,
            covenant_wurk::config::BASE_URL.into(),
        )
    }

    #[tokio::test]
    async fn create_refuses_when_x402_disabled() {
        let e = exec(X402Config::default(), agent(1), Arc::new(InMemoryLedger::new()));
        let err = e.create(req()).await.expect_err("disabled");
        assert!(err.contains("disabled"), "got: {err}");
    }

    #[tokio::test]
    async fn create_refuses_payer_without_budget() {
        let x402 = X402Config {
            enabled: true,
            signer_binary: "/nonexistent-signer".into(),
            signer_env: vec![],
        };
        let e = exec(x402, agent(1), Arc::new(InMemoryLedger::new()));
        let err = e.create(req()).await.expect_err("no capacity");
        assert!(err.contains("capacity"), "got: {err}");
    }
}
