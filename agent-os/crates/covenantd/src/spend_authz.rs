//! Daemon-side pre-spend authorization for an external agent wallet.
//!
//! Covenant is the spending policy. An external wallet (e.g. OrbWallet)
//! asks the daemon to approve a spend *before* it signs: the daemon
//! checks the agent's capability scope and budget, records the verdict in
//! the audit chain, and returns approve or deny. This module holds no
//! keys and moves no funds — it decides and it records. Settlement (the
//! receipt + budget debit after the wallet actually pays) is the separate
//! `x402::record_paid_call` path; an authorization here does not debit.
//!
//! The split mirrors `x402.rs`: the capability *grant* lookup stays in the
//! daemon `Server` (it owns the capability store), which resolves a
//! [`SpendScope`] and hands it here. This module then enforces the
//! per-call cap, the chain/asset match, and the budget, and writes one
//! [`AuditKind::SpendAuthorizationDecided`] row per decision.
//!
//! Budget is an optional ceiling. When the payer has a configured budget
//! bucket it is enforced, and a real budget-subsystem failure denies
//! (fail-closed). A payer with no bucket has no cumulative ceiling, so the
//! per-call cap and the capability are the active gates. Every deny is
//! audited with its reason.

use std::time::{SystemTime, UNIX_EPOCH};

use covenant_audit::{AuditEvent, AuditKind, AuditLog};
use covenant_budget::{BudgetError, BudgetLedger};
use covenant_settlement::Settlement;
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use tracing::debug;
use uuid::Uuid;

/// Opt-in switch for the daemon's spend-authorization surface. Defaults
/// to `false` so a daemon with no operator opt-in authorizes nothing.
#[derive(Debug, Clone, Default)]
pub struct SpendAuthzConfig {
    pub enabled: bool,
}

/// The resolved spending policy for one `(agent, provider)` pair, derived
/// by the caller from a granted capability. `network` is CAIP-2, `asset`
/// is the mint (Solana) or contract (EVM) the spend must be denominated
/// in, and `per_call_cap` is the maximum atomic amount a single spend may
/// request. Per-day and total budgets are enforced separately through the
/// [`BudgetLedger`].
pub struct SpendScope {
    pub provider: String,
    pub network: String,
    pub asset: String,
    pub per_call_cap: u128,
}

/// A spend an external wallet wants the daemon to authorize before it
/// signs.
pub struct SpendRequest {
    /// CAIP-2 network the wallet intends to settle on.
    pub network: String,
    /// Asset (mint or contract) the spend is denominated in.
    pub asset: String,
    /// Atomic amount as a decimal string — stringified to preserve
    /// precision across languages that lack u128.
    pub amount: String,
    /// USD-pegged budget credits this spend would consume. The caller
    /// derives this from `amount` (e.g. via a price oracle), the same way
    /// the x402 path derives the credits it debits.
    pub credits: u64,
    /// Optional pay-to address, recorded on the audit row for triage.
    pub destination: Option<String>,
}

/// The daemon's verdict. `decision_id` is minted on every call (approve or
/// deny) and returned to the wallet so a later settlement receipt can join
/// back to the authorization that allowed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendDecision {
    Approve { decision_id: Uuid },
    Deny { decision_id: Uuid, reason: String },
}

impl SpendDecision {
    pub fn approved(&self) -> bool {
        matches!(self, SpendDecision::Approve { .. })
    }

    pub fn decision_id(&self) -> Uuid {
        match self {
            SpendDecision::Approve { decision_id } | SpendDecision::Deny { decision_id, .. } => {
                *decision_id
            }
        }
    }
}

/// Subsystem handles borrowed for the duration of one decision. `issuer`
/// is the actor recorded as the audit event's issuer (the daemon/operator
/// identity), distinct from the `payer` whose budget is checked.
pub struct AuthzContext<'a> {
    pub audit: &'a dyn AuditLog,
    pub budget: &'a dyn BudgetLedger,
    pub issuer: &'a AgentId,
}

#[derive(Debug, thiserror::Error)]
pub enum SpendAuthzError {
    #[error("spend-authorization surface is disabled")]
    Disabled,
    #[error("budget: {0}")]
    Budget(String),
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

/// Evaluates the policy without touching the audit log. `Ok(())` is an
/// approval; `Err(reason)` is a deny with an operator-readable reason.
/// Budget *system* errors deny (fail-closed) rather than propagate.
async fn evaluate(
    scope: &SpendScope,
    req: &SpendRequest,
    budget: &dyn BudgetLedger,
    payer: &AgentId,
) -> Result<(), String> {
    if req.network != scope.network {
        return Err(format!(
            "network {:?} is not allowed by this capability (allows {:?})",
            req.network, scope.network
        ));
    }
    if req.asset != scope.asset {
        return Err(format!(
            "asset {:?} is not allowed by this capability (allows {:?})",
            req.asset, scope.asset
        ));
    }
    let amount: u128 = req
        .amount
        .parse()
        .map_err(|_| format!("amount {:?} is not a decimal u128", req.amount))?;
    if amount > scope.per_call_cap {
        return Err(format!(
            "amount {} exceeds the per-call cap {}",
            amount, scope.per_call_cap
        ));
    }
    match budget.would_exceed(payer, req.credits).await {
        Ok(false) => Ok(()),
        Ok(true) => Err("spend would exceed the payer's budget".into()),
        // No bucket configured means no cumulative ceiling applies to this
        // payer: the per-call cap and the capability are the active gates,
        // and a budget is an opt-in tightening (seeded for registered
        // agents from their manifest). This is the one place spend
        // authorization diverges from the funds-moving x402 path, which
        // refuses on no-capacity because it is about to spend real money;
        // here Covenant only advises a wallet that holds its own keys.
        Err(BudgetError::NoCapacity(_)) => Ok(()),
        // A real budget-subsystem failure still denies, fail-closed.
        Err(e) => Err(format!("budget check failed: {e}")),
    }
}

/// Decides a spend-authorization request and records the verdict.
///
/// Always writes exactly one [`AuditKind::SpendAuthorizationDecided`] row
/// — on approve and on deny — so the audit chain is a complete record of
/// what the wallet was and was not permitted to spend. If the audit write
/// fails the call returns [`SpendAuthzError::Audit`] and no decision is
/// returned to the caller: a verdict the chain did not record must not be
/// acted on.
pub async fn authorize_spend(
    ctx: &AuthzContext<'_>,
    config: &SpendAuthzConfig,
    payer: &AgentId,
    scope: &SpendScope,
    req: &SpendRequest,
) -> Result<SpendDecision, SpendAuthzError> {
    if !config.enabled {
        return Err(SpendAuthzError::Disabled);
    }

    let decision_id = Uuid::new_v4();
    let outcome = evaluate(scope, req, ctx.budget, payer).await;
    let (approved, reason) = match &outcome {
        Ok(()) => (true, None),
        Err(r) => (false, Some(r.clone())),
    };

    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: ctx.issuer.clone(),
            kind: AuditKind::SpendAuthorizationDecided {
                provider: scope.provider.clone(),
                network: req.network.clone(),
                asset: req.asset.clone(),
                amount: req.amount.clone(),
                credits: req.credits,
                destination: req.destination.clone(),
                approved,
                reason: reason.clone(),
                decision_id,
            },
        })
        .await
        .map_err(|e| SpendAuthzError::Audit(e.to_string()))?;

    debug!(
        provider = scope.provider,
        network = req.network,
        approved,
        %decision_id,
        "decided spend authorization"
    );

    Ok(match outcome {
        Ok(()) => SpendDecision::Approve { decision_id },
        Err(reason) => SpendDecision::Deny {
            decision_id,
            reason,
        },
    })
}

/// Settlement facts an external wallet reports after it has paid, so the
/// daemon can record the receipt that closes the loop opened by
/// [`authorize_spend`].
pub struct SettleFacts {
    /// The `decision_id` the daemon returned from the authorization this
    /// payment acted on. Joins the settlement row back to the approval.
    pub decision_id: Uuid,
    pub provider: String,
    pub network: String,
    pub asset: String,
    /// Atomic amount actually settled, as a decimal string.
    pub amount: String,
    /// USD-pegged budget credits this spend consumed.
    pub credits: u64,
    /// On-chain transaction signature or hash, when the wallet has it.
    pub tx_sig: Option<String>,
}

/// Subsystem handles borrowed for the duration of one settlement record.
pub struct SettleContext<'a> {
    pub settlement: &'a dyn Settlement,
    pub audit: &'a dyn AuditLog,
    pub budget: &'a dyn BudgetLedger,
    pub issuer: &'a AgentId,
}

/// Records the settlement of a previously authorized spend: a budget debit
/// (when the payer has a bucket), a [`SettlementReceipt`], and one
/// [`AuditKind::SpendSettled`] row, all sharing the receipt id and carrying
/// the originating `decision_id`. Returns the receipt id.
///
/// The payment has already happened on-chain by the time this is called, so
/// the budget debit is best-effort against an unconfigured payer: no bucket
/// means no cumulative ledger to debit, matching [`authorize_spend`]'s
/// treatment of an unconfigured budget. A real budget- or
/// settlement-subsystem failure is surfaced — the operator has an
/// accounting gap to reconcile — rather than silently dropped. Order is
/// debit, then receipt, then audit, so the logs never carry a half-recorded
/// settlement.
///
/// Idempotent on `decision_id`: a wallet retries settlement when our
/// success response is lost or a settle failed after the debit landed (the
/// reference OrbWallet client retries 3× automatically and exposes a manual
/// `retryFailedSettlement(decisionId)`). A repeat for a `decision_id` that
/// already settled returns the original receipt id without debiting or
/// recording again, so one on-chain payment yields exactly one debit and
/// one `spend_settled` row. This covers the sequential retries a client
/// actually issues; it does not serialize two settles racing on the same
/// `decision_id`, which the client does not produce.
///
/// The `decision_id` is recorded for correlation; this path does not yet
/// verify it names a prior *approved* authorization. The caller is
/// authenticated and capability-gated, so this is an accounting join, not
/// an enforcement gate; binding settlement to a stored approval is a
/// planned tightening.
pub async fn record_spend_settlement(
    ctx: &SettleContext<'_>,
    config: &SpendAuthzConfig,
    payer: &AgentId,
    facts: &SettleFacts,
) -> Result<Uuid, SpendAuthzError> {
    if !config.enabled {
        return Err(SpendAuthzError::Disabled);
    }

    if let Some(receipt_id) = ctx
        .audit
        .settled_receipt_for(facts.decision_id)
        .await
        .map_err(|e| SpendAuthzError::Audit(e.to_string()))?
    {
        debug!(
            decision_id = %facts.decision_id,
            %receipt_id,
            "spend settlement already recorded; returning original receipt"
        );
        return Ok(receipt_id);
    }

    let receipt_id = Uuid::new_v4();
    let now = epoch_ms();

    match ctx.budget.try_debit(payer, facts.credits, receipt_id).await {
        Ok(()) => {}
        // No bucket configured: nothing to debit, consistent with the
        // optional-ceiling model the authorize path uses.
        Err(BudgetError::NoCapacity(_)) => {}
        Err(e) => return Err(SpendAuthzError::Budget(e.to_string())),
    }

    ctx.settlement
        .record(SettlementReceipt {
            id: receipt_id,
            payer: payer.clone(),
            resource: ResourceKind::Tool,
            memory_record_id: None,
            credits_consumed: facts.credits,
            settled_at: now,
            chain: None,
            cluster: None,
            batch_id: None,
            merkle_root: None,
            tx_sig: facts.tx_sig.clone(),
            slot: None,
            confirmed_at: None,
            onchain_sig: None,
        })
        .await
        .map_err(|e| SpendAuthzError::Settlement(e.to_string()))?;

    ctx.audit
        .record(AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: now,
            issuer: ctx.issuer.clone(),
            kind: AuditKind::SpendSettled {
                decision_id: facts.decision_id,
                receipt_id,
                provider: facts.provider.clone(),
                network: facts.network.clone(),
                asset: facts.asset.clone(),
                amount: facts.amount.clone(),
                credits: facts.credits,
                tx_sig: facts.tx_sig.clone(),
            },
        })
        .await
        .map_err(|e| SpendAuthzError::Audit(e.to_string()))?;

    debug!(
        provider = facts.provider,
        %receipt_id,
        decision_id = %facts.decision_id,
        "recorded spend settlement"
    );
    Ok(receipt_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covenant_audit::InMemoryAuditLog;
    use covenant_budget::InMemoryLedger;

    fn agent(tag: u8) -> AgentId {
        AgentId::new("payer@local", [tag; 32])
    }

    fn scope() -> SpendScope {
        SpendScope {
            provider: "orbserv".into(),
            network: "eip155:8453".into(),
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            per_call_cap: 100_000,
        }
    }

    fn request() -> SpendRequest {
        SpendRequest {
            network: "eip155:8453".into(),
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            amount: "80000".into(),
            credits: 8,
            destination: Some("0xPayee".into()),
        }
    }

    fn enabled() -> SpendAuthzConfig {
        SpendAuthzConfig { enabled: true }
    }

    async fn ctx_with<'a>(
        audit: &'a InMemoryAuditLog,
        budget: &'a InMemoryLedger,
        issuer: &'a AgentId,
    ) -> AuthzContext<'a> {
        AuthzContext {
            audit,
            budget,
            issuer,
        }
    }

    #[tokio::test]
    async fn approves_within_cap_and_budget_and_audits() {
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let ctx = ctx_with(&audit, &budget, &issuer).await;
        let decision = authorize_spend(&ctx, &enabled(), &payer, &scope(), &request())
            .await
            .expect("decide");
        assert!(decision.approved(), "should approve: {decision:?}");

        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            AuditKind::SpendAuthorizationDecided {
                approved,
                reason,
                credits,
                decision_id,
                provider,
                ..
            } => {
                assert!(approved);
                assert!(reason.is_none());
                assert_eq!(*credits, 8);
                assert_eq!(*decision_id, decision.decision_id());
                assert_eq!(provider, "orbserv");
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }
        // Authorization must not debit; the budget is untouched.
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 1000);
    }

    #[tokio::test]
    async fn denies_when_amount_exceeds_per_call_cap() {
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(2);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let mut req = request();
        req.amount = "100001".into(); // cap is 100_000

        let ctx = ctx_with(&audit, &budget, &issuer).await;
        let decision = authorize_spend(&ctx, &enabled(), &payer, &scope(), &req)
            .await
            .expect("decide");
        match decision {
            SpendDecision::Deny { reason, .. } => assert!(reason.contains("per-call cap")),
            other => panic!("expected deny, got {other:?}"),
        }
        // A deny is still audited.
        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            AuditKind::SpendAuthorizationDecided {
                approved, reason, ..
            } => {
                assert!(!approved);
                assert!(reason.as_deref().unwrap().contains("per-call cap"));
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn denies_on_chain_or_asset_mismatch() {
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(3);
        budget.set_capacity(&payer, 1000).await.unwrap();
        let ctx = ctx_with(&audit, &budget, &issuer).await;

        let mut wrong_chain = request();
        wrong_chain.network = "solana:mainnet".into();
        let d = authorize_spend(&ctx, &enabled(), &payer, &scope(), &wrong_chain)
            .await
            .unwrap();
        assert!(!d.approved());

        let mut wrong_asset = request();
        wrong_asset.asset = "0xdeadbeef".into();
        let d = authorize_spend(&ctx, &enabled(), &payer, &scope(), &wrong_asset)
            .await
            .unwrap();
        assert!(!d.approved());
    }

    #[tokio::test]
    async fn denies_when_budget_would_exceed_without_debiting() {
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(4);
        budget.set_capacity(&payer, 5).await.unwrap(); // < 8 credits

        let ctx = ctx_with(&audit, &budget, &issuer).await;
        let decision = authorize_spend(&ctx, &enabled(), &payer, &scope(), &request())
            .await
            .expect("decide");
        match decision {
            SpendDecision::Deny { reason, .. } => assert!(reason.contains("budget")),
            other => panic!("expected deny, got {other:?}"),
        }
        // Fail-closed deny leaves the budget untouched.
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn approves_when_no_budget_bucket_configured() {
        // No bucket means no cumulative ceiling; the per-call cap and the
        // capability still gate. (x402 refuses here because it moves
        // funds; spend authorization only advises a key-holding wallet.)
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(5); // never given capacity

        let ctx = ctx_with(&audit, &budget, &issuer).await;
        let decision = authorize_spend(&ctx, &enabled(), &payer, &scope(), &request())
            .await
            .expect("decide");
        assert!(decision.approved(), "no budget bucket must not block");
        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1, "the approve is still recorded");
    }

    #[tokio::test]
    async fn refuses_when_disabled() {
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(6);
        let ctx = ctx_with(&audit, &budget, &issuer).await;
        let err = authorize_spend(
            &ctx,
            &SpendAuthzConfig::default(),
            &payer,
            &scope(),
            &request(),
        )
        .await
        .expect_err("disabled");
        assert!(matches!(err, SpendAuthzError::Disabled));
        // Disabled surface records nothing.
        assert!(audit.recent(10).await.unwrap().is_empty());
    }

    fn settle_facts() -> SettleFacts {
        SettleFacts {
            decision_id: Uuid::from_u128(0x0abc),
            provider: "orbserv".into(),
            network: "eip155:8453".into(),
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            amount: "80000".into(),
            credits: 8,
            tx_sig: Some("0xsig".into()),
        }
    }

    #[tokio::test]
    async fn settlement_records_receipt_audit_and_debits_when_budgeted() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let facts = settle_facts();
        let receipt_id = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("settle");

        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].id, receipt_id);
        assert_eq!(receipts[0].credits_consumed, 8);
        assert_eq!(receipts[0].tx_sig.as_deref(), Some("0xsig"));

        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            AuditKind::SpendSettled {
                decision_id,
                receipt_id: rid,
                tx_sig,
                credits,
                ..
            } => {
                assert_eq!(*rid, receipt_id);
                assert_eq!(*decision_id, facts.decision_id);
                assert_eq!(tx_sig.as_deref(), Some("0xsig"));
                assert_eq!(*credits, 8);
            }
            other => panic!("unexpected audit kind: {other:?}"),
        }
        // 1000 capacity - 8 debited = 992 remaining.
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
    }

    #[tokio::test]
    async fn settlement_is_idempotent_on_decision_id() {
        // A retried settlement (lost response, or a retry of one that failed
        // after the debit) must join the original receipt: one debit, one
        // receipt, one audit row, no matter how many times it is replayed.
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();

        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let facts = settle_facts();

        let first = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("first settle");
        let second = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("retry settle");
        let third = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("retry settle again");

        assert_eq!(first, second, "retry returns the original receipt id");
        assert_eq!(first, third);
        assert_eq!(settlement.recent(10).await.unwrap().len(), 1, "one receipt");
        assert_eq!(audit.recent(10).await.unwrap().len(), 1, "one settled row");
        // Debited exactly once: 1000 - 8 = 992, not 976.
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
    }

    #[tokio::test]
    async fn settlement_records_without_debit_when_no_bucket() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(2); // never given capacity

        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let receipt_id = record_spend_settlement(&ctx, &enabled(), &payer, &settle_facts())
            .await
            .expect("settle");
        assert_eq!(
            settlement.recent(10).await.unwrap().len(),
            1,
            "receipt is recorded even with no budget bucket"
        );
        assert_eq!(audit.recent(10).await.unwrap().len(), 1);
        assert!(!receipt_id.is_nil());
    }

    #[tokio::test]
    async fn settlement_refuses_when_disabled() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(3);
        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let err =
            record_spend_settlement(&ctx, &SpendAuthzConfig::default(), &payer, &settle_facts())
                .await
                .expect_err("disabled");
        assert!(matches!(err, SpendAuthzError::Disabled));
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(audit.recent(10).await.unwrap().is_empty());
    }
}
