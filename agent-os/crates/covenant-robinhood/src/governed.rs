//! The governed trader: fetch a reference price, run the order through
//! [`TradingPolicy`], and emit a signed [`SignedReceipt`] for every decision,
//! placed or blocked. Human approval and the on-chain anchor are injected
//! traits, so the daemon supplies a Telegram gate and the Metaplex anchor while
//! the crate stays runnable on its own.

use std::sync::Mutex;

use ed25519_dalek::SigningKey;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::policy::{PolicyState, TradingPolicy, Verdict};
use crate::receipt::{Decision, Mode, SignedReceipt, TradeReceipt};
use crate::transport::Transport;
use crate::types::{extract_price, OrderRequest};
use crate::RobinhoodClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Rejected,
    Pending,
}

#[async_trait::async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request(&self, receipt: &TradeReceipt) -> ApprovalOutcome;
}

pub trait Anchor: Send + Sync {
    /// Anchor a signed receipt and return the anchor reference (a transaction
    /// signature or asset id), or `None` for a local-only anchor.
    fn anchor(&self, signed: &SignedReceipt) -> Result<Option<String>>;
}

/// Rejects anything needing approval. The safe default until a human channel is
/// wired, so a large order can't slip through unattended.
pub struct AutoDeny;

#[async_trait::async_trait]
impl ApprovalGate for AutoDeny {
    async fn request(&self, _receipt: &TradeReceipt) -> ApprovalOutcome {
        ApprovalOutcome::Rejected
    }
}

pub struct AutoApprove;

#[async_trait::async_trait]
impl ApprovalGate for AutoApprove {
    async fn request(&self, _receipt: &TradeReceipt) -> ApprovalOutcome {
        ApprovalOutcome::Approved
    }
}

/// Signs the receipt locally and does not touch a chain. The daemon replaces
/// this with the Metaplex `AttestAuditRoot` writer.
pub struct LocalAnchor;
impl Anchor for LocalAnchor {
    fn anchor(&self, _signed: &SignedReceipt) -> Result<Option<String>> {
        Ok(None)
    }
}

pub struct GovernedTrader<T: Transport> {
    client: RobinhoodClient<T>,
    policy: TradingPolicy,
    policy_hash: String,
    attestor: SigningKey,
    agent_id: String,
    gate: Box<dyn ApprovalGate>,
    anchor: Box<dyn Anchor>,
    state: Mutex<PolicyState>,
}

impl<T: Transport> GovernedTrader<T> {
    pub fn new(
        client: RobinhoodClient<T>,
        policy: TradingPolicy,
        attestor: SigningKey,
        agent_id: impl Into<String>,
    ) -> Self {
        let policy_hash = policy.scope_hash();
        Self {
            client,
            policy,
            policy_hash,
            attestor,
            agent_id: agent_id.into(),
            gate: Box::new(AutoDeny),
            anchor: Box::new(LocalAnchor),
            state: Mutex::new(PolicyState::default()),
        }
    }

    pub fn with_gate(mut self, gate: Box<dyn ApprovalGate>) -> Self {
        self.gate = gate;
        self
    }

    pub fn with_anchor(mut self, anchor: Box<dyn Anchor>) -> Self {
        self.anchor = anchor;
        self
    }

    pub fn client(&self) -> &RobinhoodClient<T> {
        &self.client
    }

    pub fn record_realized_pnl(&self, pnl_usd: f64) {
        self.state
            .lock()
            .unwrap()
            .record_realized_pnl(pnl_usd, crate::unix_now());
    }

    pub async fn submit(&self, order: OrderRequest) -> Result<SignedReceipt> {
        let now = crate::unix_now();

        let reference_price = match self.reference_price(&order.symbol).await {
            Ok(p) => p,
            Err(e) => {
                return self.finalize(
                    self.receipt(Decision::Blocked, &order, 0.0)
                        .reason(e.to_string()),
                )
            }
        };

        let verdict = {
            let state = self.state.lock().unwrap();
            self.policy.authorize(&order, reference_price, now, &state)
        };

        match verdict {
            Verdict::Block(reason) => self.finalize(
                self.receipt(Decision::Blocked, &order, reference_price)
                    .reason(reason),
            ),
            Verdict::NeedsApproval(reason) => {
                let pending = self
                    .receipt(Decision::PendingApproval, &order, reference_price)
                    .reason(reason);
                match self.gate.request(&pending).await {
                    ApprovalOutcome::Approved => self.execute(order, reference_price, now).await,
                    ApprovalOutcome::Rejected => self.finalize(
                        self.receipt(Decision::Blocked, &order, reference_price)
                            .reason("approval required and not granted"),
                    ),
                    ApprovalOutcome::Pending => self.finalize(pending),
                }
            }
            Verdict::Allow => self.execute(order, reference_price, now).await,
        }
    }

    async fn reference_price(&self, symbol: &str) -> Result<f64> {
        let quote = self.client.best_bid_ask(&[symbol]).await?;
        extract_price(&quote, symbol)
            .ok_or_else(|| Error::Blocked(format!("no reference price for {symbol}")))
    }

    async fn execute(
        &self,
        order: OrderRequest,
        reference_price: f64,
        now: u64,
    ) -> Result<SignedReceipt> {
        let notional = order.notional(reference_price);

        if self.policy.mode == Mode::DryRun {
            self.state.lock().unwrap().record_order(notional, now);
            return self.finalize(
                self.receipt(Decision::Executed, &order, reference_price)
                    .reason("dry_run: not sent"),
            );
        }

        match self.client.place_order(&order).await {
            Ok(resp) => {
                self.state.lock().unwrap().record_order(notional, now);
                let mut receipt = self.receipt(Decision::Executed, &order, reference_price);
                if let Some(id) = venue_order_id(&resp) {
                    receipt = receipt.venue_order_id(id);
                }
                self.finalize(receipt)
            }
            Err(e) => self.finalize(
                self.receipt(Decision::Blocked, &order, reference_price)
                    .reason(format!("venue error: {e}")),
            ),
        }
    }

    fn receipt(
        &self,
        decision: Decision,
        order: &OrderRequest,
        reference_price: f64,
    ) -> TradeReceipt {
        TradeReceipt::new(
            self.agent_id.clone(),
            self.policy_hash.clone(),
            self.policy.mode,
            decision,
            order.clone(),
        )
        .reference_price(reference_price)
    }

    fn finalize(&self, receipt: TradeReceipt) -> Result<SignedReceipt> {
        let mut signed = receipt.attest(&self.attestor)?;
        let anchor_ref = self.anchor.anchor(&signed)?;
        signed.anchor = anchor_ref;
        Ok(signed)
    }
}

fn venue_order_id(resp: &Value) -> Option<String> {
    for key in ["id", "order_id"] {
        if let Some(id) = resp.get(key).and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Approvals, Caps, Rate, Risk, TradingPolicy, Universe};
    use crate::signer::RobinhoodSigner;
    use crate::transport::MockTransport;
    use crate::types::{OrderType, Side};
    use serde_json::json;

    fn mkpolicy(mode: Mode) -> TradingPolicy {
        TradingPolicy {
            version: 1,
            venue: "robinhood-crypto".into(),
            mode,
            caps: Caps {
                per_order_usd: Some(500.0),
                daily_notional_usd: Some(5_000.0),
            },
            risk: Risk::default(),
            universe: Universe {
                allow: Some(vec!["BTC-USD".into()]),
                deny: None,
                sides: Some(vec![Side::Buy]),
            },
            order_types: vec![OrderType::Market],
            rate: Rate::default(),
            approvals: Approvals {
                require_human_over_usd: Some(400.0),
            },
        }
    }

    fn bbo() -> MockTransport {
        MockTransport::new().json(
            "GET",
            "/best_bid_ask/",
            json!({"results":[{"symbol":"BTC-USD","price":"60000"}]}),
        )
    }

    fn trader(mode: Mode, mock: MockTransport) -> GovernedTrader<MockTransport> {
        let signer = RobinhoodSigner::new("k", SigningKey::from_bytes(&[1u8; 32]));
        let client = RobinhoodClient::new(signer, mock);
        GovernedTrader::new(
            client,
            mkpolicy(mode),
            SigningKey::from_bytes(&[2u8; 32]),
            "agent1",
        )
    }

    fn posted(t: &GovernedTrader<MockTransport>) -> bool {
        t.client
            .transport()
            .recorded()
            .iter()
            .any(|r| r.method == "POST")
    }

    #[tokio::test]
    async fn dry_run_executes_without_sending() {
        let t = trader(Mode::DryRun, bbo());
        let s = t
            .submit(OrderRequest::market("BTC-USD", Side::Buy, 0.001))
            .await
            .unwrap();
        assert_eq!(s.receipt.decision, Decision::Executed);
        assert!(s.verify());
        assert!(!posted(&t));
    }

    #[tokio::test]
    async fn live_places_and_records_venue_id() {
        let mock = bbo().json("POST", "/orders/", json!({"id":"ord_9"}));
        let t = trader(Mode::Live, mock);
        let s = t
            .submit(OrderRequest::market("BTC-USD", Side::Buy, 0.001))
            .await
            .unwrap();
        assert_eq!(s.receipt.decision, Decision::Executed);
        assert_eq!(s.receipt.venue_order_id.as_deref(), Some("ord_9"));
        assert!(s.verify());
        assert!(posted(&t));
    }

    #[tokio::test]
    async fn blocks_over_cap_with_signed_receipt_and_no_send() {
        let t = trader(Mode::Live, bbo());
        let s = t
            .submit(OrderRequest::market("BTC-USD", Side::Buy, 0.01))
            .await
            .unwrap();
        assert_eq!(s.receipt.decision, Decision::Blocked);
        assert!(s.verify());
        assert!(s.receipt.reason.as_ref().unwrap().contains("per-order"));
        assert!(!posted(&t));
    }

    #[tokio::test]
    async fn over_threshold_denied_by_default_gate() {
        let t = trader(Mode::Live, bbo());
        let s = t
            .submit(OrderRequest::market("BTC-USD", Side::Buy, 0.0075))
            .await
            .unwrap();
        assert_eq!(s.receipt.decision, Decision::Blocked);
        assert!(!posted(&t));
    }

    #[tokio::test]
    async fn over_threshold_executes_when_approved() {
        let mock = bbo().json("POST", "/orders/", json!({"id":"ord_ok"}));
        let t = trader(Mode::Live, mock).with_gate(Box::new(AutoApprove));
        let s = t
            .submit(OrderRequest::market("BTC-USD", Side::Buy, 0.0075))
            .await
            .unwrap();
        assert_eq!(s.receipt.decision, Decision::Executed);
        assert_eq!(s.receipt.venue_order_id.as_deref(), Some("ord_ok"));
    }

    #[tokio::test]
    async fn daily_loss_stop_blocks_after_loss() {
        let mut policy = mkpolicy(Mode::Live);
        policy.risk.daily_loss_stop_usd = Some(100.0);
        let signer = RobinhoodSigner::new("k", SigningKey::from_bytes(&[1u8; 32]));
        let client = RobinhoodClient::new(signer, bbo());
        let t = GovernedTrader::new(client, policy, SigningKey::from_bytes(&[2u8; 32]), "agent1");
        t.record_realized_pnl(-150.0);
        let s = t
            .submit(OrderRequest::market("BTC-USD", Side::Buy, 0.001))
            .await
            .unwrap();
        assert_eq!(s.receipt.decision, Decision::Blocked);
        assert!(s.receipt.reason.as_ref().unwrap().contains("daily loss"));
    }
}
