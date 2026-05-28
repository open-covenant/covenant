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
use crate::coordination::{self, KeeperId};
use crate::policy::{KeeperPolicy, RecoveryPolicy};
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
    /// Optional recovery policy. When `Some`, the keeper calls
    /// `list_portfolios` each tick and decides recovery actions from
    /// each portfolio's engine-issued `HealthCertV16`. When `None`,
    /// the keeper restricts itself to mark/crank actions and does not
    /// even query portfolios.
    pub recovery_policy: Option<RecoveryPolicy>,
    /// Other accountable keepers this agent knows about. Empty = solo
    /// operation, coordination is skipped entirely. Non-empty enables
    /// deterministic leader election per `(action_key, slot_window)`:
    /// every keeper in the network computes the same priority and
    /// only the leader submits, eliminating the "all N keepers race
    /// to crank the same state" waste without any inter-keeper
    /// communication.
    pub peers: Vec<KeeperId>,
    /// Slot window over which leadership rotates. `50` is a
    /// reasonable default at 400ms slots (~20s windows). `0` means
    /// "one window forever" — useful in tests; in production set a
    /// positive value so the leader rotates and no keeper is
    /// permanently in front of the queue.
    pub coordination_window_slots: u64,
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
    /// Actions the agent declined to submit because deterministic
    /// leader election put another peer in front. Non-leaders never
    /// block on the leader (no hold-and-wait); they simply skip.
    pub coordination_deferred: usize,
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
        let mut decisions = self.policy.decide(&market);
        if let Some(rp) = self.recovery_policy {
            let portfolios = self
                .client
                .list_portfolios(&self.market_address)
                .await
                .map_err(map_client_err)?;
            decisions.extend(rp.decide(&portfolios));
        }
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

            // 2. Coordination gate. When peers are known, only the
            //    deterministic leader for `(action_key, window)`
            //    submits; non-leaders skip cleanly with no hold-and-
            //    wait. Empty peers list = solo operation, always lead.
            //    (Comment numbering 1→2→3→4→5 matches the gate order.)
            if !self.peers.is_empty()
                && !coordination::should_lead(
                    &self.payer.pubkey,
                    &action,
                    market.current_slot,
                    self.coordination_window_slots,
                    &self.peers,
                )
            {
                debug!(?action, "keeper deferred to peer leader");
                report.coordination_deferred += 1;
                continue;
            }

            let receipt_id = Uuid::new_v4();

            // 3. Atomic budget debit. Exhausted stops the tick (a future
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

            // 4. Execute on-chain.
            let execution = match self.client.execute(&self.market_address, &action).await {
                Ok(e) => e,
                Err(e) => {
                    report
                        .errors
                        .push(format!("execute {:?}: {e}", action.action_label()));
                    continue;
                }
            };

            // 5. Verifiable receipt. Ties the budget debit (paired by
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

/// Current wall-clock in epoch-ms, or `u64::MAX` on a clock that's
/// gone backward. We don't silently emit `settled_at: 0` because a
/// hard-zero timestamp on a real receipt is indistinguishable from a
/// missing timestamp and breaks downstream batching; an extreme
/// sentinel is loud enough that the reconciler flags it.
fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(u64::MAX)
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
    const PROGRAM: &str = crate::MAINNET_PROGRAM_ID;

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
            peers: Vec::new(),
            coordination_window_slots: 0,
            recovery_policy: None,
            credits_per_action,
        };
        (agent, client, settlement, budget)
    }

    // Builder variant that wires a `RecoveryPolicy` and seeds the mock
    // with portfolios. Used by the recovery tests below.
    async fn build_recovery_agent(
        market_state: MarketState,
        portfolios: Vec<crate::state::PortfolioSnapshot>,
        sc: KeeperScope,
        recovery_policy: Option<RecoveryPolicy>,
        capacity: u64,
        credits_per_action: u64,
    ) -> (
        KeeperAgent<MockPercolator>,
        Arc<MockPercolator>,
        Arc<InMemorySettlement>,
        Arc<InMemoryLedger>,
    ) {
        let client = Arc::new(MockPercolator::new(market_state).with_portfolios(portfolios));
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
            peers: Vec::new(),
            coordination_window_slots: 0,
            recovery_policy,
            credits_per_action,
        };
        (agent, client, settlement, budget)
    }

    fn distressed_portfolio(asset_index: AssetIndex, deficit: u128) -> crate::state::PortfolioSnapshot {
        use crate::risk::HealthCertV16;
        crate::state::PortfolioSnapshot {
            portfolio_address: format!("Port{asset_index}"),
            health_cert: HealthCertV16 {
                valid: true,
                certified_liq_deficit: deficit,
                ..HealthCertV16::default()
            },
            asset_in_distress: Some(asset_index),
        }
    }

    fn fresh_market_no_actions() -> MarketState {
        // current=2_000, crank at 1_950 (within default 300-interval),
        // one asset freshened at 1_950 (within default 150-staleness)
        // — `KeeperPolicy::decide` returns no mark/crank actions, so
        // any executed action is necessarily a recovery action.
        market(2_000, 1_950, vec![asset(3, 1_950)])
    }

    /// Recovery path engages `HealthCertV16` directly: a portfolio
    /// with `valid=true` and a non-zero `certified_liq_deficit` emits
    /// exactly one `ForfeitRecoveryLeg` carrying the engine-bounded
    /// `b_delta_budget`.
    #[tokio::test]
    async fn recovery_triggers_on_liq_deficit_with_valid_cert() {
        let (agent, client, settlement, _budget) = build_recovery_agent(
            fresh_market_no_actions(),
            vec![distressed_portfolio(3, 1_000_000)],
            KeeperScope {
                version: 1,
                market: MARKET.into(),
                allowed_assets: Some(vec![3]),
                allowed_actions: Some(vec![ActionLabel::Recover]),
                max_actions_per_tick: None,
            },
            Some(RecoveryPolicy::default()),
            1_000,
            10,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 1);
        let executed = client.executed();
        match &executed[0] {
            KeeperAction::RecoveryForfeitLeg { asset_index, b_delta_budget } => {
                assert_eq!(*asset_index, 3);
                assert_eq!(*b_delta_budget, 1_000_000);
            }
            other => panic!("expected RecoveryForfeitLeg, got {other:?}"),
        }
        // The receipt is paired with the debit by id.
        let receipts = settlement.recent(10).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].id, report.executions[0].2);
    }

    /// Fail-closed (spec §16): a stale / invalid cert produces no
    /// recovery action — the keeper trusts the engine's freshness gate
    /// and does not second-guess.
    #[tokio::test]
    async fn recovery_fails_closed_on_stale_cert() {
        use crate::risk::HealthCertV16;
        let stale = crate::state::PortfolioSnapshot {
            portfolio_address: "Port1".into(),
            health_cert: HealthCertV16 {
                valid: false,                 // stale / invalid
                certified_liq_deficit: 5_000, // tempting, but ignored
                ..HealthCertV16::default()
            },
            asset_in_distress: Some(3),
        };
        let (agent, client, _settlement, _budget) = build_recovery_agent(
            fresh_market_no_actions(),
            vec![stale],
            KeeperScope {
                version: 1,
                market: MARKET.into(),
                allowed_assets: Some(vec![3]),
                allowed_actions: Some(vec![ActionLabel::Recover]),
                max_actions_per_tick: None,
            },
            Some(RecoveryPolicy::default()),
            1_000,
            10,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 0);
        assert!(client.executed().is_empty());
    }

    /// Healthy portfolio: `certified_liq_deficit==0` → no action.
    #[tokio::test]
    async fn recovery_skips_when_no_deficit() {
        use crate::risk::HealthCertV16;
        let healthy = crate::state::PortfolioSnapshot {
            portfolio_address: "Port1".into(),
            health_cert: HealthCertV16 {
                valid: true,
                certified_liq_deficit: 0,
                ..HealthCertV16::default()
            },
            asset_in_distress: Some(3),
        };
        let (agent, client, _settlement, _budget) = build_recovery_agent(
            fresh_market_no_actions(),
            vec![healthy],
            KeeperScope {
                version: 1,
                market: MARKET.into(),
                allowed_assets: Some(vec![3]),
                allowed_actions: Some(vec![ActionLabel::Recover]),
                max_actions_per_tick: None,
            },
            Some(RecoveryPolicy::default()),
            1_000,
            10,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 0);
        assert!(client.executed().is_empty());
    }

    /// Three independent keepers see the same stale asset in the same
    /// slot window. With coordination wired, *exactly one* (the
    /// deterministic leader) executes the action; the other two
    /// report `coordination_deferred = 1` and spend nothing. No
    /// hold-and-wait, no inter-keeper communication required — the
    /// leader election is a pure function of public state + peer ids.
    #[tokio::test]
    async fn three_keepers_only_one_executes_others_defer() {
        use crate::coordination::KeeperId;
        let kids: [KeeperId; 3] = [[1u8; 32], [2u8; 32], [3u8; 32]];
        let peers: Vec<KeeperId> = kids.to_vec();

        let mut executed = Vec::new();
        let mut deferred = Vec::new();

        for &kid_bytes in &kids {
            let client = Arc::new(MockPercolator::new(market(
                2_000,
                1_900,
                vec![asset(7, 1_000)],
            )));
            let settlement = Arc::new(InMemorySettlement::new());
            let budget = Arc::new(InMemoryLedger::new());
            let p = AgentId::new("keeper@local", kid_bytes);
            budget.set_capacity(&p, 1_000).await.unwrap();
            let agent = KeeperAgent {
                client: client.clone(),
                settlement: settlement.clone(),
                budget: budget.clone(),
                payer: p,
                market_address: MARKET.into(),
                scope: KeeperScope {
                    version: 1,
                    market: MARKET.into(),
                    allowed_assets: Some(vec![7]),
                    allowed_actions: Some(vec![ActionLabel::PushMark, ActionLabel::Crank]),
                    max_actions_per_tick: None,
                },
                policy: KeeperPolicy::default(),
                peers: peers.clone(),
                coordination_window_slots: 50,
                recovery_policy: None,
                credits_per_action: 5,
            };
            let report = agent.tick().await.unwrap();
            executed.push(report.executed);
            deferred.push(report.coordination_deferred);
        }

        assert_eq!(
            executed.iter().filter(|&&n| n > 0).count(),
            1,
            "exactly one keeper executes (executed counts: {executed:?})"
        );
        assert_eq!(
            deferred.iter().filter(|&&n| n > 0).count(),
            2,
            "the other two defer (deferred counts: {deferred:?})"
        );
    }

    /// Capability is independent of policy: a recovery-needing
    /// portfolio with a scope that does NOT grant the `recover` verb
    /// → no execution, scope rejection counted.
    #[tokio::test]
    async fn recovery_blocked_when_scope_lacks_recover_verb() {
        let (agent, client, _settlement, _budget) = build_recovery_agent(
            fresh_market_no_actions(),
            vec![distressed_portfolio(3, 1_000_000)],
            KeeperScope {
                version: 1,
                market: MARKET.into(),
                allowed_assets: Some(vec![3]),
                allowed_actions: Some(vec![ActionLabel::PushMark, ActionLabel::Crank]), // no Recover
                max_actions_per_tick: None,
            },
            Some(RecoveryPolicy::default()),
            1_000,
            10,
        )
        .await;
        let report = agent.tick().await.unwrap();
        assert_eq!(report.executed, 0);
        assert!(report.skipped_capability >= 1);
        assert!(client.executed().is_empty());
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
            KeeperAction::PushAuthMark { asset_index: 7, .. }
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
        assert!(matches!(
            client.executed()[0],
            KeeperAction::CrankAsset { asset_index: 0 }
        ));
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
