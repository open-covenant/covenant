//! Daemon-side advisory preflight for an external agent wallet.
//!
//! An external wallet can ask the daemon for a decision before it signs. The
//! current HTTP caller supplies both the proposed spend and its network, asset,
//! per-call cap, and budget units. The returned UUID is not bound to transaction
//! bytes, and the signer does not require or consume it. This module therefore
//! records an advisory decision; it is not the wallet's signing boundary. A
//! decision does not debit. The later [`record_spend_settlement`] path requires
//! the stored decision to be approved and to match its payer and reported spend
//! facts, but it still trusts the authenticated wallet's transaction report and
//! does not verify the transaction on chain.
//!
//! The split mirrors `x402.rs`: the capability *grant* lookup stays in the
//! daemon `Server` (it owns the capability store), which resolves a
//! [`SpendScope`] and hands it here. This module then enforces the
//! per-call cap, the chain/asset match, and the optional budget, and writes one
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
use sha2::{Digest, Sha256};
use tracing::debug;
use uuid::Uuid;

const SPEND_RECEIPT_DOMAIN: &[u8] = b"covenant.spend-settlement.receipt.v2";
const SPEND_BINDING_DOMAIN: &[u8] = b"covenant.spend-settlement.binding.v1";
static SPEND_SETTLEMENT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// A spend an external wallet wants the daemon to evaluate before it signs.
pub struct SpendRequest {
    /// CAIP-2 network the wallet intends to settle on.
    pub network: String,
    /// Asset (mint or contract) the spend is denominated in.
    pub asset: String,
    /// Atomic amount as a decimal string — stringified to preserve
    /// precision across languages that lack u128.
    pub amount: String,
    /// Caller-supplied budget units this spend would consume. This module
    /// does not independently derive them from `amount`.
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

fn spend_receipt_id(decision_id: Uuid, payer: &AgentId) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(SPEND_RECEIPT_DOMAIN);
    hasher.update([0]);
    hasher.update(decision_id.as_bytes());
    hasher.update(payer.pubkey);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

fn hash_binding_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn spend_settlement_binding(
    payer: &AgentId,
    facts: &SettleFacts,
    authorized_destination: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SPEND_BINDING_DOMAIN);
    hasher.update([0]);
    hash_binding_field(&mut hasher, facts.decision_id.as_bytes());
    hash_binding_field(&mut hasher, &payer.pubkey);
    hash_binding_field(&mut hasher, facts.provider.as_bytes());
    hash_binding_field(&mut hasher, facts.network.as_bytes());
    hash_binding_field(&mut hasher, facts.asset.as_bytes());
    hash_binding_field(&mut hasher, facts.amount.as_bytes());
    hash_binding_field(&mut hasher, &facts.credits.to_be_bytes());
    match authorized_destination {
        Some(destination) => {
            hasher.update([1]);
            hash_binding_field(&mut hasher, destination.as_bytes());
        }
        None => hasher.update([0]),
    }
    match &facts.tx_sig {
        Some(tx_sig) => {
            hasher.update([1]);
            hash_binding_field(&mut hasher, tx_sig.as_bytes());
        }
        None => hasher.update([0]),
    }

    use std::fmt::Write as _;
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn receipt_matches_settlement(
    receipt: &SettlementReceipt,
    payer: &AgentId,
    facts: &SettleFacts,
) -> bool {
    receipt.payer.pubkey == payer.pubkey
        && receipt.resource == ResourceKind::Tool
        && receipt.memory_record_id.is_none()
        && receipt.credits_consumed == facts.credits
        && receipt.tx_sig == facts.tx_sig
}

fn completed_settlement_receipt(
    events: &[AuditEvent],
    receipts: &[SettlementReceipt],
    payer: &AgentId,
    facts: &SettleFacts,
) -> Result<Option<Uuid>, SpendAuthzError> {
    let expected_receipt_id = spend_receipt_id(facts.decision_id, payer);
    let mut completed_receipt_id = None;

    for event in events {
        let AuditKind::SpendSettled {
            decision_id,
            receipt_id,
            provider,
            network,
            asset,
            amount,
            credits,
            tx_sig,
        } = &event.kind
        else {
            continue;
        };
        if *decision_id != facts.decision_id {
            continue;
        }
        if *receipt_id != expected_receipt_id
            || provider != &facts.provider
            || network != &facts.network
            || asset != &facts.asset
            || amount != &facts.amount
            || *credits != facts.credits
            || tx_sig != &facts.tx_sig
            || completed_receipt_id.is_some()
        {
            return Err(SpendAuthzError::Settlement(format!(
                "settlement idempotency conflict for decision {}",
                facts.decision_id
            )));
        }
        completed_receipt_id = Some(*receipt_id);
    }

    let Some(receipt_id) = completed_receipt_id else {
        return Ok(None);
    };
    let matching_receipts = receipts
        .iter()
        .filter(|receipt| receipt.id == receipt_id)
        .collect::<Vec<_>>();
    if matching_receipts.len() != 1
        || matching_receipts
            .iter()
            .any(|receipt| !receipt_matches_settlement(receipt, payer, facts))
    {
        return Err(SpendAuthzError::Settlement(format!(
            "completed settlement receipt {receipt_id} is missing or conflicts"
        )));
    }
    Ok(Some(receipt_id))
}

fn validate_settlement_authorization(
    events: &[AuditEvent],
    payer: &AgentId,
    facts: &SettleFacts,
) -> Result<Option<String>, SpendAuthzError> {
    let mut authorized_destination: Option<Option<String>> = None;
    for event in events {
        let AuditKind::SpendAuthorizationDecided {
            provider,
            payer: authorized_payer,
            network,
            asset,
            amount,
            credits,
            destination,
            approved,
            decision_id,
            ..
        } = &event.kind
        else {
            continue;
        };
        if *decision_id != facts.decision_id {
            continue;
        }
        if !*approved
            || authorized_payer
                .as_ref()
                .is_none_or(|authorized| authorized.pubkey != payer.pubkey)
            || provider != &facts.provider
            || network != &facts.network
            || asset != &facts.asset
            || amount != &facts.amount
            || *credits != facts.credits
        {
            return Err(SpendAuthzError::Settlement(format!(
                "decision {} is not an approved authorization for these payer and spend facts",
                facts.decision_id
            )));
        }
        if authorized_destination.is_some() {
            return Err(SpendAuthzError::Settlement(format!(
                "decision {} has multiple authorization rows",
                facts.decision_id
            )));
        }
        authorized_destination = Some(destination.clone());
    }
    let Some(destination) = authorized_destination else {
        return Err(SpendAuthzError::Settlement(format!(
            "no stored authorization for decision {}",
            facts.decision_id
        )));
    };
    Ok(destination)
}

fn map_settlement_budget_error(error: BudgetError) -> SpendAuthzError {
    match error {
        BudgetError::IdempotencyConflict { paired_receipt } => SpendAuthzError::Settlement(
            format!("settlement idempotency conflict for receipt {paired_receipt}"),
        ),
        other => SpendAuthzError::Budget(other.to_string()),
    }
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

/// Evaluates a spend-preflight request and records the advisory verdict.
///
/// Always writes exactly one [`AuditKind::SpendAuthorizationDecided`] row
/// — on approve and on deny — so the audit chain records the decisions this
/// endpoint returned. It does not prove that the wallet honored them. If the audit write
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
                payer: Some(payer.clone()),
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
#[derive(Clone)]
pub struct SettleFacts {
    /// The `decision_id` the daemon returned from the authorization this
    /// payment acted on. Joins the settlement row back to the approval.
    pub decision_id: Uuid,
    pub provider: String,
    pub network: String,
    pub asset: String,
    /// Wallet-reported atomic amount, as a decimal string. Settlement requires
    /// it to match the stored authorization exactly; it is not derived from the
    /// transaction.
    pub amount: String,
    /// Wallet-reported USD-pegged budget credits. Settlement requires an exact
    /// match to the stored authorization; it does not derive credits from the
    /// amount or transaction.
    pub credits: u64,
    /// Wallet-reported transaction signature or hash, when available.
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
/// The authenticated caller reports that payment has happened; this function
/// does not verify the transaction or reported facts on chain. Before touching
/// accounting it requires a stored approved authorization with the same payer,
/// provider, network, asset, amount, and credits. The authorization's stored
/// destination is carried into the persisted binding rather than accepted again
/// from the settlement caller. It then binds a payer-namespaced deterministic
/// receipt id to that destination and every reported fact, including `tx_sig`.
/// Exact retries reuse a prior debit or compacted debit tombstone, receipt, and
/// completed audit row; changed facts fail with an idempotency conflict. A
/// process-local lock serializes this path. The three stores are still not one
/// transaction, but failures between writes are recoverable without a second
/// debit. Legacy authorization rows without a payer and older partial receipts
/// without a fact-binding claim are refused for reconciliation rather than
/// adopted. The authorization audit row must still be retained when settlement
/// or a retry is evaluated.
///
/// The budget debit is best-effort against an unconfigured payer: no bucket
/// means no cumulative ledger to debit, matching [`authorize_spend`]'s treatment
/// of an unconfigured budget. A real budget- or settlement-subsystem failure is
/// surfaced rather than silently dropped. The JSONL backends serialize writes
/// within one process; this function does not provide cross-process locking or
/// an `fsync` durability guarantee.
///
/// Each call scans the retained audit and receipt logs while holding the
/// process-local settlement lock. A fully completed legacy row without a claim
/// can be replayed only while its matching audit row and receipt remain
/// available. No claim is backfilled because older compaction may have
/// discarded the corresponding debit idempotency key.
///
/// This remains accounting enforcement, not proof of payment: Covenant does
/// not inspect transaction bytes, verify `tx_sig` on chain, or confirm that the
/// authorization's optional destination received funds. The external wallet
/// remains the signing and transaction-submission boundary.
pub async fn record_spend_settlement(
    ctx: &SettleContext<'_>,
    config: &SpendAuthzConfig,
    payer: &AgentId,
    facts: &SettleFacts,
) -> Result<Uuid, SpendAuthzError> {
    if !config.enabled {
        return Err(SpendAuthzError::Disabled);
    }

    let _guard = SPEND_SETTLEMENT_LOCK.lock().await;

    let receipt_id = spend_receipt_id(facts.decision_id, payer);
    let audit_events = ctx
        .audit
        .recent(usize::MAX)
        .await
        .map_err(|e| SpendAuthzError::Audit(e.to_string()))?;
    let authorized_destination = validate_settlement_authorization(&audit_events, payer, facts)?;
    let binding_sha256 = spend_settlement_binding(payer, facts, authorized_destination.as_deref());
    let receipts = ctx
        .settlement
        .recent(usize::MAX)
        .await
        .map_err(|e| SpendAuthzError::Settlement(e.to_string()))?;

    if let Some(receipt_id) = completed_settlement_receipt(&audit_events, &receipts, payer, facts)?
    {
        debug!(
            decision_id = %facts.decision_id,
            %receipt_id,
            "spend settlement already recorded; returning original receipt"
        );
        return Ok(receipt_id);
    }

    let existing = receipts
        .iter()
        .filter(|receipt| receipt.id == receipt_id)
        .collect::<Vec<_>>();

    if existing.is_empty() {
        ctx.budget
            .claim_debit(payer, facts.credits, receipt_id, &binding_sha256)
            .await
            .map_err(map_settlement_budget_error)?;
    } else if !ctx
        .budget
        .debit_claim_matches(payer, facts.credits, receipt_id, &binding_sha256)
        .await
        .map_err(map_settlement_budget_error)?
    {
        return Err(SpendAuthzError::Settlement(format!(
            "receipt {receipt_id} predates settlement fact binding; reconciliation required"
        )));
    }

    let now = if let Some(receipt) = existing.first() {
        if existing.len() != 1
            || existing
                .iter()
                .any(|receipt| !receipt_matches_settlement(receipt, payer, facts))
        {
            return Err(SpendAuthzError::Settlement(format!(
                "receipt idempotency conflict for {receipt_id}"
            )));
        }
        receipt.settled_at
    } else {
        match ctx.budget.try_debit(payer, facts.credits, receipt_id).await {
            Ok(()) => {}
            // No bucket configured: nothing to debit, consistent with the
            // optional-ceiling model the authorize path uses.
            Err(BudgetError::NoCapacity(_)) => {}
            Err(e) => return Err(map_settlement_budget_error(e)),
        }

        let now = epoch_ms();
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
        now
    };

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
    use covenant_audit::{
        AuditError, AuditEvent, AuditIntegrityReport, AuditLog, InMemoryAuditLog,
    };
    use covenant_budget::InMemoryLedger;
    use covenant_settlement::{ChainConfirmation, InMemorySettlement, SettlementError};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FailOnceAudit {
        inner: InMemoryAuditLog,
        fail_next_settlement: AtomicBool,
    }

    impl FailOnceAudit {
        fn new() -> Self {
            Self {
                inner: InMemoryAuditLog::new(),
                fail_next_settlement: AtomicBool::new(true),
            }
        }
    }

    #[async_trait::async_trait]
    impl AuditLog for FailOnceAudit {
        async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
            if matches!(&event.kind, AuditKind::SpendSettled { .. })
                && self.fail_next_settlement.swap(false, Ordering::SeqCst)
            {
                return Err(AuditError::Io(std::io::Error::other(
                    "injected spend settlement audit failure",
                )));
            }
            self.inner.record(event).await
        }

        async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, AuditError> {
            self.inner.recent(limit).await
        }

        async fn purge_older_than(&self, before_ms: u64) -> Result<u64, AuditError> {
            self.inner.purge_older_than(before_ms).await
        }

        async fn verify_integrity(&self) -> Result<AuditIntegrityReport, AuditError> {
            self.inner.verify_integrity().await
        }
    }

    struct FailOnceSettlement {
        inner: InMemorySettlement,
        fail_next_record: AtomicBool,
    }

    impl FailOnceSettlement {
        fn new() -> Self {
            Self {
                inner: InMemorySettlement::new(),
                fail_next_record: AtomicBool::new(true),
            }
        }
    }

    #[async_trait::async_trait]
    impl Settlement for FailOnceSettlement {
        async fn record(&self, receipt: SettlementReceipt) -> Result<(), SettlementError> {
            if self.fail_next_record.swap(false, Ordering::SeqCst) {
                return Err(SettlementError::Io(std::io::Error::other(
                    "injected settlement receipt failure",
                )));
            }
            self.inner.record(receipt).await
        }

        async fn recent(&self, limit: usize) -> Result<Vec<SettlementReceipt>, SettlementError> {
            self.inner.recent(limit).await
        }

        async fn mark_batch_confirmed(
            &self,
            receipt_ids: &[Uuid],
            confirmation: ChainConfirmation,
        ) -> Result<u64, SettlementError> {
            self.inner
                .mark_batch_confirmed(receipt_ids, confirmation)
                .await
        }
    }

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
                payer: authorized_payer,
                ..
            } => {
                assert!(approved);
                assert!(reason.is_none());
                assert_eq!(*credits, 8);
                assert_eq!(*decision_id, decision.decision_id());
                assert_eq!(provider, "orbserv");
                assert_eq!(
                    authorized_payer.as_ref().map(|agent| agent.pubkey),
                    Some(payer.pubkey)
                );
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

    async fn seed_approved_authorization(
        audit: &dyn AuditLog,
        issuer: &AgentId,
        payer: &AgentId,
        facts: &SettleFacts,
    ) {
        audit
            .record(AuditEvent {
                id: Uuid::new_v4(),
                timestamp_ms: epoch_ms(),
                issuer: issuer.clone(),
                kind: AuditKind::SpendAuthorizationDecided {
                    provider: facts.provider.clone(),
                    payer: Some(payer.clone()),
                    network: facts.network.clone(),
                    asset: facts.asset.clone(),
                    amount: facts.amount.clone(),
                    credits: facts.credits,
                    destination: None,
                    approved: true,
                    reason: None,
                    decision_id: facts.decision_id,
                },
            })
            .await
            .unwrap();
    }

    #[test]
    fn spend_receipt_identity_is_stable_and_namespaced() {
        let decision_id = Uuid::from_u128(0x0abc);
        let payer = agent(1);
        let receipt_id = spend_receipt_id(decision_id, &payer);

        assert_eq!(
            receipt_id,
            Uuid::parse_str("bd79e7ee-82d6-89cb-b5bd-88220a595ed9").unwrap()
        );
        assert_eq!(receipt_id.get_version(), Some(uuid::Version::Custom));
        assert_ne!(receipt_id, decision_id);
        assert_ne!(
            receipt_id,
            spend_receipt_id(Uuid::from_u128(0x0abd), &payer)
        );
        assert_ne!(receipt_id, spend_receipt_id(decision_id, &agent(2)));
    }

    #[test]
    fn settlement_binding_commits_every_reported_fact() {
        let payer = agent(1);
        let facts = settle_facts();
        let binding = spend_settlement_binding(&payer, &facts, Some("0xPayee"));
        assert_eq!(binding.len(), 64);
        assert_eq!(
            binding,
            "80fd4fe4cb77741a408e9f709d6a7d92b9eedbcb670ae168af549629e01f2ddf"
        );

        let mut changed = Vec::new();
        let mut decision = facts.clone();
        decision.decision_id = Uuid::from_u128(0x0abd);
        changed.push(decision);
        let mut provider = facts.clone();
        provider.provider = "other-provider".into();
        changed.push(provider);
        let mut network = facts.clone();
        network.network = "solana:mainnet".into();
        changed.push(network);
        let mut asset = facts.clone();
        asset.asset = "other-asset".into();
        changed.push(asset);
        let mut amount = facts.clone();
        amount.amount = "79999".into();
        changed.push(amount);
        let mut credits = facts.clone();
        credits.credits = 9;
        changed.push(credits);
        let mut tx_sig = facts.clone();
        tx_sig.tx_sig = Some("0xother-sig".into());
        changed.push(tx_sig);
        let mut no_tx_sig = facts.clone();
        no_tx_sig.tx_sig = None;
        changed.push(no_tx_sig);

        for changed_facts in &changed {
            assert_ne!(
                binding,
                spend_settlement_binding(&payer, changed_facts, Some("0xPayee"))
            );
        }
        assert_ne!(
            binding,
            spend_settlement_binding(&agent(2), &facts, Some("0xPayee"))
        );
        assert_ne!(
            binding,
            spend_settlement_binding(&payer, &facts, Some("0xOtherPayee"))
        );
        assert_ne!(binding, spend_settlement_binding(&payer, &facts, None));
    }

    #[tokio::test]
    async fn settlement_records_receipt_audit_and_debits_when_budgeted() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();
        let facts = settle_facts();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;

        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let receipt_id = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("settle");

        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].id, receipt_id);
        assert_eq!(receipts[0].credits_consumed, 8);
        assert_eq!(receipts[0].tx_sig.as_deref(), Some("0xsig"));

        let events = audit.recent(10).await.unwrap();
        assert_eq!(events.len(), 2);
        let settled = events
            .iter()
            .find(|event| matches!(event.kind, AuditKind::SpendSettled { .. }))
            .expect("settlement audit row");
        match &settled.kind {
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
    async fn completed_settlement_dedupes_on_decision_id() {
        // Once the audit row exists, repeats join the recorded receipt without
        // another debit, receipt, or row. Partial-failure recovery is separate.
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();
        let facts = settle_facts();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;

        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };

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
        assert_eq!(audit.recent(10).await.unwrap().len(), 2, "auth plus settle");

        let mut changed_tx = facts.clone();
        changed_tx.tx_sig = Some("0xchanged".into());
        let conflict = record_spend_settlement(&ctx, &enabled(), &payer, &changed_tx)
            .await
            .expect_err("completed retry cannot change its transaction signature");
        assert!(matches!(conflict, SpendAuthzError::Settlement(_)));
        // Debited exactly once: 1000 - 8 = 992, not 976.
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
    }

    #[tokio::test]
    async fn retry_after_audit_failure_reuses_receipt_without_second_debit() {
        let settlement = InMemorySettlement::new();
        let audit = FailOnceAudit::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();
        let facts = settle_facts();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;
        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };

        let first = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect_err("first audit write fails after debit and receipt");
        assert!(matches!(first, SpendAuthzError::Audit(_)));
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
        assert_eq!(settlement.recent(10).await.unwrap().len(), 1);
        assert_eq!(audit.recent(10).await.unwrap().len(), 1);

        let mut conflicts = Vec::new();
        let mut provider = facts.clone();
        provider.provider = "other-provider".into();
        conflicts.push(provider);
        let mut network = facts.clone();
        network.network = "solana:mainnet".into();
        conflicts.push(network);
        let mut asset = facts.clone();
        asset.asset = "other-asset".into();
        conflicts.push(asset);
        let mut amount = facts.clone();
        amount.amount = "79999".into();
        conflicts.push(amount);
        let mut credits = facts.clone();
        credits.credits = 9;
        conflicts.push(credits);
        let mut tx_sig = facts.clone();
        tx_sig.tx_sig = Some("0xother-sig".into());
        conflicts.push(tx_sig);
        for conflicting in &conflicts {
            let conflict = record_spend_settlement(&ctx, &enabled(), &payer, conflicting)
                .await
                .expect_err("same decision cannot change any bound settlement fact");
            assert!(matches!(conflict, SpendAuthzError::Settlement(_)));
        }
        let wrong_payer = agent(2);
        let conflict = record_spend_settlement(&ctx, &enabled(), &wrong_payer, &facts)
            .await
            .expect_err("same decision cannot change payer");
        assert!(matches!(conflict, SpendAuthzError::Settlement(_)));
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);

        let receipt_id = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("retry completes the missing audit row");
        assert_eq!(receipt_id, spend_receipt_id(facts.decision_id, &payer));
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
        assert_eq!(budget.recent_debits(&payer, 10).await.unwrap().len(), 1);
        assert_eq!(settlement.recent(10).await.unwrap().len(), 1);
        assert_eq!(audit.recent(10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn retry_after_receipt_failure_reuses_persisted_debit() {
        let settlement = FailOnceSettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();
        let facts = settle_facts();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;
        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };

        let first = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect_err("first receipt write fails after debit");
        assert!(matches!(first, SpendAuthzError::Settlement(_)));
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
        assert_eq!(budget.recent_debits(&payer, 10).await.unwrap().len(), 1);
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert_eq!(audit.recent(10).await.unwrap().len(), 1);

        let receipt_id = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("retry reuses the persisted debit");
        assert_eq!(receipt_id, spend_receipt_id(facts.decision_id, &payer));
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
        assert_eq!(budget.recent_debits(&payer, 10).await.unwrap().len(), 1);
        assert_eq!(settlement.recent(10).await.unwrap().len(), 1);
        assert_eq!(audit.recent(10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn racing_settlements_share_one_accounting_effect() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        budget.set_capacity(&payer, 1000).await.unwrap();
        let facts = settle_facts();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;
        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let config = enabled();

        let (first, second) = tokio::join!(
            record_spend_settlement(&ctx, &config, &payer, &facts),
            record_spend_settlement(&ctx, &config, &payer, &facts)
        );
        let first = first.unwrap();
        let second = second.unwrap();

        assert_eq!(first, second);
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
        assert_eq!(budget.recent_debits(&payer, 10).await.unwrap().len(), 1);
        assert_eq!(settlement.recent(10).await.unwrap().len(), 1);
        assert_eq!(audit.recent(10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn settlement_decision_cannot_be_squatted_by_another_payer() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        let attacker = agent(2);
        let facts = settle_facts();
        budget.set_capacity(&payer, 1000).await.unwrap();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;
        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };

        let error = record_spend_settlement(&ctx, &enabled(), &attacker, &facts)
            .await
            .expect_err("another payer cannot consume or squat the decision");
        assert!(matches!(error, SpendAuthzError::Settlement(_)));
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(budget.recent_debits_all(10).await.unwrap().is_empty());

        let receipt_id = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("authorized payer can still settle");
        assert_eq!(receipt_id, spend_receipt_id(facts.decision_id, &payer));
        assert_ne!(receipt_id, spend_receipt_id(facts.decision_id, &attacker));
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 992);
    }

    #[tokio::test]
    async fn legacy_authorization_without_payer_binding_is_refused() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        let facts = settle_facts();
        let legacy = AuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: epoch_ms(),
            issuer: issuer.clone(),
            kind: AuditKind::SpendAuthorizationDecided {
                provider: facts.provider.clone(),
                payer: None,
                network: facts.network.clone(),
                asset: facts.asset.clone(),
                amount: facts.amount.clone(),
                credits: facts.credits,
                destination: Some("0xPayee".into()),
                approved: true,
                reason: None,
                decision_id: facts.decision_id,
            },
        };
        let wire = serde_json::to_value(&legacy).unwrap();
        assert!(wire["kind"].get("payer").is_none());
        let legacy = serde_json::from_value(wire).expect("legacy row without payer still decodes");
        audit.record(legacy).await.unwrap();
        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };

        let error = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect_err("legacy decision has no authenticated payer binding");
        assert!(matches!(error, SpendAuthzError::Settlement(_)));
        assert!(settlement.recent(10).await.unwrap().is_empty());
        assert!(budget.recent_debits_all(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn legacy_partial_receipt_without_fact_claim_is_refused() {
        let settlement = InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(1);
        let facts = settle_facts();
        budget.set_capacity(&payer, 1000).await.unwrap();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;
        let receipt_id = spend_receipt_id(facts.decision_id, &payer);
        settlement
            .record(SettlementReceipt {
                id: receipt_id,
                payer: payer.clone(),
                resource: ResourceKind::Tool,
                memory_record_id: None,
                credits_consumed: facts.credits,
                settled_at: epoch_ms(),
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
            .unwrap();
        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };

        let error = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect_err("an unbound legacy partial cannot establish retry facts");
        assert!(matches!(error, SpendAuthzError::Settlement(_)));
        assert_eq!(budget.tokens_remaining(&payer).await.unwrap(), 1000);
        assert!(budget.recent_debits_all(10).await.unwrap().is_empty());
        assert_eq!(audit.recent(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn settlement_records_without_debit_when_no_bucket() {
        let settlement = covenant_settlement::InMemorySettlement::new();
        let audit = InMemoryAuditLog::new();
        let budget = InMemoryLedger::new();
        let issuer = agent(9);
        let payer = agent(2); // never given capacity
        let facts = settle_facts();
        seed_approved_authorization(&audit, &issuer, &payer, &facts).await;

        let ctx = SettleContext {
            settlement: &settlement,
            audit: &audit,
            budget: &budget,
            issuer: &issuer,
        };
        let receipt_id = record_spend_settlement(&ctx, &enabled(), &payer, &facts)
            .await
            .expect("settle");
        assert_eq!(
            settlement.recent(10).await.unwrap().len(),
            1,
            "receipt is recorded even with no budget bucket"
        );
        assert_eq!(audit.recent(10).await.unwrap().len(), 2);
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
