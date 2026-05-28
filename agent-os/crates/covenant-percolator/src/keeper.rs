//! The keeper agent. One tick = read market → decide actions → for
//! each action: capability scope gate → atomic budget debit → execute
//! on-chain → write a settlement receipt that ties the debit (paired
//! by receipt id) to the action. Anything out of policy is dropped
//! before any spend; anything over budget stops the tick.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use covenant_budget::{BudgetError, BudgetLedger};
use covenant_settlement::Settlement;
use covenant_types::{AgentId, ResourceKind, SettlementReceipt};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::capability::KeeperScope;
use crate::client::{ClientError, Execution, PercolatorClient};
use crate::policy::KeeperPolicy;
use crate::state::KeeperAction;
use crate::PercolatorError;

/// One keeper agent bound to one market, one operator policy, and one
/// capability scope. Stateless — call [`tick`] on a schedule.
pub struct KeeperAgent<C: PercolatorClient> {
    pub client: Arc<C>,
    pub settlement: Arc<dyn Settlement>,
    pub budget: Arc<dyn BudgetLedger>,
    pub payer: AgentId,
    pub market_address: String,
    pub scope: KeeperScope,
    pub policy: KeeperPolicy,
    /// Credits charged per executed keeper action. The budget ledger
    /// caps the *total* spend per refill window; this is the per-action
    /// unit charged inside it.
    pub credits_per_action: u64,
}

/// What happened during one tick. Operators read this to triage
/// behavior; tests assert on it.
#[derive(Debug, Default, Clone)]
pub struct TickReport {
    pub decided: usize,
    pub executed: usize,
    pub skipped_capability: usize,
    pub stopped_budget: bool,
    pub errors: Vec<String>,
    /// (action, on-chain execution metadata, paired receipt id) per
    /// executed action — the join key against the budget log and the
    /// settlement log is the receipt id.
    pub executions: Vec<(KeeperAction, Execution, Uuid)>,
}

impl<C: PercolatorClient> KeeperAgent<C> {
    pub async fn tick(&self) -> Result<TickReport, PercolatorError> {
        let market = self
            .client
            .read_market(&self.market_address)
            .await
            .map_err(map_client_err)?;
        let decisions = self.policy.decide(&market);
        let mut report = TickReport {
            decided: decisions.len(),
            ..Default::default()
        };
        let cap = self
            .scope
            .max_actions_per_tick
            .map(|n| n as usize)
            .unwrap_or(usize::MAX);

        for action in decisions.into_iter().take(cap) {
            // 1. Capability gate. Out-of-policy is dropped *before* any
            //    credit is debited or any RPC is made.
            if !self.scope.allows(&self.market_address, &action) {
                debug!(?action, "keeper action rejected by capability scope");
                report.skipped_capability += 1;
                continue;
            }

            let receipt_id = Uuid::new_v4();

            // 2. Atomic budget debit. Exhausted stops the tick (a future
            //    tick after refill will pick up where we left off).
            match self
                .budget
                .try_debit(&self.payer, self.credits_per_action, receipt_id)
                .await
            {
                Ok(()) => {}
                Err(BudgetError::Exhausted { .. }) => {
                    warn!(?action, "keeper budget exhausted; stopping tick");
                    report.stopped_budget = true;
                    break;
                }
                Err(other) => {
                    report.errors.push(format!("budget: {other}"));
                    break;
                }
            }

            // 3. Execute on-chain.
            let execution = match self.client.execute(&self.market_address, &action).await {
                Ok(e) => e,
                Err(e) => {
                    report
                        .errors
                        .push(format!("execute {:?}: {e}", action.action_label()));
                    continue;
                }
            };

            // 4. Verifiable receipt. Ties the budget debit (paired by
            //    receipt id) to a settlement row carrying the slot + tx
            //    sig; the daemon's batching path Merkle-roots the set
            //    and anchors on-chain.
            let receipt = SettlementReceipt {
                id: receipt_id,
                payer: self.payer.clone(),
                resource: ResourceKind::Tool,
                memory_record_id: None,
                credits_consumed: self.credits_per_action,
                settled_at: epoch_ms(),
                chain: Some("solana".into()),
                cluster: None,
                batch_id: None,
                merkle_root: None,
                tx_sig: execution.tx_signature.clone(),
                slot: Some(execution.slot),
                confirmed_at: None,
                onchain_sig: execution.tx_signature.clone(),
            };
            if let Err(e) = self.settlement.record(receipt).await {
                // The on-chain action already happened — surface the
                // accounting gap loudly rather than silently dropping it.
                report.errors.push(format!("settlement: {e}"));
            }

            report.executed += 1;
            report.executions.push((action, execution, receipt_id));
        }
        Ok(report)
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn map_client_err(e: ClientError) -> PercolatorError {
    PercolatorError::Client(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockPercolator;
    use crate::state::{ActionLabel, AssetIndex, AssetLifecycle, AssetState, MarketState};
    use covenant_budget::InMemoryLedger;
    use covenant_settlement::InMemorySettlement;
    use serde_json::json;

    const MARKET: &str = "PercoMarket1111111111111111111111111111111111";
    const PROGRAM: &str = "2SSnp35m7FQ7cRLNKGdW5UzjYFF6RBUNq7d3m5mqNByp";

    fn payer() -> AgentId {
        AgentId::new("keeper@local", [9u8; 32])
    }

    fn market(current_slot: u64, last_crank_slot: u64, assets: Vec<AssetState>) -> MarketState {
        MarketState {
            market_address: MARKET.into(),
            program_id: PROGRAM.into(),
            current_slot,
            last_crank_slot,
            assets,
        }
    }

    fn asset(index: AssetIndex, last_mark_slot: u64) -> AssetState {
        AssetState {
            index,
            label: format!("ASSET{index}"),
            lifecycle: AssetLifecycle::Active,
            last_mark_slot,
            last_mark_e6: 1_000_000,
        }
    }

    fn scope(
        allowed_assets: Option<Vec<AssetIndex>>,
        allowed_actions: Option<Vec<ActionLabel>>,
    ) -> KeeperScope {
        KeeperScope {
            version: 1,
            market: MARKET.into(),
            allowed_assets,
            allowed_actions,
            max_actions_per_tick: None,
        }
    }

    async fn build_agent(
        market_state: MarketState,
        sc: KeeperScope,
        capacity: u64,
        credits_per_action: u64,
    ) -> (
        KeeperAgent<MockPercolator>,
        Arc<MockPercolator>,
        Arc<InMemorySettlement>,
        Arc<InMemoryLedger>,
    ) {
        let client = Arc::new(MockPercolator::new(market_state));
        let settlement = Arc::new(InMemorySettlement::new());
        let budget = Arc::new(InMemoryLedger::new());
        let p = payer();
        budget.set_capacity(&p, capacity).await.unwrap();
        let agent = KeeperAgent {
            client: client.clone(),
            settlement: settlement.clone(),
            budget: budget.clone(),
            payer: p,
            market_address: MARKET.into(),
            scope: sc,
            policy: KeeperPolicy::default(),
            credits_per_action,
        };
        (agent, client, settlement, budget)
    }

    /// The headline path: an asset is stale, the scope permits it, the
    /// budget covers it — the keeper pushes a fresh mark and the
    /// settlement row carries the same id as the paired budget debit.
    #[tokio::test]
    async fn freshens_stale_active_asset_and_records_receipt() {
        let (agent, client, settlement, budget) = build_agent(
            market(2_000, 1_900, vec![asset(7, 1_000)]),
            scope(
                Some(vec![7]),
                Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
            ),
            1_000,
            5,
        )
        .await;

        let report = agent.tick().await.unwrap();
        assert!(report.executed >= 1, "expected at least the push_mark");
        assert_eq!(report.skipped_capability, 0);
        let executed = client.executed();
        assert!(matches!(
            executed[0],
            KeeperAction::PushHyperpMark { asset_index: 7, .. }
        ));
        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), report.executed);
        assert_eq!(receipts[0].resource, ResourceKind::Tool);
        assert!(receipts[0].slot.is_some());
        // The settlement row's id matches the budget-debit's paired id.
        assert_eq!(receipts[0].id, report.executions[0].2);
        let remaining = budget.tokens_remaining(&payer()).await.unwrap();
        assert_eq!(remaining, 1_000 - 5 * report.executed as u64);
    }

    /// Out-of-scope asset must be dropped *before* any spend or RPC.
    #[tokio::test]
    async fn capability_blocks_unauthorized_asset_before_any_spend() {
        let (agent, client, _settlement, budget) = build_agent(
            market(2_000, 1_900, vec![asset(3, 1_000)]),
            scope(Some(vec![7]), Some(vec![ActionLabel::PushMark])),
            1_000,
            5,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 0);
        assert!(report.skipped_capability >= 1);
        assert!(client.executed().is_empty(), "no on-chain action allowed");
        assert_eq!(budget.tokens_remaining(&payer()).await.unwrap(), 1_000);
    }

    /// Budget exhaustion stops the tick mid-loop; subsequent actions
    /// wait for refill rather than running unfunded.
    #[tokio::test]
    async fn budget_exhaustion_stops_the_tick() {
        let (agent, client, _settlement, budget) = build_agent(
            market(
                5_000,
                4_900,
                vec![asset(0, 1_000), asset(1, 1_000), asset(2, 1_000)],
            ),
            scope(
                Some(vec![0, 1, 2]),
                Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
            ),
            5, // one action's worth
            5,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 1);
        assert!(report.stopped_budget);
        assert_eq!(client.executed().len(), 1);
        assert_eq!(budget.tokens_remaining(&payer()).await.unwrap(), 0);
    }

    /// Even with no stale assets, an overdue crank still fires under
    /// the right scope — the keeper handles liveness, not just
    /// freshness.
    #[tokio::test]
    async fn crank_runs_when_interval_elapsed() {
        let interval = KeeperPolicy::default().crank_interval_slots;
        let (agent, client, _settlement, _budget) = build_agent(
            market(
                10_000,
                10_000 - interval - 1,
                vec![asset(0, 9_950)], // not stale
            ),
            scope(Some(vec![0]), Some(vec![ActionLabel::Crank])),
            1_000,
            5,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 1);
        assert!(matches!(client.executed()[0], KeeperAction::Crank));
    }

    /// Per-tick cap bounds how many actions a single pass can spend,
    /// even when the policy decides more.
    #[tokio::test]
    async fn max_actions_per_tick_bounds_the_loop() {
        let assets: Vec<_> = (0..5).map(|i| asset(i, 1_000)).collect();
        let mut sc = scope(
            Some((0..5).collect()),
            Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
        );
        sc.max_actions_per_tick = Some(2);
        let (agent, client, _settlement, _budget) = build_agent(
            market(5_000, 4_950, assets),
            sc,
            10_000,
            1,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 2);
        assert_eq!(client.executed().len(), 2);
    }

    /// Non-Active assets (Disabled/PendingActivation/DrainOnly/Retired/
    /// Recovery) are not freshened by the normal `push_mark` path — that
    /// path only acts on Active assets. The recovery scope is a
    /// separate verb on the recovery lifecycle, gated independently.
    #[tokio::test]
    async fn non_active_assets_are_skipped() {
        let mut paused = asset(0, 1_000);
        paused.lifecycle = AssetLifecycle::Recovery;
        let (agent, client, _settlement, _budget) = build_agent(
            market(5_000, 5_000, vec![paused]), // crank not overdue either
            scope(Some(vec![0]), Some(vec![ActionLabel::PushMark, ActionLabel::Crank])),
            1_000,
            5,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 0);
        assert!(client.executed().is_empty());
    }

    #[test]
    fn keeper_scope_parse_pins_version_and_market_and_serde_shape() {
        let v = json!({
            "version": 1,
            "market": MARKET,
            "allowed_assets": [0, 3],
            "allowed_actions": ["push_mark", "crank"],
        });
        let s = KeeperScope::parse(&v).unwrap();
        assert_eq!(s.market, MARKET);
        assert_eq!(s.allowed_assets.as_deref(), Some(&[0u16, 3][..]));
        assert!(s
            .allowed_actions
            .as_ref()
            .unwrap()
            .contains(&ActionLabel::PushMark));

        let unsupported = json!({ "version": 2, "market": MARKET });
        assert!(KeeperScope::parse(&unsupported).is_err());
    }
}
